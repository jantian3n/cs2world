use crate::config::{ClientConfig, Config, ServerConfig};
use crate::mixed;
use crate::platform_socket::{bind_udp, connect_tcp};
use crate::route::{Action, Network, RouteTable, Target};
use crate::tunnel::{MuxStream, TunnelClient, TunnelListener};
use cs2world_proto::mux::{decode_addr, encode_addr, UOT_LEGACY_MAGIC, UOT_MAGIC};
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicU64, Ordering::Relaxed},
    Arc,
};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::{JoinHandle, JoinSet};

pub(crate) fn invalid(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}
pub(crate) fn denied() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "destination blocked")
}
pub(crate) fn endpoint(addr: SocketAddr) -> Target {
    Target {
        host: addr.ip().to_string(),
        port: addr.port(),
    }
}
pub(crate) type Packet = (Target, Vec<u8>);
pub(crate) type Outbound = Box<dyn Duplex>;
pub(crate) trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}

async fn connect_any(addrs: impl IntoIterator<Item = SocketAddr>) -> io::Result<TcpStream> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no addresses for destination");
    for addr in addrs {
        match connect_tcp(addr).await {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(last)
}

pub(crate) async fn read_target<R: AsyncRead + Unpin>(r: &mut R, uot: bool) -> io::Result<Target> {
    let t = r.read_u8().await?;
    let mut buf = vec![t];
    match (uot, t) {
        (false, 1) | (true, 0) => buf.resize(7, 0),
        (false, 4) | (true, 1) => buf.resize(19, 0),
        (false, 3) | (true, 2) => {
            let n = r.read_u8().await? as usize;
            if n == 0 {
                return Err(invalid("empty destination hostname"));
            }
            buf.push(n as u8);
            buf.resize(4 + n, 0);
            r.read_exact(&mut buf[2..]).await?;
            return decode_addr(&buf, uot)
                .ok()
                .flatten()
                .map(|(addr, _)| addr)
                .ok_or_else(|| invalid("invalid hostname"));
        }
        _ => return Err(invalid("unsupported address family")),
    }
    r.read_exact(&mut buf[1..]).await?;
    decode_addr(&buf, uot)
        .ok()
        .flatten()
        .map(|(addr, _)| addr)
        .ok_or_else(|| invalid("invalid address"))
}
pub(crate) async fn addr_write<W: AsyncWrite + Unpin>(
    w: &mut W,
    t: &Target,
    uot: bool,
) -> io::Result<()> {
    let b = encode_addr(t, uot).map_err(|_| invalid("invalid address"))?;
    w.write_all(&b).await
}
async fn read_packet<R: AsyncRead + Unpin>(
    r: &mut R,
    fixed: Option<&Target>,
) -> io::Result<Packet> {
    let target = match fixed {
        Some(a) => a.clone(),
        None => read_target(r, true).await?,
    };
    let len = r.read_u16().await?;
    let mut bytes = vec![0; usize::from(len)];
    r.read_exact(&mut bytes).await?;
    Ok((target, bytes))
}
async fn write_packet<W: AsyncWrite + Unpin>(
    w: &mut W,
    fixed: bool,
    t: &Target,
    bytes: &[u8],
) -> io::Result<()> {
    let len = u16::try_from(bytes.len()).map_err(|_| invalid("datagram exceeds UoT length"))?;
    if !fixed {
        addr_write(w, t, true).await?;
    }
    w.write_u16(len).await?;
    w.write_all(bytes).await
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct Stats {
    pub up_total: u64,
    pub down_total: u64,
    pub up_bps: u64,
    pub down_bps: u64,
    pub active_tcp: u64,
    pub active_udp: u64,
    pub wire_up: u64,
    pub wire_down: u64,
}
#[derive(Default)]
pub(crate) struct Counters {
    pub up: AtomicU64,
    pub down: AtomicU64,
    pub tcp: AtomicU64,
    pub udp: AtomicU64,
    up_rate: AtomicU64,
    down_rate: AtomicU64,
    wire_up: AtomicU64,
    wire_down: AtomicU64,
}
impl Counters {
    fn stats(&self) -> Stats {
        Stats {
            up_total: self.up.load(Relaxed),
            down_total: self.down.load(Relaxed),
            active_tcp: self.tcp.load(Relaxed),
            active_udp: self.udp.load(Relaxed),
            up_bps: self.up_rate.load(Relaxed),
            down_bps: self.down_rate.load(Relaxed),
            wire_up: self.wire_up.load(Relaxed),
            wire_down: self.wire_down.load(Relaxed),
        }
    }
    fn wire(&self, counts: (u64, u64)) {
        self.wire_up.store(counts.0, Relaxed);
        self.wire_down.store(counts.1, Relaxed);
    }
    pub(crate) fn guard(self: &Arc<Self>, udp: bool) -> Flow {
        let c = if udp { &self.udp } else { &self.tcp };
        c.fetch_add(1, Relaxed);
        Flow(self.clone(), udp)
    }
}
pub(crate) struct Flow(Arc<Counters>, bool);
impl Drop for Flow {
    fn drop(&mut self) {
        let c = if self.1 { &self.0.udp } else { &self.0.tcp };
        c.fetch_sub(1, Relaxed);
    }
}

pub(crate) struct Meter<S> {
    pub stream: S,
    pub counts: Arc<Counters>,
    pub from_client: bool,
}
impl<S: AsyncRead + Unpin> AsyncRead for Meter<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let res = Pin::new(&mut self.stream).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = res {
            let c = if self.from_client {
                &self.counts.up
            } else {
                &self.counts.down
            };
            c.fetch_add((buf.filled().len() - before) as u64, Relaxed);
        }
        res
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for Meter<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        b: &[u8],
    ) -> Poll<io::Result<usize>> {
        let res = Pin::new(&mut self.stream).poll_write(cx, b);
        if let Poll::Ready(Ok(n)) = res {
            let c = if self.from_client {
                &self.counts.down
            } else {
                &self.counts.up
            };
            c.fetch_add(n as u64, Relaxed);
        }
        res
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

pub(crate) struct ClientRuntime {
    pub config: ClientConfig,
    pub tunnel: Option<Arc<TunnelClient>>,
    pub counts: Arc<Counters>,
    pub stop: watch::Receiver<bool>,
    pub logs: broadcast::Sender<String>,
    pub generation: tokio_util::sync::CancellationToken,
}
impl ClientRuntime {
    pub async fn tcp(&self, t: &Target, name: Option<&str>) -> io::Result<Outbound> {
        match self.config.routes.action_with_name(t, Network::Tcp, name) {
            Action::Block => Err(denied()),
            Action::Direct => Ok(Box::new(
                connect_any(tokio::net::lookup_host((t.host.as_str(), t.port)).await?).await?,
            )),
            Action::Proxy => {
                let mut s = self.proxy().await?;
                addr_write(&mut s, t, false).await?;
                Ok(Box::new(s))
            }
        }
    }
    async fn proxy(&self) -> io::Result<MuxStream> {
        let tunnel = self
            .tunnel
            .as_ref()
            .ok_or_else(|| invalid("missing tunnel node"))?;
        let s = tunnel.open().await?;
        let _ = self.logs.send("Tunnel authenticated".to_owned());
        Ok(s)
    }
    pub async fn udp_proxy(
        &self,
        first: &Target,
        mut outbound: mpsc::Receiver<Packet>,
        responses: mpsc::Sender<Packet>,
    ) -> io::Result<()> {
        let mut stream = self.proxy().await?;
        addr_write(
            &mut stream,
            &Target {
                host: UOT_MAGIC.into(),
                port: 0,
            },
            false,
        )
        .await?;
        stream.write_u8(0).await?;
        addr_write(&mut stream, first, false).await?;
        let (mut r, mut w) = tokio::io::split(stream);
        let send = async {
            while let Some((target, bytes)) = outbound.recv().await {
                write_packet(&mut w, false, &target, &bytes).await?;
            }
            Ok(())
        };
        let read = async {
            loop {
                let pkt = read_packet(&mut r, None).await?;
                if responses.send(pkt).await.is_err() {
                    return Ok(());
                }
            }
        };
        tokio::select! {r=send=>r,r=read=>r}
    }
}

/// A pair of family-specific sockets retains sources for replies from multiple targets.
/// Only endpoints contacted by this association can contribute a response.
pub(crate) async fn udp_direct(
    mut out: mpsc::Receiver<Packet>,
    responses: mpsc::Sender<Packet>,
    rules: Option<RouteTable>,
) -> io::Result<()> {
    let v4 = bind_udp(SocketAddr::from(([0, 0, 0, 0], 0))).await?;
    let v6 = bind_udp(SocketAddr::from(([0u16; 8], 0))).await.ok();
    let mut peers = HashMap::<SocketAddr, Instant>::new();
    let mut b4 = vec![0; 65535];
    let mut b6 = vec![0; 65535];
    loop {
        let recv = tokio::select! {
            msg=out.recv()=>{
                let Some((target,bytes))=msg else{return Ok(())};
                let addrs=match &rules{
                    Some(r)=>r.server_endpoints(&target,Network::Udp).await,
                    None=>tokio::net::lookup_host((target.host.as_str(),target.port)).await.map(|a|a.collect()),
                };
                let Ok(addrs)=addrs else{continue};
                let now=Instant::now();
                peers.retain(|_,last| now.duration_since(*last)<Duration::from_secs(180));
                for addr in addrs{
                    if !peers.contains_key(&addr) && peers.len()>=1024 {continue;}
                    let socket=if addr.is_ipv4(){&v4}else{let Some(s)=&v6 else{continue};s};
                    if socket.send_to(&bytes,addr).await.is_ok(){peers.insert(addr,now);break;}
                }
                continue
            }
            r=v4.recv_from(&mut b4)=>r.map(|(n,a)|(b4[..n].to_vec(),a)),
            r=async{match &v6{Some(s)=>s.recv_from(&mut b6).await,None=>std::future::pending().await}}=>r.map(|(n,a)|(b6[..n].to_vec(),a)),
        };
        let Ok((bytes, peer)) = recv else { continue };
        if peers
            .get(&peer)
            .is_some_and(|last| last.elapsed() < Duration::from_secs(180))
            && responses.send((endpoint(peer), bytes)).await.is_err()
        {
            return Ok(());
        }
    }
}

async fn serve_stream(
    mut s: MuxStream,
    config: Arc<ServerConfig>,
    counts: Arc<Counters>,
) -> io::Result<()> {
    let target = read_target(&mut s, false).await?;
    if target.host != UOT_MAGIC && target.host != UOT_LEGACY_MAGIC {
        let _flow = counts.guard(false);
        let addrs = config
            .routes
            .server_endpoints(&target, Network::Tcp)
            .await?;
        let mut remote = connect_any(addrs).await?;
        let mut metered = Meter {
            stream: s,
            counts,
            from_client: true,
        };
        tokio::io::copy_bidirectional(&mut metered, &mut remote).await?;
        return Ok(());
    }
    let _flow = counts.guard(true);
    let fixed = if target.host == UOT_MAGIC {
        let connect = s.read_u8().await? != 0;
        let initial = read_target(&mut s, false).await?;
        connect.then_some(initial)
    } else {
        None
    };
    let (mut read, mut write) = tokio::io::split(s);
    let (out_tx, out_rx) = mpsc::channel(32);
    let (in_tx, mut in_rx) = mpsc::channel(32);
    let pump_out = async {
        loop {
            let p = read_packet(&mut read, fixed.as_ref()).await?;
            counts.up.fetch_add(p.1.len() as u64, Relaxed);
            if out_tx.send(p).await.is_err() {
                return Ok(());
            }
        }
    };
    let pump_in = async {
        while let Some((target, bytes)) = in_rx.recv().await {
            let bytes: Vec<u8> = bytes;
            write_packet(&mut write, fixed.is_some(), &target, &bytes).await?;
            counts.down.fetch_add(bytes.len() as u64, Relaxed);
        }
        Ok(())
    };
    tokio::select! {r=pump_out=>r,r=pump_in=>r,r=udp_direct(out_rx,in_tx,Some(config.routes.clone()))=>r}
}

enum Command {
    Reload(Config, oneshot::Sender<io::Result<()>>),
    Preconnect(oneshot::Sender<io::Result<()>>),
}
pub struct Service {
    addr: SocketAddr,
    counts: Arc<Counters>,
    logs: broadcast::Sender<String>,
    commands: mpsc::Sender<Command>,
    stop_tx: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
    timer: Option<JoinHandle<()>>,
}
impl Service {
    pub async fn start(config: Config) -> io::Result<Self> {
        let counts = Arc::new(Counters::default());
        let (c_tx, c_rx) = mpsc::channel(16);
        let (stop_tx, stop_rx) = watch::channel(false);
        let (logs, _) = broadcast::channel(128);
        let (addr, task) = match config {
            Config::Client(cfg) => {
                let listener = TcpListener::bind(cfg.listen).await?;
                let addr = listener.local_addr()?;
                let rt = Arc::new(ClientRuntime {
                    tunnel: cfg.node.clone().map(TunnelClient::new),
                    config: cfg,
                    counts: counts.clone(),
                    stop: stop_rx.clone(),
                    logs: logs.clone(),
                    generation: tokio_util::sync::CancellationToken::new(),
                });
                (
                    addr,
                    tokio::spawn(run_client(listener, rt, c_rx, stop_rx.clone())),
                )
            }
            Config::Server(cfg) => {
                let listener = TunnelListener::bind(&cfg).await?;
                let addr = listener.local_addr()?;
                (
                    addr,
                    tokio::spawn(run_server(
                        listener,
                        Arc::new(cfg),
                        counts.clone(),
                        c_rx,
                        stop_rx.clone(),
                        logs.clone(),
                    )),
                )
            }
        };
        let c = counts.clone();
        let mut stop = stop_rx;
        let timer = tokio::spawn(async move {
            let mut time = Instant::now();
            let mut prev = Stats::default();
            loop {
                tokio::select! {_=stop.changed()=>break,_=tokio::time::sleep(Duration::from_secs(1))=>{
                    let now=Instant::now();let cur=c.stats();let elapsed=now.duration_since(time).as_secs_f64();
                    c.up_rate.store(((cur.up_total-prev.up_total) as f64/elapsed) as u64,Relaxed);
                    c.down_rate.store(((cur.down_total-prev.down_total) as f64/elapsed) as u64,Relaxed);time=now;prev=cur;
                }}
            }
        });
        Ok(Self {
            addr,
            counts,
            logs,
            commands: c_tx,
            stop_tx,
            task: Some(task),
            timer: Some(timer),
        })
    }
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }
    pub fn snapshot(&self) -> Stats {
        self.counts.stats()
    }
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.logs.subscribe()
    }
    pub async fn preconnect(&self) -> io::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Preconnect(tx))
            .await
            .map_err(|_| invalid("service stopped"))?;
        rx.await.map_err(|_| invalid("service stopped"))?
    }
    pub async fn reload(&mut self, cfg: Config) -> io::Result<()> {
        let new_addr = match &cfg {
            Config::Client(c) => c.listen,
            Config::Server(c) => c.listen,
        };
        if new_addr.ip() != self.addr.ip()
            || (new_addr.port() != self.addr.port() && new_addr.port() != 0)
        {
            let replacement = Self::start(cfg).await?;
            let mut old = std::mem::replace(self, replacement);
            old.stop().await;
            return Ok(());
        }
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Reload(cfg, tx))
            .await
            .map_err(|_| invalid("service stopped"))?;
        rx.await.map_err(|_| invalid("service stopped"))?
    }
    pub async fn stop(&mut self) {
        self.stop_tx.send_replace(true);
        if let Some(t) = self.task.take() {
            let _ = t.await;
        }
        if let Some(t) = self.timer.take() {
            let _ = t.await;
        }
        let _ = self.logs.send("Stopped".into());
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.stop_tx.send_replace(true);
    }
}

