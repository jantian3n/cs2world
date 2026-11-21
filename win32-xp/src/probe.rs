use crate::models::A2SProbeResult;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::mpsc::Sender;
use std::time::Duration;

pub fn probe_a2s_info(server: &str, port: u16) -> A2SProbeResult {
    let probe = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .and_then(|rt| rt.block_on(cs2world_core::probe::probe(server, port)));
    match probe {
        Ok(result) => {
            let i = result.info;
            A2SProbeResult {
                ok: true,
                game: i.game,
                app_id: i.app_id,
                server_name: i.server_name,
                map_name: i.map_name,
                players: i.players,
                max_players: i.max_players,
                rtt_ms: result.rtt_ms,
                error: None,
            }
        }
        Err(e) => A2SProbeResult {
            ok: false,
            game: String::new(),
            app_id: 0,
            server_name: String::new(),
            map_name: String::new(),
            players: 0,
            max_players: 0,
            rtt_ms: 0,
            error: Some(format!("A2S 查询失败（{}）", e.kind())),
        },
    }
}
pub fn start_exit_ip_check(
    mixed_port: u16,
    results: Sender<Result<String, String>>,
    notify: impl FnOnce() + Send + 'static,
) {
    std::thread::spawn(move || {
        let result = check_exit_ip_via_proxy(mixed_port);
        if results.send(result).is_ok() {
            notify();
        }
    });
}

fn check_exit_ip_via_proxy(mixed_port: u16) -> Result<String, String> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, mixed_port));
    let mut s =
        TcpStream::connect_timeout(&addr, Duration::from_secs(4)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(6)))
        .map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(Duration::from_secs(4)))
        .map_err(|e| e.to_string())?;
    s.write_all(
        b"GET http://api.ipify.org/ HTTP/1.0\r\nHost: api.ipify.org\r\nConnection: close\r\n\r\n",
    )
    .map_err(|e| e.to_string())?;
    let mut response = String::new();
    s.take(8192)
        .read_to_string(&mut response)
        .map_err(|e| e.to_string())?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or("出口检测响应格式错误")?;
    if head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        != Some("200")
    {
        return Err("出口检测未返回 HTTP 200".into());
    }
    body.trim()
        .parse::<IpAddr>()
        .map(|ip| ip.to_string())
        .map_err(|_| "出口检测未返回 IP 地址".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::BufRead, io::BufReader, net::TcpListener, sync::mpsc};

    #[test]
    fn fast_exit_probe_completes_while_an_earlier_request_is_waiting() {
        let proxy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = proxy.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        let (ready, notified) = mpsc::channel();
        let launch = || {
            let ready = ready.clone();
            start_exit_ip_check(port, tx.clone(), move || ready.send(()).unwrap());
        };
        let accept_request = || {
            let (socket, _) = proxy.accept().unwrap();
            let mut reader = BufReader::new(socket);
            let mut head = String::new();
            while !head.ends_with("\r\n\r\n") {
                assert_ne!(reader.read_line(&mut head).unwrap(), 0);
            }
            reader.into_inner()
        };
        let respond = |mut socket: TcpStream, ip| {
            write!(socket, "HTTP/1.0 200 OK\r\n\r\n{ip}").unwrap();
        };
        launch();
        let slow = accept_request();
        launch();
        let fast = accept_request();
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        respond(fast, "127.0.0.2");
        notified.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(rx.try_recv().unwrap().unwrap(), "127.0.0.2");
        respond(slow, "127.0.0.1");
        notified.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(rx.try_recv().unwrap().unwrap(), "127.0.0.1");
    }
}
