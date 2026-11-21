use super::*;
use crate::{config::Config, service::Service};
use etherparse::PacketBuilder;
use tokio::net::TcpListener;

async fn harness() -> (Stack, JoinSet<io::Result<()>>, Service) {
    let cfg = Config::from_json(
        r#"{"inbounds":[{"type":"mixed","listen":"0.0.0.0","listen_port":0}],"outbounds":[{"type":"direct"}]}"#,
    )
    .unwrap();
    let service = Service::start(cfg).await.unwrap();
    let Bridge {
        mixed,
        stack,
        runner,
        tcp,
        udp,
    } = Bridge::new(service.local_addr()).unwrap();
    let mut workers = JoinSet::new();
    workers.spawn(runner);
    let cache = names::Cache::default();
    workers.spawn(tcp_worker(tcp, mixed, cache.clone()));
    workers.spawn(udp_worker(udp, mixed, cache));
    (stack, workers, service)
}

#[tokio::test]
async fn raw_udp_roundtrip_ipv4_ipv6() {
    time::timeout(Duration::from_secs(10), async {
        let (mut stack, mut workers, mut service) = harness().await;
        for endpoint in ["127.0.0.1:0", "[::1]:0"] {
            let socket = UdpSocket::bind(endpoint).await.unwrap();
            let target = socket.local_addr().unwrap();
            let packet = if target.is_ipv4() {
                PacketBuilder::ipv4([10, 0, 0, 2], [127, 0, 0, 1], 64)
            } else {
                PacketBuilder::ipv6(
                    "fd01::2".parse::<std::net::Ipv6Addr>().unwrap().octets(),
                    std::net::Ipv6Addr::LOCALHOST.octets(),
                    64,
                )
            };
            let mut ip = Vec::new();
            packet
                .udp(31001, target.port())
                .write(&mut ip, b"outbound")
                .unwrap();
            stack.send(ip).await.unwrap();
            let mut buf = [0; 32];
            let (n, peer) = socket.recv_from(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"outbound");
            socket.send_to(b"inbound", peer).await.unwrap();
            let frame = stack.next().await.unwrap().unwrap();
            let offset = if target.is_ipv4() { 20 } else { 40 };
            assert_eq!(
                u16::from_be_bytes(frame[offset..offset + 2].try_into().unwrap()),
                target.port()
            );
            assert_eq!(
                u16::from_be_bytes(frame[offset + 2..offset + 4].try_into().unwrap()),
                31001
            );
            assert_eq!(&frame[offset + 8..], b"inbound");
        }
        workers.shutdown().await;
        service.stop().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn raw_tcp_handshake_and_echo() {
    time::timeout(Duration::from_secs(10), async {
        let (mut stack, mut workers, mut service) = harness().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let packet = |seq: u32, ack: Option<u32>, payload: &[u8]| {
            let packet =
                PacketBuilder::ipv4([10, 0, 0, 3], [127, 0, 0, 1], 64).tcp(31002, port, seq, 65535);
            let packet = if let Some(a) = ack {
                packet.ack(a)
            } else {
                packet.syn()
            };
            let mut bytes = vec![];
            packet.write(&mut bytes, payload).unwrap();
            bytes
        };
        stack.send(packet(313, None, &[])).await.unwrap();
        let syn_ack = stack.next().await.unwrap().unwrap();
        assert_eq!(syn_ack[33] & 0x12, 0x12);
        let ack = u32::from_be_bytes(syn_ack[24..28].try_into().unwrap()).wrapping_add(1);
        stack.send(packet(314, Some(ack), &[])).await.unwrap();
        stack.send(packet(314, Some(ack), b"query")).await.unwrap();
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0; 5];
        socket.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"query");
        socket.write_all(b"reply").await.unwrap();
        loop {
            let bytes = stack.next().await.unwrap().unwrap();
            let header = 20 + usize::from(bytes[32] >> 4) * 4;
            if !bytes[header..].is_empty() {
                assert_eq!(&bytes[header..], b"reply");
                break;
            }
        }
        workers.shutdown().await;
        service.stop().await;
    })
    .await
    .unwrap();
}