async fn run_client(
    listener: TcpListener,
    mut rt: Arc<ClientRuntime>,
    mut cmds: mpsc::Receiver<Command>,
    mut stop: watch::Receiver<bool>,
) {
    let mut flows = JoinSet::new();
    let mut timer = tokio::time::interval(Duration::from_millis(250));
    let mut retired: Vec<(std::sync::Weak<TunnelClient>, (u64, u64))> = vec![];
    let mut baseline = (0, 0);
    loop {
        tokio::select! {
            _=stop.changed()=>break,
            _=timer.tick()=>{
                let mut sum=baseline;
                retired.retain_mut(|(weak,last)|{
                    if let Some(t)=weak.upgrade(){*last=t.wire_totals();sum.0+=last.0;sum.1+=last.1;true}
                    else{baseline.0+=last.0;baseline.1+=last.1;sum.0+=last.0;sum.1+=last.1;false}
                });
                if let Some(t)=&rt.tunnel{let c=t.wire_totals();sum.0+=c.0;sum.1+=c.1;}rt.counts.wire(sum);
            },
            Some(cmd)=cmds.recv()=>match cmd{
                Command::Reload(Config::Client(cfg),reply)=>{
                    let same_node=rt.config.node==cfg.node;
                    let tunnel=if same_node{rt.tunnel.clone()}else{
                        if let Some(t)=&rt.tunnel{retired.push((Arc::downgrade(t),t.wire_totals()));}
                        cfg.node.clone().map(TunnelClient::new)
                    };
                    let generation=if same_node{rt.generation.clone()}else{
                        let expired=rt.generation.clone();
                        flows.spawn(async move{tokio::time::sleep(Duration::from_secs(30)).await;expired.cancel();});
                        tokio_util::sync::CancellationToken::new()
                    };
                    rt=Arc::new(ClientRuntime{config:cfg,tunnel,counts:rt.counts.clone(),stop:rt.stop.clone(),logs:rt.logs.clone(),generation});
                    let _=rt.logs.send("Configuration reloaded".into());let _=reply.send(Ok(()));
                }
                Command::Reload(_,reply)=>{let _=reply.send(Err(invalid("listener role cannot change in place")));}
                Command::Preconnect(reply)=>{let rt=rt.clone();flows.spawn(async move{
                    let result=match &rt.tunnel{Some(t)=>t.connect().await,None=>Ok(())};
                    if result.is_ok() && rt.tunnel.is_some(){let _=rt.logs.send("Tunnel authenticated".into());}
                    let _=reply.send(result);
                });}
            },
            res=listener.accept()=>match res {
                Ok((s,_))=>{let rt=rt.clone();flows.spawn(async move{
                    tokio::select!{_=rt.generation.cancelled()=>{},result=mixed::serve(s,rt.clone())=>{
                        if let Err(e)=result{let _=rt.logs.send(format!("Mixed connection ended ({:?})",e.kind()));}
                    }}
                });}
                Err(_)=>break,
            },
            Some(_)=flows.join_next(),if !flows.is_empty()=>{},
        }
    }
    flows.shutdown().await;
}

