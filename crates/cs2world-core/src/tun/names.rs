//! Routing hints are scoped to an internal SOCKS connection. The outbound still dials the original IP.
use dns_parser::{Packet, QueryType, RData};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};
use tls_parser::{TlsExtension, TlsMessage, TlsMessageHandshake};
use tokio::io::{AsyncRead, AsyncReadExt};

#[derive(Default)]
pub(super) struct Names {
    // The original IP sender distinguishes separate clients of the TUN.
    ips: HashMap<(IpAddr, IpAddr), (Option<String>, Instant)>,
}
pub(super) type Cache = Arc<Mutex<Names>>;
pub(super) type Pending = HashMap<(SocketAddr, u16), (String, QueryType, Instant)>;

fn valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.trim_end_matches('.').split('.').all(|s| {
            !s.is_empty()
                && s.len() <= 63
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}
fn dns(data: &[u8]) -> Option<Packet<'_>> {
    let h = dns_parser::Header::parse(data).ok()?;
    if h.questions != 1
        || u32::from(h.answers) + u32::from(h.nameservers) + u32::from(h.additional) > 128
    {
        return None;
    }
    Packet::parse(data).ok()
}
pub(super) fn query(requests: &mut Pending, resolver: SocketAddr, data: &[u8]) {
    if resolver.port() != 53 {
        return;
    }
    let Some(p) = dns(data) else { return };
    let q = &p.questions[0];
    if !p.header.query
        || !matches!(q.qtype, QueryType::A | QueryType::AAAA)
        || q.qclass != dns_parser::QueryClass::IN
    {
        return;
    }
    let name = q.qname.to_string().to_ascii_lowercase();
    if !valid(&name) {
        return;
    }
    let now = Instant::now();
    requests.retain(|_, v| v.2 > now);
    if requests.len() < 256 {
        requests.insert(
            (resolver, p.header.id),
            (name, q.qtype, now + Duration::from_secs(10)),
        );
    }
}
pub(super) fn answer(
    cache: &Cache,
    requests: &mut Pending,
    src: IpAddr,
    resolver: SocketAddr,
    data: &[u8],
) {
    if resolver.port() != 53 {
        return;
    }
    let Some(p) = dns(data) else { return };
    if p.header.query
        || p.header.truncated
        || p.header.response_code != dns_parser::ResponseCode::NoError
    {
        return;
    }
    let Some((requested, ty, expiry)) = requests.get(&(resolver, p.header.id)) else {
        return;
    };
    let q = &p.questions[0];
    let now = Instant::now();
    if *expiry <= now
        || q.qclass != dns_parser::QueryClass::IN
        || q.qtype != *ty
        || !q.qname.to_string().eq_ignore_ascii_case(requested)
    {
        return;
    }
    let name = requested.clone();
    let ty = *ty;
    requests.remove(&(resolver, p.header.id));
    let mut canonical = name.clone();
    let mut ttl = 3600;
    for _ in 0..8 {
        let alias = p.answers.iter().find_map(|rr| match &rr.data {
            RData::CNAME(c)
                if rr.cls == dns_parser::Class::IN
                    && rr.name.to_string().eq_ignore_ascii_case(&canonical) =>
            {
                Some((c.0.to_string(), rr.ttl))
            }
            _ => None,
        });
        let Some((next, t)) = alias else { break };
        canonical = next;
        ttl = ttl.min(t);
    }
    let mut cache = cache.lock().unwrap();
    if cache.ips.len() >= 4096 {
        cache.ips.retain(|_, v| v.1 > now);
    }
    for rr in &p.answers {
        if rr.cls != dns_parser::Class::IN || !rr.name.to_string().eq_ignore_ascii_case(&canonical)
        {
            continue;
        }
        let ip = match (&rr.data, ty) {
            (RData::A(a), QueryType::A) => IpAddr::V4(a.0),
            (RData::AAAA(a), QueryType::AAAA) => IpAddr::V6(a.0),
            _ => continue,
        };
        let key = (src, ip);
        let until = now + Duration::from_secs(u64::from(ttl.min(rr.ttl)));
        if let Some((known, t)) = cache.ips.get_mut(&key) {
            if *t <= now {
                *known = Some(name.clone());
                *t = until;
            } else if known.as_deref() == Some(&name) {
                *t = until;
            } else {
                *known = None;
                *t = (*t).max(until);
            }
        } else if cache.ips.len() < 4096 {
            cache.ips.insert(key, (Some(name.clone()), until));
        }
    }
}

