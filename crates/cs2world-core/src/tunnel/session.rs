use super::socket::{now_ms, transient};
use super::stream::{Chunk, Command, Inbox, MuxStream};
use crate::config::KcpConfig;
use bytes::{Buf, Bytes, BytesMut};
use cs2world_proto::gns::{self, Codec};
use cs2world_proto::mux::{self, Command as Op};
use cs2world_proto::reliable::{extract_conv, Reliable};
use cs2world_proto::shaper::Shaper;
use rand_core::OsRng;
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{self, Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

const SHARED_BUFFER: usize = 4 * 1024 * 1024;
const MAX_STREAMS: usize = 16384;
type Datagram = (Vec<u8>, SocketAddr);
pub(super) type Shaping = Arc<Mutex<Shaper>>;

pub(super) struct Peer {
    pub wire: mpsc::Sender<Datagram>,
    pub stop: CancellationToken,
}
pub(super) struct Endpoint {
    pub commands: mpsc::Sender<Command>,
    pub stop: CancellationToken,
}
impl Endpoint {
    pub async fn open(&self) -> io::Result<MuxStream> {
        let (ready, rx) = oneshot::channel();
        self.commands
            .send(Command::Open(ready))
            .await
            .map_err(|_| disconnected())?;
        rx.await.map_err(|_| disconnected())?
    }
}
pub(super) fn disconnected() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionReset, "CS2World session ended")
}
fn wire_error(e: impl std::fmt::Debug) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("CS2World framing: {e:?}"),
    )
}

pub(super) struct Session {
    meter: Arc<super::WireMeter>,
    socket: Arc<UdpSocket>,
    peer: SocketAddr,
    codec: Codec,
    config: KcpConfig,
    shaping: Shaping,
    kcp: Option<Reliable>,
    rxbuf: BytesMut,
    streams: HashMap<u32, Option<Inbox>>,
    charged: Arc<AtomicUsize>,
    commands: mpsc::WeakSender<Command>,
    received: mpsc::Receiver<Command>,
    accept: Option<mpsc::Sender<MuxStream>>,
    next_id: u32,
    active: bool,
    stop: CancellationToken,
}

pub(super) struct Params {
    pub meter: Arc<super::WireMeter>,
    pub socket: Arc<UdpSocket>,
    pub addr: SocketAddr,
    pub codec: Codec,
    pub kcp: KcpConfig,
    pub shaping: Shaping,
    pub stop: CancellationToken,
}

fn reliable(config: &KcpConfig, conv: u32) -> io::Result<Reliable> {
    let snd = u16::try_from(config.snd_wnd).map_err(wire_error)?;
    let rcv = u16::try_from(config.rcv_wnd).map_err(wire_error)?;
    Reliable::open(
        conv,
        config.mtu,
        snd,
        rcv,
        config.data_shards,
        config.parity_shards,
    )
    .map_err(wire_error)
}
impl Session {
    fn new(
        p: Params,
        initial: Option<Reliable>,
        accept: Option<mpsc::Sender<MuxStream>>,
    ) -> (Self, mpsc::Sender<Command>) {
        let (commands, received) = mpsc::channel(96);
        let session = Self {
            meter: p.meter,
            socket: p.socket,
            peer: p.addr,
            codec: p.codec,
            config: p.kcp,
            shaping: p.shaping,
            stop: p.stop,
            kcp: initial,
            rxbuf: BytesMut::new(),
            streams: HashMap::new(),
            charged: Arc::new(AtomicUsize::new(0)),
            next_id: if accept.is_some() { 2 } else { 3 },
            accept,
            active: true,
            commands: commands.downgrade(),
            received,
        };
        (session, commands)
    }
    pub(super) fn client(p: Params, conv: u32) -> io::Result<Endpoint> {
        let kcp = reliable(&p.kcp, conv)?;
        let stop = p.stop.clone();
        let (s, commands) = Self::new(p, Some(kcp), None);
        tokio::spawn(async move {
            s.run(None).await;
        });
        Ok(Endpoint { commands, stop })
    }
    pub(super) fn server(p: Params, accept: mpsc::Sender<MuxStream>) -> Peer {
        let (wire, rx) = mpsc::channel(256);
        let stop = p.stop.clone();
        let (s, hold) = Self::new(p, None, Some(accept));
        tokio::spawn(async move {
            s.run(Some(rx)).await;
            drop(hold);
        });
        Peer { wire, stop }
    }

