use crate::route::{Action, Network, Target};
use crate::service::{
    addr_write, endpoint, invalid, read_target, udp_direct, ClientRuntime, Meter, Packet,
};
use bytes::Bytes;
use cs2world_proto::mux::{decode_addr, encode_addr};
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full};
use hyper::{
    body::Incoming,
    header::{HeaderMap, HeaderName, HOST},
    service::service_fn,
    Method, Request, Response, StatusCode,
};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{atomic::Ordering::Relaxed, Arc};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

fn routing_hint(local: SocketAddr, peer: SocketAddr, target: &Target) -> Option<String> {
    #[cfg(feature = "tun")]
    {
        crate::tun::names::lookup(local, peer, &target.host)
    }
    #[cfg(not(feature = "tun"))]
    {
        let _ = (local, peer, target);
        None
    }
}

pub(crate) async fn serve(mut sock: TcpStream, rt: Arc<ClientRuntime>) -> io::Result<()> {
    let mut first = [0u8; 1];
    if sock.peek(&mut first).await? == 0 {
        return Ok(());
    }
    if first[0] != 5 {
        return hyper::server::conn::http1::Builder::new()
            .max_buf_size(64 * 1024)
            .serve_connection(
                TokioIo::new(sock),
                service_fn(move |req| http_request(req, rt.clone())),
            )
            .with_upgrades()
            .await
            .map_err(io::Error::other);
    }
    let _ = sock.read_u8().await?;
    let n = sock.read_u8().await?;
    let mut methods = vec![0; n as usize];
    sock.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        sock.write_all(&[5, 255]).await?;
        return Ok(());
    }
    sock.write_all(&[5, 0]).await?;
    let mut header = [0; 3];
    sock.read_exact(&mut header).await?;
    if header[0] != 5 || header[2] != 0 {
        return Err(invalid("invalid SOCKS header"));
    }
    let local = sock.local_addr()?;
    let target = match read_target(&mut sock, false).await {
        Ok(t) => t,
        Err(_) => {
            reply(&mut sock, 8, local).await?;
            return Ok(());
        }
    };
    match header[1] {
        1 => {
            let _guard = rt.counts.guard(false);
            let name = routing_hint(local, sock.peer_addr()?, &target);
            let mut remote = match rt.tcp(&target, name.as_deref()).await {
                Ok(stream) => stream,
                Err(e) => {
                    let code = match e.kind() {
                        io::ErrorKind::PermissionDenied => 2,
                        io::ErrorKind::NetworkUnreachable => 3,
                        io::ErrorKind::HostUnreachable => 4,
                        io::ErrorKind::ConnectionRefused => 5,
                        io::ErrorKind::TimedOut => 6,
                        _ => 1,
                    };
                    reply(&mut sock, code, local).await?;
                    return Err(e);
                }
            };
            reply(&mut sock, 0, local).await?;
            let mut metered = Meter {
                stream: sock,
                counts: rt.counts.clone(),
                from_client: true,
            };
            tokio::io::copy_bidirectional(&mut metered, &mut remote).await?;
            Ok(())
        }
        3 => association(sock, rt, target).await,
        _ => reply(&mut sock, 7, local).await,
    }
}

async fn reply(s: &mut TcpStream, status: u8, bound: SocketAddr) -> io::Result<()> {
    s.write_all(&[5, status, 0]).await?;
    addr_write(s, &endpoint(bound), false).await
}

async fn association(mut ctrl: TcpStream, rt: Arc<ClientRuntime>, hint: Target) -> io::Result<()> {
    let peer = ctrl.peer_addr()?;
    let local = ctrl.local_addr()?;
    let peer_ip = peer.ip().to_canonical();
    let bind = SocketAddr::new(ctrl.local_addr()?.ip(), 0);
    let sock = UdpSocket::bind(bind).await?;
    if hint
        .host
        .parse::<IpAddr>()
        .is_ok_and(|ip| !ip.is_unspecified() && ip.to_canonical() != peer_ip)
    {
        reply(&mut ctrl, 2, sock.local_addr()?).await?;
        return Ok(());
    }
    reply(&mut ctrl, 0, sock.local_addr()?).await?;
    let _guard = rt.counts.guard(true);
    let (responses, mut incoming) = mpsc::channel::<Packet>(32);
    let (direct_tx, direct_rx) = mpsc::channel(32);
    let mut tasks = JoinSet::new();
    tasks.spawn(udp_direct(direct_rx, responses.clone(), None));
    let mut proxy_tx: Option<mpsc::Sender<Packet>> = None;
    let mut client = None;
    let mut buf = vec![0; 65535];
    let mut control_byte = [0; 1];
    let timeout = tokio::time::sleep(Duration::from_secs(180));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            ctrl_result=ctrl.read(&mut control_byte)=>{if ctrl_result?==0{break;}},
            _=&mut timeout=>break,
            rx=sock.recv_from(&mut buf)=>{
                let(n,src)=rx?;
                if src.ip().to_canonical()!=peer_ip || (hint.port!=0 && src.port()!=hint.port) || client.is_some_and(|c|c!=src) || n<4 || buf[..3]!=[0,0,0]{continue;}
                let Ok(Some((t,used)))=decode_addr(&buf[3..n],false) else{continue};
                if t.port==0 {continue;}
                let bytes=buf[3+used..n].to_vec();let len=bytes.len();
                let name=routing_hint(local,peer,&t);
                let route=rt.config.routes.action_with_name(&t,Network::Udp,name.as_deref());
                if route==Action::Block{continue;}
                client=Some(src);
                timeout.as_mut().reset(tokio::time::Instant::now()+Duration::from_secs(180));
                let sender=if route==Action::Direct{&direct_tx}else{
                    if proxy_tx.as_ref().is_none_or(|s|s.is_closed()){
                        let(tx,rx)=mpsc::channel(32);let first=t.clone();let r=rt.clone();let writes=responses.clone();
                        tasks.spawn(async move{r.udp_proxy(&first,rx,writes).await});proxy_tx=Some(tx);
                    }
                    proxy_tx.as_ref().expect("created above")
                };
                if sender.try_send((t,bytes)).is_ok(){rt.counts.up.fetch_add(len as u64,Relaxed);}
            }
            Some((t,bytes))=incoming.recv()=>{
                let Some(addr)=client else{continue};
                let mut data=vec![0,0,0];let Ok(encoded)=encode_addr(&t,false) else{continue};
                data.extend_from_slice(&encoded);data.extend_from_slice(&bytes);
                if sock.send_to(&data,addr).await.is_ok(){rt.counts.down.fetch_add(bytes.len() as u64,Relaxed);}
            }
            Some(_)=tasks.join_next(),if !tasks.is_empty()=>{}
        }
    }
    tasks.shutdown().await;
    Ok(())
}

