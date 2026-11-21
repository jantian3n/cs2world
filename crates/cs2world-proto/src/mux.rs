//! Incremental smux v1 and sing UoT records. A short prefix returns Ok(None).
use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use core::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    str,
};

pub const MAX_FRAME: usize = 32768;
pub const UOT_MAGIC: &str = "sp.v2.udp-over-tcp.arpa";
pub const UOT_LEGACY_MAGIC: &str = "sp.udp-over-tcp.arpa";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    InvalidHeader,
    InvalidAddress,
    TooLong,
}
impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidHeader => "invalid frame header",
            Self::InvalidAddress => "invalid destination address",
            Self::TooLong => "wire length exceeded",
        })
    }
}
impl core::error::Error for WireError {}

pub type Decoded<T> = Result<Option<(T, usize)>, WireError>;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Syn = 0,
    Fin = 1,
    Push = 2,
    Nop = 3,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    pub cmd: Command,
    pub sid: u32,
    pub payload: &'a [u8],
}

pub fn encode_frame(cmd: Command, sid: u32, payload: &[u8]) -> Result<Vec<u8>, WireError> {
    if cmd != Command::Push && !payload.is_empty() {
        return Err(WireError::InvalidHeader);
    }
    if payload.len() > MAX_FRAME {
        return Err(WireError::TooLong);
    }
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&[1, cmd as u8]);
    out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    out.extend_from_slice(&sid.to_le_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

pub fn decode_frame(buf: &[u8]) -> Decoded<Frame<'_>> {
    let Some(head) = buf.get(..8) else {
        return Ok(None);
    };
    if head[0] != 1 {
        return Err(WireError::InvalidHeader);
    }
    let cmd = match head[1] {
        0 => Command::Syn,
        1 => Command::Fin,
        2 => Command::Push,
        3 => Command::Nop,
        _ => return Err(WireError::InvalidHeader),
    };
    let len = u16::from_le_bytes([head[2], head[3]]) as usize;
    if cmd != Command::Push && len != 0 {
        return Err(WireError::InvalidHeader);
    }
    let Some(payload) = buf.get(8..8 + len) else {
        return Ok(None);
    };
    let sid = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
    Ok(Some((Frame { cmd, sid, payload }, 8 + len)))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Target {
    pub host: String,
    pub port: u16,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Datagram<'a> {
    pub addr: Option<Target>,
    pub payload: &'a [u8],
}

/// The stream prefix and v2 request use SOCKS tags. Only per-datagram addresses use UoT tags.
pub fn encode_addr(target: &Target, uot: bool) -> Result<Vec<u8>, WireError> {
    let mut out = Vec::new();
    match target.host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            out.push(if uot { 0 } else { 1 });
            out.extend_from_slice(&ip.octets());
        }
        Ok(IpAddr::V6(ip)) => {
            out.push(if uot { 1 } else { 4 });
            out.extend_from_slice(&ip.octets());
        }
        Err(_) => {
            let domain = target.host.as_bytes();
            if domain.is_empty() {
                return Err(WireError::InvalidAddress);
            }
            let len = u8::try_from(domain.len()).map_err(|_| WireError::TooLong)?;
            out.extend_from_slice(&[if uot { 2 } else { 3 }, len]);
            out.extend_from_slice(domain);
        }
    }
    out.extend_from_slice(&target.port.to_be_bytes());
    Ok(out)
}

pub fn decode_addr(buf: &[u8], uot: bool) -> Decoded<Target> {
    let Some(&tag) = buf.first() else {
        return Ok(None);
    };
    let (start, len) = match (uot, tag) {
        (false, 1) | (true, 0) => (1, 4),
        (false, 4) | (true, 1) => (1, 16),
        (false, 3) | (true, 2) => {
            let Some(&len) = buf.get(1) else {
                return Ok(None);
            };
            if len == 0 {
                return Err(WireError::InvalidAddress);
            }
            (2, usize::from(len))
        }
        _ => return Err(WireError::InvalidAddress),
    };
    let end = start + len;
    if buf.len() < end + 2 {
        return Ok(None);
    }
    let raw = &buf[start..end];
    let host = if start == 2 {
        str::from_utf8(raw)
            .map_err(|_| WireError::InvalidAddress)?
            .to_string()
    } else if len == 4 {
        Ipv4Addr::new(raw[0], raw[1], raw[2], raw[3]).to_string()
    } else {
        let mut octets = [0; 16];
        octets.copy_from_slice(raw);
        Ipv6Addr::from(octets).to_string()
    };
    let port = u16::from_be_bytes([buf[end], buf[end + 1]]);
    Ok(Some((Target { host, port }, end + 2)))
}

