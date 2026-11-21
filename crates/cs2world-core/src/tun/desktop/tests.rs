use super::*;
use crate::service::read_target;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    time::{timeout, Duration},
};

/// Uses only the documentation address 198.51.100.42/32. The host's
/// default route and system proxy stay on their original interfaces.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires administrator/root and a usable Wintun or utun driver"]
async fn native_device_routes_and_forwards_one_test_address() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mixed = listener.local_addr().unwrap();
    let target = "198.51.100.42:18443".parse::<SocketAddr>().unwrap();
    let capture = "198.51.100.42/32".parse::<ipnet::IpNet>().unwrap();
    let mut tun = start_capture(mixed, mixed, vec![capture]).await.unwrap();
    let check = timeout(Duration::from_secs(15), async {
        let (delivered, received) = tokio::sync::oneshot::channel();
        let stub = async {
            let mut received = Some(received);
            for _ in 0..2 {
                let (mut control, _) = listener.accept().await?;
                let mut triple = [0; 3];
                control.read_exact(&mut triple).await?;
                assert_eq!(triple, [5, 1, 0]);
                control.write_all(&[5, 0]).await?;
                control.read_exact(&mut triple).await?;
                let command = triple[1];
                let dst = read_target(&mut control, false).await?;
                let relay = UdpSocket::bind("127.0.0.1:0").await?;
                let port = relay.local_addr()?.port();
                let mut reply = vec![5, 0, 0, 1, 127, 0, 0, 1];
                reply.extend_from_slice(&port.to_be_bytes());
                control.write_all(&reply).await?;
                if command == 1 {
                    assert_eq!(dst.host, target.ip().to_string());
                    assert_eq!(dst.port, target.port());
                    let mut data = [0; 4];
                    control.read_exact(&mut data).await?;
                    assert_eq!(&data, b"ping");
                    control.write_all(b"pong").await?;
                } else {
                    assert_eq!(command, 3);
                    let mut packet = [0; 128];
                    let (n, src) = relay.recv_from(&mut packet).await?;
                    assert_eq!(
                        &packet[..n - 4],
                        &[0, 0, 0, 1, 198, 51, 100, 42, 0x48, 0x0b]
                    ); // 18443
                    assert_eq!(&packet[n - 4..n], b"ping");
                    packet[n - 4..n].copy_from_slice(b"pong");
                    relay.send_to(&packet[..n], src).await?;
                    // Keep the SOCKS association open until the native UDP socket gets the reply.
                    received
                        .take()
                        .expect("single UDP association")
                        .await
                        .map_err(|_| io::Error::other("native UDP receiver stopped"))?;
                }
            }
            Ok::<(), io::Error>(())
        };
        let traffic = async {
            let mut s = TcpStream::connect(target).await?;
            s.write_all(b"ping").await?;
            let mut data = [0; 4];
            s.read_exact(&mut data).await?;
            assert_eq!(&data, b"pong");
            let udp = UdpSocket::bind("0.0.0.0:0").await?;
            udp.connect(target).await?;
            udp.send(b"ping").await?;
            let n = udp.recv(&mut data).await?;
            assert_eq!(&data[..n], b"pong");
            let _ = delivered.send(());
            Ok::<(), io::Error>(())
        };
        tokio::try_join!(stub, traffic).map(|_| ())
    })
    .await;
    tun.stop().await.unwrap();
    let table = Routes::snapshot().await.unwrap();
    assert!(!table
        .original
        .iter()
        .any(|r| r.destination() == target.ip() && r.prefix() == 32));
    check.unwrap().unwrap();
}