type Body = UnsyncBoxBody<Bytes, hyper::Error>;
fn plain(code: StatusCode, text: &'static str) -> Response<Body> {
    let body = Full::new(Bytes::from_static(text.as_bytes()))
        .map_err(|never: Infallible| match never {})
        .boxed_unsync();
    let mut res = Response::new(body);
    *res.status_mut() = code;
    res
}

fn remove_hop_headers(headers: &mut HeaderMap) {
    let names: Vec<HeaderName> = headers
        .get_all("connection")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|s| HeaderName::from_bytes(s.trim().as_bytes()).ok())
        .collect();
    for name in names {
        headers.remove(name);
    }
    for name in [
        "connection",
        "proxy-connection",
        "proxy-authenticate",
        "proxy-authorization",
        "keep-alive",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
}

async fn http_request(
    mut req: Request<Incoming>,
    rt: Arc<ClientRuntime>,
) -> Result<Response<Body>, Infallible> {
    let connect = req.method() == Method::CONNECT;
    if !connect && req.uri().scheme_str().is_some_and(|s| s != "http") {
        return Ok(plain(StatusCode::BAD_REQUEST, "Expected http URI"));
    }
    let authority = req
        .uri()
        .authority()
        .cloned()
        .or_else(|| req.headers().get(HOST)?.to_str().ok()?.parse().ok());
    let Some(authority) = authority else {
        return Ok(plain(StatusCode::BAD_REQUEST, "Missing authority"));
    };
    let host = authority.host().trim_matches(['[', ']']);
    if host.is_empty()
        || authority.as_str().contains('@')
        || (connect && authority.port_u16().is_none())
    {
        return Ok(plain(StatusCode::BAD_REQUEST, "Invalid authority"));
    }
    let target = Target {
        host: host.into(),
        port: authority.port_u16().unwrap_or(80),
    };
    let guard = rt.counts.guard(false);
    let remote = match rt.tcp(&target, None).await {
        Ok(s) => s,
        Err(e) => {
            return Ok(if e.kind() == io::ErrorKind::PermissionDenied {
                plain(StatusCode::FORBIDDEN, "Blocked by route")
            } else {
                plain(StatusCode::BAD_GATEWAY, "Connection failed")
            })
        }
    };
    let mut metered = Meter {
        stream: remote,
        counts: rt.counts.clone(),
        from_client: false,
    };
    let mut stop = rt.stop.clone();
    let generation = rt.generation.clone();
    if connect {
        let upgrade = hyper::upgrade::on(&mut req);
        tokio::spawn(async move {
            let _guard = guard;
            tokio::select! {_=stop.changed()=>{},_=generation.cancelled()=>{},_=async{
                if let Ok(stream)=upgrade.await{let mut client=TokioIo::new(stream);let _=tokio::io::copy_bidirectional(&mut client,&mut metered).await;}
            }=>{}}
        });
        return Ok(plain(StatusCode::OK, ""));
    }
    remove_hop_headers(req.headers_mut());
    let Ok(host_header) = authority.as_str().parse() else {
        return Ok(plain(StatusCode::BAD_REQUEST, "Invalid authority"));
    };
    req.headers_mut().insert(HOST, host_header);
    let path = req
        .uri()
        .path_and_query()
        .map(|s| s.as_str())
        .unwrap_or("/");
    let Ok(origin) = path.parse() else {
        return Ok(plain(StatusCode::BAD_REQUEST, "Invalid request path"));
    };
    *req.uri_mut() = origin;
    let Ok((mut sender, driver)) =
        hyper::client::conn::http1::handshake(TokioIo::new(metered)).await
    else {
        return Ok(plain(StatusCode::BAD_GATEWAY, "Origin unavailable"));
    };
    tokio::spawn(async move {
        let _guard = guard;
        tokio::select! {_=stop.changed()=>{},_=generation.cancelled()=>{},_=driver=>{}}
    });
    match sender.send_request(req).await {
        Ok(mut res) => {
            remove_hop_headers(res.headers_mut());
            Ok(res.map(BodyExt::boxed_unsync))
        }
        Err(_) => Ok(plain(StatusCode::BAD_GATEWAY, "Origin response failed")),
    }
}
