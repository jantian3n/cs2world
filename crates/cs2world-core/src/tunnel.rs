mod session;
mod socket;
mod stream;

pub use stream::MuxStream;

use crate::config::{Node, ServerConfig};
use cs2world_proto::{
    a2s, gns,
    shaper::{Direction, Shaper},
};
use rand_core::{OsRng, RngCore};
use session::{Endpoint, Params, Peer, Session, Shaping};
use socket::{local_hour, now_ms, transient, udp, unix};
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::net::{lookup_host, UdpSocket};
use tokio::sync::{mpsc, watch, Mutex};
use tokio::time;
use tokio_util::sync::CancellationToken;

fn bad(e: impl std::fmt::Debug) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("CS2World protocol: {e:?}"),
    )
}
fn shaping(config: cs2world_proto::shaper::Config, dir: Direction) -> Shaping {
    Arc::new(StdMutex::new(Shaper::new_for_direction(
        config,
        now_ms(),
        dir,
    )))
}

#[derive(Default)]
struct WireMeter {
    tx: std::sync::atomic::AtomicU64,
    rx: std::sync::atomic::AtomicU64,
}
impl WireMeter {
    async fn send(&self, socket: &UdpSocket, bytes: &[u8], addr: SocketAddr) -> io::Result<()> {
        let n = socket.send_to(bytes, addr).await?;
        self.tx
            .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
    fn received(&self, n: usize) {
        self.rx
            .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
    }
    fn totals(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        (self.tx.load(Relaxed), self.rx.load(Relaxed))
    }
}
pub struct TunnelClient {
    meter: Arc<WireMeter>,
    node: Node,
    last_peer: StdMutex<Option<SocketAddr>>,
    active: Mutex<Option<Endpoint>>,
    stop: CancellationToken,
}
impl TunnelClient {
    pub fn new(node: Node) -> Arc<Self> {
        Arc::new(Self {
            node,
            last_peer: StdMutex::new(None),
            meter: Arc::default(),
            active: Mutex::new(None),
            stop: CancellationToken::new(),
        })
    }

