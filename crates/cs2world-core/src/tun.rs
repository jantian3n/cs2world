//! Shared IP-to-Mixed bridge. Platform code supplies a layer-3 packet device.
use crate::service::{addr_write, endpoint, invalid, read_target};
use cs2world_proto::mux::{decode_addr, encode_addr};
use futures_util::{SinkExt, StreamExt};
use netstack_smoltcp::{Stack, StackBuilder};
use std::{
    collections::HashMap,
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    sync::mpsc,
    task::{JoinHandle, JoinSet},
    time,
};
use tokio_util::sync::CancellationToken;
use tun_rs::AsyncDevice;

#[cfg(any(windows, target_os = "macos"))]
mod desktop;
pub(crate) mod names;
#[cfg(any(windows, target_os = "macos"))]
pub use desktop::start_desktop;

/// Stop waits until packet workers have exited and desktop routes have been restored.
pub struct TunSession {
    cancel: CancellationToken,
    task: Option<JoinHandle<io::Result<()>>>,
}
impl TunSession {
    pub async fn stop(&mut self) -> io::Result<()> {
        self.cancel.cancel();
        match self.task.take() {
            Some(task) => task.await.map_err(io::Error::other)?,
            None => Ok(()),
        }
    }
}
impl Drop for TunSession {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(target_os = "android")]
pub async fn start_fd(mixed: SocketAddr, fd: std::os::fd::OwnedFd) -> io::Result<TunSession> {
    use std::os::fd::IntoRawFd;
    // from_fd takes ownership; VpnService provides raw IP packets.
    let device = unsafe { AsyncDevice::from_fd(fd.into_raw_fd()) }?;
    let bridge = Bridge::new(mixed)?;
    let cancel = CancellationToken::new();
    let task = tokio::spawn(bridge.drive(Arc::new(device), cancel.clone()));
    Ok(TunSession {
        cancel,
        task: Some(task),
    })
}

struct Bridge {
    mixed: SocketAddr,
    stack: Stack,
    runner: netstack_smoltcp::Runner,
    tcp: netstack_smoltcp::TcpListener,
    udp: netstack_smoltcp::UdpSocket,
}
impl Bridge {
    fn new(mut mixed: SocketAddr) -> io::Result<Self> {
        if mixed.ip().is_unspecified() {
            let ip = if mixed.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            };
            mixed.set_ip(ip);
        }
        let (stack, runner, udp, tcp) = StackBuilder::default()
            .enable_tcp(true)
            .enable_udp(true)
            .enable_icmp(true)
            .mtu(1500)
            .tcp_recv_buffer_size(65536)
            .tcp_send_buffer_size(65536)
            .build()?;
        Ok(Self {
            mixed,
            stack,
            runner: runner.unwrap(),
            udp: udp.unwrap(),
            tcp: tcp.unwrap(),
        })
    }
    async fn drive(self, device: Arc<AsyncDevice>, cancel: CancellationToken) -> io::Result<()> {
        let (mut sink, mut stream) = self.stack.split();
        let mut workers = JoinSet::new();
        let tun = device.clone();
        workers.spawn(async move {
            let mut buf = vec![0; 65535];
            loop {
                let n = tun.recv(&mut buf).await?;
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "TUN disconnected",
                    ));
                }
                sink.send(buf[..n].to_vec()).await?;
            }
        });
        workers.spawn(async move {
            while let Some(packet) = stream.next().await {
                let packet = packet?;
                if device.send(&packet).await? != packet.len() {
                    return Err(io::Error::new(io::ErrorKind::WriteZero, "short TUN write"));
                }
            }
            Ok(())
        });
        workers.spawn(self.runner);
        let names = names::Cache::default();
        workers.spawn(tcp_worker(self.tcp, self.mixed, names.clone()));
        workers.spawn(udp_worker(self.udp, self.mixed, names));
        let result = tokio::select! {
            _=cancel.cancelled()=>Ok(()),
            r=workers.join_next()=>r.unwrap().map_err(io::Error::other)?,
        };
        workers.shutdown().await;
        result
    }
}

