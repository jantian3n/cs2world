//! Share link profiles. Python preserves signed query values; the Windows GUI accepts u32.
use alloc::{
    collections::BTreeMap,
    format,
    string::{String, ToString},
    vec::Vec,
};
use base64::{
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD},
    Engine,
};
use core::{fmt, net::Ipv6Addr};
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde::{Deserialize, Serialize};
use url::Url;

pub const PREFIX: &str = "cs2world://";
const PYTHON_ESCAPE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Dialect {
    #[default]
    Python,
    Win32,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Kcp {
    pub data_shards: i64,
    pub parity_shards: i64,
    pub mtu: i64,
    pub snd_wnd: i64,
    pub rcv_wnd: i64,
}
impl Default for Kcp {
    fn default() -> Self {
        Self {
            data_shards: 10,
            parity_shards: 3,
            mtu: 1180,
            snd_wnd: 1024,
            rcv_wnd: 1024,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct LinkShaper {
    pub enabled: bool,
    pub tick_rate_hz: i64,
    pub idle_sleep_seconds: i64,
    pub combat_mean_bytes: i64,
    pub fastdl_chunk_size: i64,
}
impl Default for LinkShaper {
    fn default() -> Self {
        Self {
            enabled: true,
            tick_rate_hz: 64,
            idle_sleep_seconds: 600,
            combat_mean_bytes: 180,
            fastdl_chunk_size: 1248,
        }
    }
}
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Node {
    pub name: String,
    pub server: String,
    pub server_port: u16,
    pub psk: String,
    #[serde(default)]
    pub kcp: Kcp,
    #[serde(default)]
    pub shaper: LinkShaper,
}
impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Node")
            .field("name", &self.name)
            .field("server", &self.server)
            .field("server_port", &self.server_port)
            .field("kcp", &self.kcp)
            .field("shaper", &self.shaper)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Error {
    InvalidLink,
    InvalidPort,
    MissingCredentials,
    LossyPsk,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidLink => "invalid cs2world share link",
            Self::InvalidPort => "invalid server port",
            Self::MissingCredentials => "missing server or PSK",
            Self::LossyPsk => "PSK has surrounding whitespace; use JSON to preserve it",
        })
    }
}
impl core::error::Error for Error {}
fn decode(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().trim().to_string()
}

struct Parts {
    psk: String,
    host: String,
    port: u16,
    query: String,
    name: String,
}
impl Parts {
    fn windows(input: &str) -> Result<Self, Error> {
        let u = Url::parse(input).map_err(|_| Error::InvalidLink)?;
        if !u.scheme().eq_ignore_ascii_case("cs2world") {
            return Err(Error::InvalidLink);
        }
        let port = u.port().unwrap_or(27015);
        if port == 0 {
            return Err(Error::InvalidPort);
        }
        Ok(Self {
            psk: decode(u.username()),
            host: u
                .host_str()
                .unwrap_or("")
                .trim_matches(['[', ']'])
                .trim()
                .into(),
            port,
            query: u.query().unwrap_or("").into(),
            name: decode(u.fragment().unwrap_or("")),
        })
    }
    fn python(input: &str) -> Result<Self, Error> {
        let (scheme, remainder) = input.split_once("://").ok_or(Error::InvalidLink)?;
        if !scheme.eq_ignore_ascii_case("cs2world") {
            return Err(Error::InvalidLink);
        }
        let (prefix, fragment) = remainder.split_once('#').unwrap_or((remainder, ""));
        let (path, query) = prefix.split_once('?').unwrap_or((prefix, ""));
        let authority = path.split('/').next().unwrap_or("");
        let (userinfo, host_port) = authority
            .rsplit_once('@')
            .ok_or(Error::MissingCredentials)?;
        let psk = decode(userinfo.split(':').next().unwrap_or(""));
        let (host, raw_port) = if let Some(bracket) = host_port.strip_prefix('[') {
            let (host, trailer) = bracket.split_once(']').ok_or(Error::InvalidLink)?;
            let ip = host.split('%').next().unwrap_or("");
            if ip.parse::<Ipv6Addr>().is_err() {
                return Err(Error::InvalidLink);
            }
            let port = if trailer.is_empty() {
                ""
            } else {
                trailer.strip_prefix(':').ok_or(Error::InvalidPort)?
            };
            (host, port)
        } else {
            host_port.split_once(':').unwrap_or((host_port, ""))
        };
        let port = if raw_port.is_empty() {
            27015
        } else {
            if !raw_port.bytes().all(|c| c.is_ascii_digit()) {
                return Err(Error::InvalidPort);
            }
            let v = raw_port.parse::<u16>().map_err(|_| Error::InvalidPort)?;
            if v == 0 {
                27015
            } else {
                v
            }
        };
        // urllib lowercases the name before the IPv6 scope separator; the scope is case-sensitive.
        let host = match host.split_once('%') {
            Some((ip, zone)) => format!("{}%{zone}", ip.to_lowercase()),
            None => host.to_lowercase(),
        };
        Ok(Self {
            psk,
            host: host.trim().into(),
            port,
            query: query.into(),
            name: decode(fragment),
        })
    }
}

fn python_integer(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.contains('_') {
        let magnitude = s.strip_prefix(['+', '-']).unwrap_or(s);
        if magnitude
            .split('_')
            .any(|part| part.is_empty() || !part.bytes().all(|c| c.is_ascii_digit()))
        {
            return None;
        }
        s.replace('_', "").parse().ok()
    } else {
        s.parse().ok()
    }
}
pub fn parse(input: &str, dialect: Dialect) -> Result<Node, Error> {
    let input = input.trim();
    let parts = match dialect {
        Dialect::Win32 => Parts::windows(input)?,
        Dialect::Python => Parts::python(input)?,
    };
    if parts.host.is_empty() || parts.psk.is_empty() {
        return Err(Error::MissingCredentials);
    }
    let mut q = BTreeMap::new();
    for (key, value) in url::form_urlencoded::parse(parts.query.as_bytes()) {
        match dialect {
            Dialect::Win32 => {
                q.insert(key.to_ascii_lowercase(), value);
            }
            Dialect::Python => {
                if !value.is_empty() {
                    q.entry(key.into_owned()).or_insert(value);
                }
            }
        }
    }
    let num = |keys: &[&str], default: i64, min: u32| {
        keys.iter()
            .find_map(|key| {
                let s = q.get(*key)?;
                match dialect {
                    Dialect::Python => python_integer(s),
                    Dialect::Win32 => s.parse::<u32>().ok().filter(|&v| v >= min).map(i64::from),
                }
            })
            .unwrap_or(default)
    };
    let enabled = ["shaper", "shaper_enabled"]
        .iter()
        .find_map(|k| match q.get(*k)?.trim().to_ascii_lowercase().as_str() {
            "1" | "on" | "true" | "yes" => Some(true),
            "0" | "off" | "false" | "no" => Some(false),
            _ => None,
        })
        .unwrap_or(true);
    let k = Kcp::default();
    let s = LinkShaper::default();
    let kcp = Kcp {
        data_shards: num(&["ds", "data_shards"], k.data_shards, 0),
        parity_shards: num(&["ps", "parity_shards"], k.parity_shards, 0),
        mtu: num(&["mtu"], k.mtu, 256),
        snd_wnd: num(&["sw", "snd_wnd"], k.snd_wnd, 16),
        rcv_wnd: num(&["rw", "rcv_wnd"], k.rcv_wnd, 16),
    };
    let shaper = LinkShaper {
        enabled,
        tick_rate_hz: num(&["tick", "tick_hz", "tick_rate_hz"], s.tick_rate_hz, 1),
        idle_sleep_seconds: num(
            &["sleep", "idle_sleep", "idle_sleep_seconds"],
            s.idle_sleep_seconds,
            1,
        ),
        combat_mean_bytes: num(
            &["combat", "combat_bytes", "combat_mean_bytes"],
            s.combat_mean_bytes,
            64,
        ),
        fastdl_chunk_size: num(
            &["fastdl", "fastdl_chunk", "fastdl_chunk_size"],
            s.fastdl_chunk_size,
            256,
        ),
    };
    let name = if parts.name.is_empty() {
        format!("cs2world_{}:{}", parts.host, parts.port)
    } else {
        parts.name
    };
    Ok(Node {
        name,
        server: parts.host,
        server_port: parts.port,
        psk: parts.psk,
        kcp,
        shaper,
    })
}

pub fn encode(node: &Node, dialect: Dialect) -> Result<String, Error> {
    if node.psk.trim() != node.psk {
        return Err(Error::LossyPsk);
    }
    if node.psk.is_empty() || node.server.is_empty() {
        return Err(Error::MissingCredentials);
    }
    if node.server_port == 0 {
        return Err(Error::InvalidPort);
    }
    let (k, s) = (&node.kcp, &node.shaper);
    let (kd, sd) = (Kcp::default(), LinkShaper::default());
    let mut params = Vec::new();
    for (key, v, def) in [
        ("ds", k.data_shards, kd.data_shards),
        ("ps", k.parity_shards, kd.parity_shards),
        ("mtu", k.mtu, kd.mtu),
        ("sw", k.snd_wnd, kd.snd_wnd),
        ("rw", k.rcv_wnd, kd.rcv_wnd),
        ("shaper", i64::from(s.enabled), i64::from(sd.enabled)),
        ("tick", s.tick_rate_hz, sd.tick_rate_hz),
        ("sleep", s.idle_sleep_seconds, sd.idle_sleep_seconds),
        ("combat", s.combat_mean_bytes, sd.combat_mean_bytes),
        ("fastdl", s.fastdl_chunk_size, sd.fastdl_chunk_size),
    ] {
        if v != def {
            params.push(format!("{key}={v}"));
        }
    }
    let encoder = match dialect {
        Dialect::Win32 => NON_ALPHANUMERIC,
        Dialect::Python => PYTHON_ESCAPE,
    };
    let host = if node.server.contains(':') && !node.server.starts_with('[') {
        format!("[{}]", node.server)
    } else {
        node.server.clone()
    };
    let mut out = format!(
        "{PREFIX}{}@{host}:{}",
        utf8_percent_encode(&node.psk, encoder),
        node.server_port
    );
    if !params.is_empty() {
        out.push('?');
        out.push_str(&params.join("&"));
    }
    let name = match dialect {
        Dialect::Win32 => node.name.trim().to_string(),
        Dialect::Python if node.name.is_empty() => {
            format!("cs2world_{}:{}", node.server, node.server_port)
        }
        Dialect::Python => node.name.clone(),
    };
    if !name.is_empty() {
        out.push('#');
        out.push_str(&utf8_percent_encode(&name, encoder).to_string());
    }
    Ok(out)
}

#[derive(Debug, Default)]
pub struct Batch {
    pub nodes: Vec<Node>,
    pub failed: usize,
}
pub fn parse_batch(raw: &str) -> Batch {
    let raw = raw.trim();
    let mut decoded = None;
    if !raw.to_ascii_lowercase().contains(PREFIX) {
        let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.len() >= 12 {
            decoded = [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD]
                .iter()
                .find_map(|engine| {
                    let s = String::from_utf8(engine.decode(compact.as_bytes()).ok()?).ok()?;
                    s.to_ascii_lowercase().contains(PREFIX).then_some(s)
                });
        }
    }
    let mut batch = Batch::default();
    for line in decoded
        .as_deref()
        .unwrap_or(raw)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        match parse(line, Dialect::Win32) {
            Ok(node) => batch.nodes.push(node),
            Err(_) => batch.failed += 1,
        }
    }
    batch
}