#[derive(Clone)]
pub(super) struct Scope {
    pub src: IpAddr,
    pub name: Option<String>,
    pub cache: Cache,
}
type Channel = (SocketAddr, SocketAddr);
static SCOPES: LazyLock<Mutex<HashMap<Channel, Scope>>> = LazyLock::new(Default::default);
pub(super) struct Lease(Channel);
impl Drop for Lease {
    fn drop(&mut self) {
        SCOPES.lock().unwrap().remove(&self.0);
    }
}
impl Scope {
    pub(super) fn register(self, key: Channel) -> Lease {
        SCOPES.lock().unwrap().insert(key, self);
        Lease(key)
    }
}
pub(crate) fn lookup(local: SocketAddr, peer: SocketAddr, host: &str) -> Option<String> {
    let ip = host.parse::<IpAddr>().ok()?;
    let scope = SCOPES.lock().unwrap().get(&(local, peer))?.clone();
    scope.name.or_else(|| {
        let cache = scope.cache.lock().unwrap();
        let (name, t) = cache.ips.get(&(scope.src, ip))?;
        if *t > Instant::now() {
            name.clone()
        } else {
            None
        }
    })
}
fn hostname(data: &[u8], port: u16) -> Option<String> {
    if port == 443 {
        let (_, tls) = tls_parser::parse_tls_plaintext(data).ok()?;
        let TlsMessage::Handshake(TlsMessageHandshake::ClientHello(h)) = tls.msg.first()? else {
            return None;
        };
        for ext in tls_parser::parse_tls_extensions(h.ext?).ok()?.1 {
            if let TlsExtension::SNI(names) = ext {
                for (kind, bytes) in names {
                    let s = std::str::from_utf8(bytes).ok()?;
                    if kind == tls_parser::SNIType::HostName && valid(s) {
                        return Some(s.to_ascii_lowercase());
                    }
                }
            }
        }
    } else {
        let mut headers = [httparse::EMPTY_HEADER; 64];
        let mut req = httparse::Request::new(&mut headers);
        if req.parse(data).ok()?.is_complete() {
            let host = req
                .headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case("host"))?;
            let authority = std::str::from_utf8(host.value)
                .ok()?
                .trim()
                .parse::<hyper::http::uri::Authority>()
                .ok()?;
            if valid(authority.host()) {
                return Some(authority.host().to_ascii_lowercase());
            }
        }
    }
    None
}
pub(super) async fn sniff(
    socket: &mut (impl AsyncRead + Unpin),
    port: u16,
) -> (Vec<u8>, Option<String>) {
    let mut prefix = Vec::new();
    if port != 80 && port != 443 {
        return (prefix, None);
    }
    let mut buf = [0; 2048];
    let _ = tokio::time::timeout(Duration::from_millis(250), async {
        while prefix.len() < 16384 {
            let Ok(n) = socket.read(&mut buf).await else {
                break;
            };
            if n == 0 {
                break;
            }
            prefix.extend_from_slice(&buf[..n]);
            if port == 443 {
                if prefix[0] != 22
                    || (prefix.len() >= 5
                        && prefix.len()
                            >= 5 + usize::from(u16::from_be_bytes([prefix[3], prefix[4]])))
                {
                    break;
                }
            } else if prefix.windows(4).any(|s| s == b"\r\n\r\n") {
                break;
            }
        }
    })
    .await;
    let name = hostname(&prefix, port);
    (prefix, name)
}

#[cfg(test)]
mod tests;