async fn socks(
    mixed: SocketAddr,
    cmd: u8,
    target: SocketAddr,
    scope: names::Scope,
) -> io::Result<(TcpStream, SocketAddr, names::Lease)> {
    let mut s = TcpStream::connect(mixed).await?;
    let lease = scope.register((s.peer_addr()?, s.local_addr()?));
    s.write_all(&[5, 1, 0]).await?;
    let mut reply = [0; 2];
    s.read_exact(&mut reply).await?;
    if reply != [5, 0] {
        return Err(invalid("Mixed SOCKS authentication failed"));
    }
    s.write_all(&[5, cmd, 0]).await?;
    addr_write(&mut s, &endpoint(target), false).await?;
    let mut reply = [0; 3];
    s.read_exact(&mut reply).await?;
    if reply != [5, 0, 0] {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "Mixed outbound refused",
        ));
    }
    let bound = read_target(&mut s, false).await?;
    let mut ip: IpAddr = bound
        .host
        .parse()
        .map_err(|_| invalid("invalid Mixed bind address"))?;
    if ip.is_unspecified() {
        ip = mixed.ip();
    }
    Ok((s, SocketAddr::new(ip, bound.port), lease))
}
async fn tcp_worker(
    mut tcp: netstack_smoltcp::TcpListener,
    mixed: SocketAddr,
    cache: names::Cache,
) -> io::Result<()> {
    let mut flows = JoinSet::new();
    loop {
        tokio::select! {
            Some((mut socket,src,dest))=tcp.next(),if flows.len()<2048=>{
                let cache=cache.clone();
                flows.spawn(async move{
                    let (prefix,name)=names::sniff(&mut socket,dest.port()).await;
                    let scope=names::Scope{src:src.ip(),name,cache};
                    let (mut remote,_,_lease)=time::timeout(Duration::from_secs(20),socks(mixed,1,dest,scope)).await??;
                    remote.write_all(&prefix).await?;
                    tokio::io::copy_bidirectional(&mut socket,&mut remote).await?;
                    Ok::<(),io::Error>(())
                });
            }
            Some(_)=flows.join_next(),if !flows.is_empty()=>{},
            else=>return Ok(()),
        }
    }
}

type Message = netstack_smoltcp::udp::UdpMsg;
type Outgoing = mpsc::Sender<(SocketAddr, Vec<u8>)>;

async fn udp_worker(
    udp: netstack_smoltcp::UdpSocket,
    mixed: SocketAddr,
    cache: names::Cache,
) -> io::Result<()> {
    let (mut read, mut write) = udp.split();
    let (replies, mut incoming) = mpsc::channel::<Message>(256);
    let mut flows = JoinSet::new();
    let mut peers: HashMap<SocketAddr, Outgoing> = HashMap::new();
    loop {
        tokio::select! {
            packet=read.next()=>{
                let Some((data,src,dst))=packet else{return Ok(())};
                if !peers.contains_key(&src){
                    if peers.len()>=1024 {continue;}
                    let (tx,rx)=mpsc::channel(32);
                    peers.insert(src,tx);
                    let responses=replies.clone();let cache=cache.clone();
                    flows.spawn(async move{let _=association(mixed,src,rx,responses,cache).await;src});
                }
                if let Some(tx)=peers.get(&src){let _=tx.try_send((dst,data));}
            }
            Some(reply)=incoming.recv()=>write.send(reply).await?,
            Some(done)=flows.join_next(),if !flows.is_empty()=>{
                peers.remove(&done.map_err(io::Error::other)?);
            }
        }
    }
}
async fn association(
    mixed: SocketAddr,
    src: SocketAddr,
    mut outbound: mpsc::Receiver<(SocketAddr, Vec<u8>)>,
    replies: mpsc::Sender<Message>,
    cache: names::Cache,
) -> io::Result<()> {
    let udp = UdpSocket::bind(SocketAddr::new(mixed.ip(), 0)).await?;
    let mut requests = names::Pending::new();
    let scope = names::Scope {
        src: src.ip(),
        name: None,
        cache: cache.clone(),
    };
    let (mut control, relay, _lease) = time::timeout(
        Duration::from_secs(20),
        socks(mixed, 3, udp.local_addr()?, scope),
    )
    .await??;
    udp.connect(relay).await?;
    let mut buf = vec![0; 65535];
    loop {
        tokio::select! {
            to=outbound.recv()=>{
                let Some((dest,data))=to else{return Ok(())};
                let mut msg=vec![0,0,0];
                msg.extend(encode_addr(&endpoint(dest),false).map_err(io::Error::other)?);
                names::query(&mut requests,dest,&data);
                msg.extend_from_slice(&data);
                udp.send(&msg).await?;
            }
            n=udp.recv(&mut buf)=>{
                let packet=&buf[..n?];
                if !packet.starts_with(&[0,0,0]){continue;}
                if let Ok(Some((from,k)))=decode_addr(&packet[3..],false){
                    if let Ok(ip)=from.host.parse::<IpAddr>(){
                        names::answer(&cache,&mut requests,src.ip(),SocketAddr::new(ip,from.port),&packet[3+k..]);
                        if ip.is_ipv4()==src.is_ipv4(){let _=replies.send((packet[3+k..].to_vec(),SocketAddr::new(ip,from.port),src)).await;}
                    }
                }
            }
            _=control.read_u8()=>return Ok(()),
            _=time::sleep(Duration::from_secs(120))=>return Ok(()),
        }
    }
}

#[cfg(test)]
mod packet_tests;