#[cfg(test)]
mod tests {
    use super::*;
    const PY: &str = "cs2world://p%40ss-._~@[2001:db8::1]:27016?ds=4&ps=1&shaper=0#HK%20Lab";
    const WIN: &str =
        "cs2world://p%40ss%2D%2E%5F%7E@[2001:db8::1]:27016?ds=4&ps=1&shaper=0#HK%20Lab";
    #[test]
    fn cross_reads_legacy_exports_exactly() {
        for dialect in [Dialect::Python, Dialect::Win32] {
            for input in [PY, WIN] {
                let n = parse(input, dialect).unwrap();
                assert_eq!(n.psk, "p@ss-._~");
                assert_eq!(n.name, "HK Lab");
                assert_eq!(encode(&n, Dialect::Python).unwrap(), PY);
                assert_eq!(encode(&n, Dialect::Win32).unwrap(), WIN);
            }
        }
    }
    #[test]
    fn dialect_specific_inputs() {
        let src =
            "cs2world://a%2Bb%2523@Foo.Example?DS=2&ds=&ds=3&ds=4&sw=-2&RCV_WND=42&tick_hz=89#A+B";
        let w = parse(src, Dialect::Win32).unwrap();
        let p = parse(src, Dialect::Python).unwrap();
        assert_eq!((&*w.psk, &*w.name), ("a+b%23", "A+B"));
        assert_eq!(
            (w.kcp.data_shards, w.kcp.snd_wnd, w.kcp.rcv_wnd),
            (4, 1024, 42)
        );
        assert_eq!(
            (p.kcp.data_shards, p.kcp.snd_wnd, p.kcp.rcv_wnd),
            (3, -2, 1024)
        );
        assert_eq!(p.server, "foo.example");
        assert!(parse("cs2world://key@host:0", Dialect::Win32).is_err());
        assert_eq!(
            parse("cs2world://key@host:0", Dialect::Python)
                .unwrap()
                .server_port,
            27015
        );
        assert_eq!(w.shaper.tick_rate_hz, 89);
        let n = parse(
            "cs2world://%E5%AF%86%E9%92%A5@host#%E9%A6%99%E6%B8%AF",
            Dialect::Win32,
        )
        .unwrap();
        assert_eq!((&*n.psk, &*n.name), ("密钥", "香港"));
    }
    #[test]
    fn batch_forms_and_lossy_exports() {
        let raw = format!("{PY}\r\ncs2world://@invalid\n {WIN}\n");
        for input in [
            raw.clone(),
            STANDARD.encode(&raw),
            STANDARD_NO_PAD.encode(&raw),
            URL_SAFE.encode(&raw),
            URL_SAFE_NO_PAD.encode(&raw),
        ] {
            let b = parse_batch(&input);
            assert_eq!((b.nodes.len(), b.failed), (2, 1));
        }
        let mut n = parse("cs2world://key@host", Dialect::Python).unwrap();
        assert_eq!(n.name, "cs2world_host:27015");
        n.kcp.data_shards = 0;
        n.kcp.parity_shards = 0;
        n.shaper.enabled = false;
        assert!(encode(&n, Dialect::Python)
            .unwrap()
            .contains("ds=0&ps=0&shaper=0"));
        n.psk = " secret ".into();
        assert_eq!(encode(&n, Dialect::Win32), Err(Error::LossyPsk));
    }
}