    async fn dial(&self, addr: SocketAddr) -> io::Result<Endpoint> {
        let bind = if addr.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let sock = Arc::new(udp(bind.parse().unwrap())?);
        crate::platform_socket::bind_udp_interface(&sock)?;
        let (req, hs) = gns::build_request(&self.node.psk, unix(), &mut OsRng).map_err(bad)?;
        let mut buf = [0u8; 2048];
        let mut rejected = false;
        for _ in 0..3 {
            self.meter.send(&sock, &req, addr).await?;
            let packet = time::timeout(Duration::from_millis(1500), sock.recv_from(&mut buf)).await;
            match packet {
                Ok(Ok((n, peer))) if peer == addr => {
                    self.meter.received(n);
                    if let Ok(codec) = hs.complete(&buf[..n]) {
                        return Session::client(
                            Params {
                                socket: sock,
                                meter: self.meter.clone(),
                                addr,
                                codec,
                                kcp: self.node.kcp.clone(),
                                shaping: shaping(self.node.shaper.clone(), Direction::Uplink),
                                stop: self.stop.child_token(),
                            },
                            OsRng.next_u32(),
                        );
                    }
                    rejected |= n > 0
                        && (buf[0] == gns::CONNECTION_CLOSED
                            || buf[0] == gns::VALVE_CONNECTION_CLOSED);
                }
                Ok(Err(e)) if !transient(&e) => return Err(e),
                _ => {}
            }
        }
        Err(if rejected {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "peer returned an unauthenticated rejection; verify node, PSK and clock",
            )
        } else {
            io::Error::new(io::ErrorKind::TimedOut, "CS2World handshake timed out")
        })
    }
    async fn ensure(&self, slot: &mut Option<Endpoint>) -> io::Result<()> {
        if slot
            .as_ref()
            .is_some_and(|s| !s.stop.is_cancelled() && !s.commands.is_closed())
        {
            return Ok(());
        }
        *slot = None;
        let mut last = io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "node address did not resolve",
        );
        // Reconnecting while a full-tunnel route is active must not depend on a
        // DNS query through the same disconnected tunnel.
        let previous = *self.last_peer.lock().unwrap();
        if let Some(peer) = previous {
            match self.dial(peer).await {
                Ok(s) => {
                    *slot = Some(s);
                    return Ok(());
                }
                Err(e) => last = e,
            }
        }
        for addr in lookup_host((self.node.server.as_str(), self.node.server_port)).await? {
            if Some(addr) == previous {
                continue;
            }
            match self.dial(addr).await {
                Ok(s) => {
                    *self.last_peer.lock().unwrap() = Some(addr);
                    *slot = Some(s);
                    return Ok(());
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }
    pub fn wire_totals(&self) -> (u64, u64) {
        self.meter.totals()
    }
    pub async fn connect(&self) -> io::Result<()> {
        self.ensure(&mut *self.active.lock().await).await
    }
    pub async fn open(&self) -> io::Result<MuxStream> {
        let mut slot = self.active.lock().await;
        self.ensure(&mut slot).await?;
        let session = slot.as_ref().expect("initialized session");
        session.open().await
    }
}
impl Drop for TunnelClient {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

pub struct TunnelListener {
    meter: Arc<WireMeter>,
    addr: SocketAddr,
    update: watch::Sender<ServerConfig>,
    incoming: mpsc::Receiver<MuxStream>,
    stop: CancellationToken,
}
impl TunnelListener {
    pub async fn bind(config: &ServerConfig) -> io::Result<Self> {
        let socket = Arc::new(udp(config.listen)?);
        let addr = socket.local_addr()?;
        let (send, incoming) = mpsc::channel(256);
        let (update, watch) = watch::channel(config.clone());
        let stop = CancellationToken::new();
        let meter = Arc::<WireMeter>::default();
        tokio::spawn(serve(socket, watch, send, stop.clone(), meter.clone()));
        Ok(Self {
            meter,
            addr,
            incoming,
            update,
            stop,
        })
    }
    pub fn wire_totals(&self) -> (u64, u64) {
        self.meter.totals()
    }
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.addr)
    }
    pub fn reload(&mut self, config: &ServerConfig) -> io::Result<()> {
        self.update
            .send(config.clone())
            .map_err(|_| session::disconnected())
    }
    pub async fn accept(&mut self) -> io::Result<MuxStream> {
        self.incoming.recv().await.ok_or_else(session::disconnected)
    }
}
impl Drop for TunnelListener {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

async fn serve(
    socket: Arc<UdpSocket>,
    mut config_rx: watch::Receiver<ServerConfig>,
    send: mpsc::Sender<MuxStream>,
    stop: CancellationToken,
    meter: Arc<WireMeter>,
) {
    let mut config = config_rx.borrow_and_update().clone();
    let mut responder = a2s::Responder::new(config.a2s.clone(), now_ms(), local_hour(), &mut OsRng);
    let mut peers: HashMap<u32, Peer> = HashMap::new();
    let mut cleanup = time::interval(Duration::from_secs(60));
    let mut buf = [0u8; 2048];
    loop {
        let (n, addr) = tokio::select! {
            _=stop.cancelled()=>break,
            changed=config_rx.changed()=>{
                if changed.is_err(){break;}
                config=config_rx.borrow_and_update().clone();
                responder=a2s::Responder::new(config.a2s.clone(),now_ms(),local_hour(),&mut OsRng);
                continue;
            }
            _=cleanup.tick()=>{peers.retain(|_,p|!p.stop.is_cancelled());continue;}
            read=socket.recv_from(&mut buf)=>match read{
                Ok(val)=>val,Err(e) if transient(&e)=>continue,Err(_)=>break
            }
        };
        meter.received(n);
        let pkt = &buf[..n];
        if a2s::is_query(pkt) {
            if let Some(reply) = responder.handle(pkt, now_ms(), local_hour(), &mut OsRng) {
                let _ = meter.send(&socket, &reply, addr).await;
            }
        } else if pkt.first() == Some(&1) && n >= 61 {
            if peers.len() >= 4096 {
                peers.retain(|_, p| !p.stop.is_cancelled());
            }
            if peers.len() < 4096 {
                if let Ok((reply, codec)) =
                    gns::accept_request(pkt, &config.psk, unix(), &mut OsRng)
                {
                    let id = codec.conn_id();
                    if peers.contains_key(&id) {
                        continue;
                    }
                    let peer = Session::server(
                        Params {
                            meter: meter.clone(),
                            socket: socket.clone(),
                            addr,
                            codec,
                            kcp: config.kcp.clone(),
                            shaping: shaping(config.shaper.clone(), Direction::Downlink),
                            stop: stop.child_token(),
                        },
                        send.clone(),
                    );
                    peers.insert(id, peer);
                    let _ = meter.send(&socket, &reply, addr).await;
                    continue;
                }
            }
            let _ = meter.send(&socket, &gns::reject_for_packet(pkt), addr).await;
        } else if let Some(peer) = gns::extract_conn_id(pkt).and_then(|id| peers.get(&id)) {
            // A full session queue acts like a dropped UDP datagram; KCP retransmits.
            let _ = peer.wire.try_send((pkt.to_vec(), addr));
        } else {
            let _ = meter.send(&socket, &gns::reject_for_packet(pkt), addr).await;
        }
    }
    stop.cancel();
}
