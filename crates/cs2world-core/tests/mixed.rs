use cs2world_core::{config::Config, service::Service};
use std::net::SocketAddr;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
};

fn client() -> Config {
    Config::from_json(r#"{"inbounds":[{"type":"mixed","listen_port":0}],"outbounds":[{"type":"direct","tag":"direct"}]}"#).unwrap()
}

#[tokio::test]
async fn connect_preserves_coalesced_bytes_and_half_close() {
    let mut service = Service::start(client()).await.unwrap();
    let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dst = echo.local_addr().unwrap();
    let origin = tokio::spawn(async move {
        let (mut s, _) = echo.accept().await.unwrap();
        let mut bytes = vec![];
        s.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"hello");
        s.write_all(b"reply").await.unwrap();
    });
    let mut c = TcpStream::connect(service.local_addr()).await.unwrap();
    c.write_all(format!("CONNECT {dst} HTTP/1.1\r\nHost: {dst}\r\n\r\nhello").as_bytes())
        .await
        .unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(c.read_u8().await.unwrap());
    }
    assert!(head.starts_with(b"HTTP/1.1 200"));
    c.shutdown().await.unwrap();
    let mut bytes = vec![];
    c.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(bytes, b"reply");
    origin.await.unwrap();
    assert_eq!(service.snapshot().up_total, 5);
    assert_eq!(service.snapshot().down_total, 5);
    let addr = service.local_addr();
    service.stop().await;
    service.stop().await;
    TcpListener::bind(addr).await.unwrap();
}

#[tokio::test]
async fn udp_associate_preserves_two_origins() {
    let mut service = Service::start(client()).await.unwrap();
    let mut control = TcpStream::connect(service.local_addr()).await.unwrap();
    control.write_all(&[5, 1, 0]).await.unwrap();
    let mut ok = [0; 2];
    control.read_exact(&mut ok).await.unwrap();
    assert_eq!(ok, [5, 0]);
    control
        .write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0])
        .await
        .unwrap();
    let mut reply = [0; 10];
    control.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply[..4], &[5, 0, 0, 1]);
    let relay = SocketAddr::from((
        [reply[4], reply[5], reply[6], reply[7]],
        u16::from_be_bytes([reply[8], reply[9]]),
    ));
    let c = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut buf = [0; 64];
    for _ in 0..2 {
        let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut record = vec![0, 0, 0, 1, 127, 0, 0, 1];
        record.extend_from_slice(&echo.local_addr().unwrap().port().to_be_bytes());
        record.extend_from_slice(b"test");
        c.send_to(&record, relay).await.unwrap();
        let (n, addr) = echo.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"test");
        echo.send_to(b"test", addr).await.unwrap();
        let (n, _) = c.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], &record);
    }
    drop(control);
    service.stop().await;
}

#[test]
fn rejects_unsupported_acl_with_path() {
    let result = Config::from_json(
        r#"{"inbounds":[{"type":"mixed"}],"outbounds":[{"type":"direct"}],"route":{"rules":[{"port":80,"outbound":"direct"}]}}"#,
    );
    assert!(result
        .err()
        .unwrap()
        .to_string()
        .contains("route.rules[0].port"));
}

#[tokio::test]
async fn persistent_http_can_change_origin_after_a_chunked_post() {
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::{body::Incoming, Request, Response};
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;

    async fn origin(path: &'static str, expected: &'static [u8]) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            let serve = hyper::service::service_fn(move |req: Request<Incoming>| async move {
                assert_eq!(req.uri().to_string(), path);
                assert!(!req.headers().contains_key("x-local"));
                assert_eq!(req.headers()["host"], addr.to_string());
                assert_eq!(
                    &req.into_body().collect().await.unwrap().to_bytes()[..],
                    expected
                );
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"ok"))))
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(s), serve)
                .await;
        });
        addr
    }
    let mut service = Service::start(client()).await.unwrap();
    let mut c = TcpStream::connect(service.local_addr()).await.unwrap();
    for (path, body) in [("/one", &b"hello"[..]), ("/two", &b""[..])] {
        let port = origin(path, body).await;
        let request = if body.is_empty() {
            format!("GET http://{port}{path} HTTP/1.1\r\nHost: invalid\r\n\r\n")
        } else {
            format!("POST http://{port}{path} HTTP/1.1\r\nHost: invalid\r\nConnection: x-local\r\nX-Local: secret\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n")
        };
        c.write_all(request.as_bytes()).await.unwrap();
        let mut hdr = vec![];
        while !hdr.ends_with(b"\r\n\r\n") {
            hdr.push(c.read_u8().await.unwrap());
        }
        assert!(hdr.starts_with(b"HTTP/1.1 200"));
        let mut body = [0; 2];
        c.read_exact(&mut body).await.unwrap();
        assert_eq!(&body, b"ok");
    }
    service.stop().await;
}