pub fn encode_uot_request(connected: bool, target: &Target) -> Result<Vec<u8>, WireError> {
    let mut out = alloc::vec![u8::from(connected)];
    out.extend_from_slice(&encode_addr(target, false)?);
    Ok(out)
}
pub fn decode_uot_request(buf: &[u8]) -> Decoded<(bool, Target)> {
    let Some((&flag, rest)) = buf.split_first() else {
        return Ok(None);
    };
    // encoding/binary reads every nonzero wire byte as true.
    Ok(decode_addr(rest, false)?.map(|(target, n)| ((flag != 0, target), n + 1)))
}
pub fn encode_uot_packet(addr: Option<&Target>, data: &[u8]) -> Result<Vec<u8>, WireError> {
    let len = u16::try_from(data.len()).map_err(|_| WireError::TooLong)?;
    let mut out = match addr {
        Some(target) => encode_addr(target, true)?,
        None => Vec::new(),
    };
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(data);
    Ok(out)
}
pub fn decode_uot_packet(buf: &[u8], connected: bool) -> Decoded<Datagram<'_>> {
    let (addr, offset) = if connected {
        (None, 0)
    } else {
        let Some((target, n)) = decode_addr(buf, true)? else {
            return Ok(None);
        };
        (Some(target), n)
    };
    let Some(bytes) = buf.get(offset..offset + 2) else {
        return Ok(None);
    };
    let size = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
    let end = offset + 2 + size;
    Ok(buf
        .get(offset + 2..end)
        .map(|payload| (Datagram { addr, payload }, end)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_three_and_incremental_uot() {
        let f = encode_frame(Command::Push, 3, b"abc").unwrap();
        assert_eq!(&f, b"\x01\x02\x03\0\x03\0\0\0abc");
        for end in 0..f.len() {
            assert!(decode_frame(&f[..end]).unwrap().is_none());
        }
        assert_eq!(decode_frame(&f).unwrap().unwrap().0.payload, b"abc");
        assert!(decode_frame(&[1, 1, 1, 0, 3, 0, 0, 0]).is_err());
        let target = Target {
            host: "192.0.2.1".into(),
            port: 53,
        };
        assert_eq!(
            encode_addr(&target, false).unwrap(),
            [1, 192, 0, 2, 1, 0, 53]
        );
        let rec = encode_uot_packet(Some(&target), b"abc").unwrap();
        assert_eq!(rec, [0, 192, 0, 2, 1, 0, 53, 0, 3, 97, 98, 99]);
        for end in 0..rec.len() {
            assert!(decode_uot_packet(&rec[..end], false).unwrap().is_none());
        }
        let (dg, used) = decode_uot_packet(&rec, false).unwrap().unwrap();
        assert_eq!(dg.addr, Some(target));
        assert_eq!(used, rec.len());
    }

    #[test]
    fn both_address_families_and_magic() {
        for name in ["::1", "a.test", UOT_MAGIC, UOT_LEGACY_MAGIC] {
            let target = Target {
                host: name.into(),
                port: 0x1234,
            };
            let req = encode_uot_request(false, &target).unwrap();
            assert_eq!(
                decode_uot_request(&req).unwrap(),
                Some(((false, target.clone()), req.len()))
            );
            let data = encode_uot_packet(Some(&target), b"").unwrap();
            assert_eq!(
                decode_uot_packet(&data, false).unwrap().unwrap().0.addr,
                Some(target)
            );
        }
        assert!(encode_uot_packet(None, &alloc::vec![0;65536]).is_err());
        assert_eq!(encode_uot_packet(None, b"").unwrap(), [0, 0]);
    }
}