    fn emit(&mut self, op: Op, id: u32, data: &[u8]) -> io::Result<()> {
        let frame = mux::encode_frame(op, id, data).map_err(wire_error)?;
        self.kcp
            .as_mut()
            .ok_or_else(disconnected)?
            .send(&frame)
            .map_err(wire_error)
    }
    fn stream(&mut self, id: u32) -> io::Result<MuxStream> {
        if self.streams.len() >= MAX_STREAMS {
            return Err(io::Error::other("smux stream limit reached"));
        }
        let (stream, inbox) = MuxStream::new(id, self.commands.upgrade().ok_or_else(disconnected)?);
        self.streams.insert(id, Some(inbox));
        Ok(stream)
    }
    fn command(&mut self, cmd: Command) -> io::Result<()> {
        match cmd {
            Command::Open(reply) => {
                let sid = self.next_id;
                self.next_id = sid.checked_add(2).ok_or_else(disconnected)?;
                let result = self.stream(sid);
                if result.is_ok() {
                    self.emit(Op::Syn, sid, &[])?;
                }
                let _ = reply.send(result);
            }
            Command::Data(id, data) => self.emit(Op::Push, id, &data)?,
            Command::Fin(id) => self.emit(Op::Fin, id, &[])?,
            Command::Release(id, fin) => {
                if self.streams.remove(&id).is_some() && fin {
                    self.emit(Op::Fin, id, &[])?;
                }
            }
        }
        Ok(())
    }
    fn deliver(&mut self) -> io::Result<()> {
        if self.charged.load(Ordering::Relaxed) >= SHARED_BUFFER {
            return Ok(());
        }
        if let Some(kcp) = self.kcp.as_mut() {
            self.rxbuf.extend_from_slice(&kcp.recv());
        }
        while self.charged.load(Ordering::Relaxed) < SHARED_BUFFER {
            let Some((frame, consumed)) = mux::decode_frame(&self.rxbuf).map_err(wire_error)?
            else {
                break;
            };
            let (op, id, len) = (frame.cmd, frame.sid, frame.payload.len());
            let mut bytes: Bytes = self.rxbuf.split_to(consumed).freeze();
            bytes.advance(consumed - len);
            self.active = true;
            match op {
                Op::Syn if !self.streams.contains_key(&id) => {
                    if let Some(accept) = self.accept.clone() {
                        let stream = self.stream(id)?;
                        // Avoid a slow acceptor blocking all UDP peers or an unbounded accept queue.
                        if let Err(err) = accept.try_send(stream) {
                            drop(err.into_inner());
                        }
                    }
                }
                Op::Fin => {
                    if let Some(incoming) = self.streams.get_mut(&id) {
                        incoming.take();
                    }
                }
                Op::Push if len > 0 => {
                    if let Some(Some(to)) = self.streams.get(&id) {
                        let _ = to.send(Ok(Chunk::new(bytes, &self.charged)));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    async fn input(&mut self, wire: &[u8], from: SocketAddr) -> io::Result<()> {
        match self.codec.open(wire) {
            Ok((mode, body)) => {
                self.peer = from;
                if mode == 0 || body.is_empty() {
                    return Ok(());
                }
                if self.kcp.is_none() {
                    let Some(conv) = extract_conv(&body) else {
                        return Ok(());
                    };
                    self.kcp = Some(reliable(&self.config, conv)?);
                }
                if let Some(k) = self.kcp.as_mut() {
                    let _ = k.input(now_ms() as u32, &body);
                }
            }
            Err(gns::Error::Replay) => {}
            Err(_) if self.accept.is_some() => {
                let _ = self
                    .meter
                    .send(&self.socket, &gns::reject_for_packet(wire), from)
                    .await;
            }
            Err(_) => {}
        }
        Ok(())
    }
    async fn flush(&mut self) -> io::Result<()> {
        if let Some(k) = self.kcp.as_mut() {
            let now = now_ms();
            for data in k.update(now as u32) {
                let (mode, size) = self
                    .shaping
                    .lock()
                    .unwrap()
                    .plan(data.len(), now, &mut OsRng);
                let wire = self
                    .codec
                    .seal(&data, mode, size, &mut OsRng)
                    .map_err(wire_error)?;
                self.meter.send(&self.socket, &wire, self.peer).await?;
            }
        }
        Ok(())
    }
    async fn run(mut self, mut forwarded: Option<mpsc::Receiver<Datagram>>) {
        let result = self.pump(&mut forwarded).await;
        for tx in self.streams.values().flatten() {
            let err = result
                .as_ref()
                .err()
                .map(|e| io::Error::new(e.kind(), e.to_string()))
                .unwrap_or_else(disconnected);
            let _ = tx.send(Err(err));
        }
        self.stop.cancel();
    }
    async fn pump(&mut self, forwarded: &mut Option<mpsc::Receiver<Datagram>>) -> io::Result<()> {
        let socket = Arc::clone(&self.socket);
        let client = forwarded.is_none();
        let mut raw = [0u8; 2048];
        let mut tick = ticker(15);
        let mut heartbeat = ticker(cs2world_proto::shaper::HEARTBEAT_POLL_MS);
        let mut ping = ticker(15_000);
        let mut timeout = ticker(45_000);
        let started = Instant::now();
        loop {
            let can_write = self
                .kcp
                .as_ref()
                .is_some_and(|k| k.pending() < 2 * self.config.snd_wnd);
            tokio::select! {
                _=self.stop.cancelled()=>return Ok(()),
                _=tick.tick()=>{},
                packet=socket.recv_from(&mut raw), if client=>{
                    match packet {
                        Ok((n,from)) if from==self.peer=>{self.meter.received(n);self.input(&raw[..n],from).await?;},
                        Err(e) if !transient(&e)=>return Err(e),
                        _=>{}
                    }
                }
                incoming=async {forwarded.as_mut().unwrap().recv().await},if !client=>{
                    let (wire,from)=incoming.ok_or_else(disconnected)?;
                    self.input(&wire,from).await?;
                }
                cmd=self.received.recv(),if can_write=>{
                    self.command(cmd.ok_or_else(disconnected)?)?;
                }
                _=heartbeat.tick(),if self.kcp.is_some()=>{
                    let size=self.shaping.lock().unwrap().dummy(now_ms(),&mut OsRng);
                    if let Some(size)=size{
                        let wire=self.codec.seal(&[],0,size,&mut OsRng).map_err(wire_error)?;
                        self.meter.send(&self.socket,&wire,self.peer).await?;
                    }
                }
                _=ping.tick(),if self.kcp.is_some()=> self.emit(Op::Nop,0,&[])?,
                _=timeout.tick()=>{
                    if self.kcp.is_none() {
                        if started.elapsed().as_secs()>900 { return Err(disconnected()); }
                    } else if !self.active && self.charged.load(Ordering::Relaxed)<SHARED_BUFFER {
                        return Err(io::Error::new(io::ErrorKind::TimedOut,"smux keepalive expired"));
                    }
                    self.active=false;
                }
            }
            self.deliver()?;
            self.flush().await?;
        }
    }
}
fn ticker(ms: u64) -> time::Interval {
    let period = Duration::from_millis(ms);
    let mut tick = time::interval_at(Instant::now() + period, period);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    tick
}