async fn run_server(
    mut listener: TunnelListener,
    mut config: Arc<ServerConfig>,
    counts: Arc<Counters>,
    mut cmds: mpsc::Receiver<Command>,
    mut stop: watch::Receiver<bool>,
    logs: broadcast::Sender<String>,
) {
    let mut flows = JoinSet::new();
    let mut timer = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            _=stop.changed()=>break,
            _=timer.tick()=>counts.wire(listener.wire_totals()),
            Some(cmd)=cmds.recv()=>match cmd{
                Command::Reload(Config::Server(new),reply)=>{let result=listener.reload(&new);if result.is_ok(){config=Arc::new(new);let _=logs.send("Configuration reloaded".into());}let _=reply.send(result);}
                Command::Reload(_,reply)=>{let _=reply.send(Err(invalid("listener role cannot change in place")));}
                Command::Preconnect(reply)=>{let _=reply.send(Err(invalid("no client tunnel")));}
            },
            s=listener.accept()=>match s {
                Ok(s)=>{let cfg=config.clone();let c=counts.clone();let log=logs.clone();flows.spawn(async move{if let Err(e)=serve_stream(s,cfg,c).await{let _=log.send(format!("Server stream ended ({:?})",e.kind()));}});}
                Err(_)=>break,
            },
            Some(_)=flows.join_next(),if !flows.is_empty()=>{},
        }
    }
    counts.wire(listener.wire_totals());
    flows.shutdown().await;
}
