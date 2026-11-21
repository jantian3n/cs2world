use cs2world_proto::a2s::{parse_info, Info, INFO_QUERY};
use std::io;
use std::time::Duration;
use tokio::net::lookup_host;
use tokio::time::{timeout, Instant};

#[derive(Debug, Clone)]
pub struct Probe {
    pub info: Info,
    pub rtt_ms: u64,
}

pub async fn probe(host: &str, port: u16) -> io::Result<Probe> {
    let addr = lookup_host((host, port)).await?.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "node address did not resolve",
        )
    })?;
    let socket = crate::platform_socket::bind_udp(
        if addr.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        }
        .parse()
        .unwrap(),
    )
    .await?;
    socket.connect(addr).await?;
    let start = Instant::now();
    let mut reply = [0u8; 2048];
    let n = timeout(Duration::from_millis(2500), async {
        socket.send(INFO_QUERY).await?;
        socket.recv(&mut reply).await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "A2S_INFO timed out"))??;
    let info =
        parse_info(&reply[..n]).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Probe {
        info,
        rtt_ms: start.elapsed().as_millis() as u64,
    })
}
