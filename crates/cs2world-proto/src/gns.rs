//! CS2World v0.0.1 authenticated datagrams. All clock and entropy inputs come from the caller.
use aes_gcm::{aead::AeadInPlace, Aes256Gcm, KeyInit, Nonce};
use alloc::{vec, vec::Vec};
use core::fmt;
use hmac::{Hmac, Mac};
use rand_core::CryptoRngCore;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

pub const CONNECT_REQUEST: u8 = 0x01;
pub const CONNECTION_CLOSED: u8 = 0x02;
pub const CONNECT_OK: u8 = 0x03;
pub const DATA: u8 = 0x82;
/// Real Valve GameNetworkingSockets (SteamNetSockets) UDP message lead bytes.
pub const VALVE_CHALLENGE_REQUEST: u8 = 0x20;
pub const VALVE_CHALLENGE_REPLY: u8 = 0x21;
pub const VALVE_CONNECT_REQUEST: u8 = 0x22;
pub const VALVE_CONNECT_OK: u8 = 0x23;
pub const VALVE_CONNECTION_CLOSED: u8 = 0x24;
pub const VALVE_NO_CONNECTION: u8 = 0x25;
/// Real CS2 Valve SDR empirical UDP payload ceiling observed in live match captures.
pub const VALVE_SDR_MAX_UDP: usize = 1257;
pub const IDLE: u8 = 0;
pub const COMBAT: u8 = 1;
pub const FAST_DL: u8 = 2;
pub const OVERHEAD: usize = 28;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Handshake,
    Entropy,
    Authentication,
    Replay,
    Length,
    SequenceExhausted,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Handshake => "invalid handshake",
            Self::Entropy => "secure random source unavailable",
            Self::Authentication => "invalid authenticated datagram",
            Self::Replay => "replayed or out-of-window datagram",
            Self::Length => "invalid datagram length",
            Self::SequenceExhausted => "session sequence exhausted",
        })
    }
}
impl core::error::Error for Error {}

fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    for p in parts {
        mac.update(p);
    }
    mac.finalize().into_bytes().into()
}

fn labelled_key(label: &[u8], psk: &[u8]) -> Zeroizing<Vec<u8>> {
    let mut k = Zeroizing::new(Vec::with_capacity(label.len() + psk.len()));
    k.extend_from_slice(label);
    k.extend_from_slice(psk);
    k
}

fn random(rng: &mut impl CryptoRngCore, buf: &mut [u8]) -> Result<(), Error> {
    rng.try_fill_bytes(buf).map_err(|_| Error::Entropy)
}

fn private_key(rng: &mut impl CryptoRngCore) -> Result<StaticSecret, Error> {
    let mut bytes = Zeroizing::new([0; 32]);
    random(rng, bytes.as_mut())?;
    Ok(StaticSecret::from(*bytes))
}

fn word4(bytes: &[u8]) -> [u8; 4] {
    bytes.try_into().expect("four-byte field")
}

pub struct ClientHandshake {
    private: StaticSecret,
    tmp_id: [u8; 4],
    psk: Zeroizing<Vec<u8>>,
}

pub fn build_request(
    psk: &str,
    unix: i64,
    rng: &mut impl CryptoRngCore,
) -> Result<(Vec<u8>, ClientHandshake), Error> {
    let private = private_key(rng)?;
    let mut req = vec![0; 148];
    req[0] = CONNECT_REQUEST;
    random(rng, &mut req[1..5])?;
    req[5..37].copy_from_slice(PublicKey::from(&private).as_bytes());
    req[37..45].copy_from_slice(&unix.to_be_bytes());
    let mac = hmac(
        &labelled_key(b"cs2world-hs-req:", psk.as_bytes()),
        &[&req[..45]],
    );
    req[45..61].copy_from_slice(&mac[..16]);
    random(rng, &mut req[61..])?;
    let state = ClientHandshake {
        private,
        tmp_id: word4(&req[1..5]),
        psk: Zeroizing::new(psk.as_bytes().to_vec()),
    };
    Ok((req, state))
}

pub fn accept_request(
    req: &[u8],
    psk: &str,
    unix: i64,
    rng: &mut impl CryptoRngCore,
) -> Result<(Vec<u8>, Codec), Error> {
    if req.len() < 61 || req[0] != CONNECT_REQUEST {
        return Err(Error::Handshake);
    }
    let mac = hmac(
        &labelled_key(b"cs2world-hs-req:", psk.as_bytes()),
        &[&req[..45]],
    );
    if !bool::from(mac[..16].ct_eq(&req[45..61])) {
        return Err(Error::Handshake);
    }
    let timestamp = i64::from_be_bytes(req[37..45].try_into().expect("timestamp"));
    if unix.abs_diff(timestamp) > 60 {
        return Err(Error::Handshake);
    }
    let client_pub = PublicKey::from(<[u8; 32]>::try_from(&req[5..37]).expect("public key"));
    let private = private_key(rng)?;
    let server_pub = PublicKey::from(&private);
    let mut resp = vec![0; 92];
    resp[0] = CONNECT_OK;
    random(rng, &mut resp[1..5])?;
    let id = u32::from_le_bytes(word4(&resp[1..5])).max(1);
    resp[1..5].copy_from_slice(&id.to_le_bytes());
    resp[5..37].copy_from_slice(server_pub.as_bytes());
    let mac = hmac(
        &labelled_key(b"cs2world-hs-ok:", psk.as_bytes()),
        &[&resp[..37], &req[1..5], client_pub.as_bytes()],
    );
    resp[37..53].copy_from_slice(&mac[..16]);
    let codec = Codec::derive(&private, &client_pub, psk.as_bytes(), id, true)?;
    random(rng, &mut resp[53..])?;
    Ok((resp, codec))
}

impl ClientHandshake {
    /// The caller retains this state until a valid response arrives or all retries expire.
    pub fn complete(&self, response: &[u8]) -> Result<Codec, Error> {
        if response.len() < 53 || response[0] != CONNECT_OK {
            return Err(Error::Handshake);
        }
        let client_pub = PublicKey::from(&self.private);
        let mac = hmac(
            &labelled_key(b"cs2world-hs-ok:", &self.psk),
            &[&response[..37], &self.tmp_id, client_pub.as_bytes()],
        );
        if !bool::from(mac[..16].ct_eq(&response[37..53])) {
            return Err(Error::Handshake);
        }
        let other = PublicKey::from(<[u8; 32]>::try_from(&response[5..37]).expect("public key"));
        Codec::derive(
            &self.private,
            &other,
            &self.psk,
            u32::from_le_bytes(word4(&response[1..5])),
            false,
        )
    }
}

#[derive(Default)]
struct ReplayWindow {
    highest: u32,
    bitmap: u64,
}
impl ReplayWindow {
    fn accept(&mut self, seq: u32) -> bool {
        if seq == 0 {
            return false;
        }
        if seq > self.highest {
            self.bitmap = self.bitmap.checked_shl(seq - self.highest).unwrap_or(0) | 1;
            self.highest = seq;
            return true;
        }
        let Some(mask) = 1u64.checked_shl(self.highest - seq) else {
            return false;
        };
        if self.bitmap & mask != 0 {
            return false;
        }
        self.bitmap |= mask;
        true
    }
}
pub struct Codec {
    cipher: Aes256Gcm,
    id: u32,
    tx: [u8; 4],
    rx: [u8; 4],
    seq: u32,
    replay: ReplayWindow,
}
impl Codec {
    fn derive(
        private: &StaticSecret,
        other: &PublicKey,
        psk: &[u8],
        id: u32,
        server: bool,
    ) -> Result<Self, Error> {
        let shared = private.diffie_hellman(other);
        if !shared.was_contributory() {
            return Err(Error::Handshake);
        }
        let own = PublicKey::from(private);
        let (client_pub, server_pub) = if server { (other, &own) } else { (&own, other) };
        let prk = Zeroizing::new(hmac(
            &labelled_key(b"cs2world-pfs-v0.0.1:", psk),
            &[
                shared.as_bytes(),
                client_pub.as_bytes(),
                server_pub.as_bytes(),
            ],
        ));
        let aes_key = Zeroizing::new(hmac(prk.as_ref(), &[b"aes-256-gcm-key"]));
        let cipher = Aes256Gcm::new_from_slice(aes_key.as_ref()).expect("32 byte AES key");
        let c2s = word4(&hmac(prk.as_ref(), &[b"c2s-nonce-salt"])[..4]);
        let s2c = word4(&hmac(prk.as_ref(), &[b"s2c-nonce-salt"])[..4]);
        let (tx, rx) = if server { (s2c, c2s) } else { (c2s, s2c) };
        Ok(Self {
            cipher,
            id,
            tx,
            rx,
            seq: 0,
            replay: ReplayWindow::default(),
        })
    }
    pub fn conn_id(&self) -> u32 {
        self.id
    }
    pub fn seal(
        &mut self,
        payload: &[u8],
        mode: u8,
        target: usize,
        rng: &mut impl CryptoRngCore,
    ) -> Result<Vec<u8>, Error> {
        let real = u16::try_from(payload.len()).map_err(|_| Error::Length)?;
        let min = OVERHEAD + payload.len();
        let total = if min <= VALVE_SDR_MAX_UDP {
            target.clamp(min, VALVE_SDR_MAX_UDP)
        } else {
            target.max(min)
        };
        if total > u16::MAX as usize {
            return Err(Error::Length);
        }
        let seq = self.seq.checked_add(1).ok_or(Error::SequenceExhausted)?;
        let mut wire = vec![0; total - 16];
        let (head, body) = wire.split_at_mut(9);
        head[0] = DATA;
        head[1..5].copy_from_slice(&self.id.to_le_bytes());
        head[5..9].copy_from_slice(&seq.to_be_bytes());
        body[0] = mode;
        body[1..3].copy_from_slice(&real.to_be_bytes());
        body[3..3 + payload.len()].copy_from_slice(payload);
        if 3 + payload.len() < body.len() {
            random(rng, &mut body[3 + payload.len()..])?;
        }
        let nonce = nonce(self.tx, head);
        self.seq = seq;
        let tag = self
            .cipher
            .encrypt_in_place_detached(Nonce::from_slice(&nonce), head, body)
            .map_err(|_| Error::Authentication)?;
        wire.extend_from_slice(&tag);
        Ok(wire)
    }
    pub fn open(&mut self, wire: &[u8]) -> Result<(u8, Vec<u8>), Error> {
        if wire.len() < OVERHEAD {
            return Err(Error::Length);
        }
        if extract_conn_id(wire) != Some(self.id) {
            return Err(Error::Authentication);
        }
        let head = &wire[..9];
        let nonce = nonce(self.rx, head);
        let mut body = wire[9..].to_vec();
        self.cipher
            .decrypt_in_place(Nonce::from_slice(&nonce), head, &mut body)
            .map_err(|_| Error::Authentication)?;
        let seq = u32::from_be_bytes(word4(&head[5..9]));
        if !self.replay.accept(seq) {
            return Err(Error::Replay);
        }
        let mode = body[0];
        let len = u16::from_be_bytes([body[1], body[2]]) as usize;
        if len > body.len() - 3 {
            return Err(Error::Length);
        }
        body.copy_within(3..3 + len, 0);
        body.truncate(len);
        Ok((mode, body))
    }
}
fn nonce(salt: [u8; 4], head: &[u8]) -> [u8; 12] {
    let mut n = [0; 12];
    n[..4].copy_from_slice(&salt);
    n[4..].copy_from_slice(&head[1..9]);
    n
}

pub fn extract_conn_id(packet: &[u8]) -> Option<u32> {
    if packet.len() < OVERHEAD || packet[0] != DATA {
        return None;
    }
    Some(u32::from_le_bytes(word4(&packet[1..5])))
}
/// Extract client `connection_id` (protobuf field 1, fixed32, tag 0x0d) from a real Valve GNS
/// `0x20` ChallengeRequest (padded wire format: `[0]=0x20, [1..3]=u16 LE proto len, [3..3+len]=proto, [..512]=0x00`).
fn extract_valve_challenge_conn_id(incoming: &[u8]) -> u32 {
    if incoming.len() < 8 || incoming[0] != VALVE_CHALLENGE_REQUEST {
        return 0;
    }
    let proto_len = u16::from_le_bytes([incoming[1], incoming[2]]) as usize;
    let body = if proto_len > 0 && 3 + proto_len <= incoming.len() {
        &incoming[3..3 + proto_len]
    } else {
        &incoming[1..]
    };
    let mut i = 0;
    while i < body.len() {
        let tag = body[i];
        i += 1;
        let wire_type = tag & 0x07;
        if tag == 0x0d && i + 4 <= body.len() {
            return u32::from_le_bytes(word4(&body[i..i + 4]));
        }
        match wire_type {
            0 => {
                while i < body.len() && (body[i] & 0x80) != 0 {
                    i += 1;
                }
                i = i.saturating_add(1);
            }
            1 => i = i.saturating_add(8),
            2 => {
                let mut len = 0usize;
                let mut shift = 0u32;
                while i < body.len() && shift < 28 {
                    let b = body[i];
                    i += 1;
                    len |= ((b & 0x7f) as usize) << shift;
                    if (b & 0x80) == 0 {
                        break;
                    }
                    shift += 7;
                }
                i = i.saturating_add(len);
            }
            5 => i = i.saturating_add(4),
            _ => break,
        }
    }
    0
}

/// Build a genuine 512-byte padded Valve SteamNetSockets `0x24` (`CMsgSteamSockets_UDP_ConnectionClosed`)
/// packet (`[0]=0x24, [1..3]=u16 LE proto len, [3..]=proto, zero-padded to 512B`) so real CS2 clients
/// accept the packet and cleanly show "Server is full" instead of rejecting it as under-padded.
pub fn reject_for_packet(incoming: &[u8]) -> Vec<u8> {
    let to_conn_id = extract_valve_challenge_conn_id(incoming);
    let reason = b"Server is full (20/20). VAC authentication required.";
    let mut proto = Vec::with_capacity(2 + reason.len() + 3 + 5 + 5);
    // field 2: debug string (length-delimited, tag = (2<<3)|2 = 0x12)
    proto.push(0x12);
    proto.push(reason.len() as u8);
    proto.extend_from_slice(reason);
    // field 3: reason_code = 1005 (App_Generic clean disconnect, varint 0xed 0x07, tag 0x18)
    proto.push(0x18);
    proto.push(0xed);
    proto.push(0x07);
    // field 4: to_connection_id (fixed32, tag = (4<<3)|5 = 0x25)
    proto.push(0x25);
    proto.extend_from_slice(&to_conn_id.to_le_bytes());
    // field 5: from_connection_id (fixed32, tag = (5<<3)|5 = 0x2d)
    proto.push(0x2d);
    proto.extend_from_slice(&0u32.to_le_bytes());

    let mut out = vec![0u8; 512];
    out[0] = VALVE_CONNECTION_CLOSED;
    out[1..3].copy_from_slice(&(proto.len() as u16).to_le_bytes());
    out[3..3 + proto.len()].copy_from_slice(&proto);
    out
}

pub fn reject_packet() -> Vec<u8> {
    reject_for_packet(&[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_rng::TestRng;
    const PSK: &str = "stage0-vector-only";
    const TIME: i64 = 1_700_000_000;
    fn hex(s: &str) -> Vec<u8> {
        s.as_bytes()
            .chunks_exact(2)
            .map(|c| u8::from_str_radix(core::str::from_utf8(c).unwrap(), 16).unwrap())
            .collect()
    }
    fn client_rng() -> TestRng {
        TestRng::new(&hex(
            "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a40302010",
        ))
    }
    fn server_rng() -> TestRng {
        TestRng::new(&hex(
            "5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0ebd4c3b2a1",
        ))
    }
    fn pair() -> (Codec, Codec) {
        let (req, hs) = build_request(PSK, TIME, &mut client_rng()).unwrap();
        let (resp, server) = accept_request(&req, PSK, TIME, &mut server_rng()).unwrap();
        (hs.complete(&resp).unwrap(), server)
    }
    #[test]
    fn matches_fixed_go_frames_both_directions() {
        let (req, hs) = build_request(PSK, TIME, &mut client_rng()).unwrap();
        assert_eq!(req.len(), 148);
        assert_eq!(&req[45..61], hex("2fdec1699c66a9cac50fae6ee5dbdbd2"));
        let (resp, mut server) = accept_request(&req[..61], PSK, TIME, &mut server_rng()).unwrap();
        assert_eq!(resp.len(), 92);
        assert_eq!(&resp[37..53], hex("1f9edcdce2dab15977bd05cfb428b6e3"));
        let mut client = hs.complete(&resp[..53]).unwrap();
        assert_eq!(client.tx, [0x28, 0x5e, 0x65, 0xe1]);
        assert_eq!(client.rx, [0x84, 0x38, 0xbe, 0xbf]);
        let cw = client.seal(b"ping", COMBAT, 0, &mut client_rng()).unwrap();
        let sw = server.seal(b"pong", FAST_DL, 0, &mut server_rng()).unwrap();
        assert_eq!(
            cw,
            hex("82d4c3b2a100000001385296a0d6f0a24e1303729edeab36dd46e19f697032a3")
        );
        assert_eq!(
            sw,
            hex("82d4c3b2a1000000015a84f4d15ea4e032a3957131b8023478e45758a4c968c4")
        );
        assert_eq!(server.open(&cw), Ok((COMBAT, b"ping".to_vec())));
        assert_eq!(client.open(&sw), Ok((FAST_DL, b"pong".to_vec())));
    }
    #[test]
    fn independently_signed_clock_boundaries_and_psk() {
        for (skew, valid) in [(-61, false), (-60, true), (60, true), (61, false)] {
            let (req, _) = build_request(" secret ", TIME + skew, &mut client_rng()).unwrap();
            assert_eq!(
                accept_request(&req, " secret ", TIME, &mut server_rng()).is_ok(),
                valid
            );
            assert!(matches!(
                accept_request(&req, "secret", TIME, &mut server_rng()),
                Err(Error::Handshake)
            ));
        }
        let (req, hs) = build_request(PSK, TIME, &mut client_rng()).unwrap();
        let (mut resp, _) = accept_request(&req, PSK, TIME, &mut server_rng()).unwrap();
        resp[52] ^= 1;
        assert!(matches!(hs.complete(&resp), Err(Error::Handshake)));
        assert!(matches!(
            accept_request(&req[..60], PSK, TIME, &mut server_rng()),
            Err(Error::Handshake)
        ));
    }
    #[test]
    fn authenticated_before_window_and_sequence_boundary() {
        let (mut tx, mut rx) = pair();
        let mut rng = client_rng();
        let frames: Vec<_> = (0..65)
            .map(|_| tx.seal(b"x", COMBAT, 28, &mut rng).unwrap())
            .collect();
        let mut bad = frames[64].clone();
        bad[8] ^= 1;
        assert_eq!(rx.open(&bad), Err(Error::Authentication));
        assert!(rx.open(&frames[64]).is_ok());
        assert_eq!(rx.open(&frames[0]), Err(Error::Replay));
        assert!(rx.open(&frames[1]).is_ok());
        assert_eq!(rx.open(&frames[1]), Err(Error::Replay));
        assert!(!rx.replay.accept(0));
        tx.seq = u32::MAX - 1;
        let last = tx.seal(b"", IDLE, 60, &mut rng).unwrap();
        assert_eq!(&last[5..9], &[255; 4]);
        assert_eq!(rx.open(&last), Ok((IDLE, vec![])));
        assert_eq!(
            tx.seal(b"", IDLE, 0, &mut rng),
            Err(Error::SequenceExhausted)
        );
    }
    #[test]
    fn ignores_control_padding_and_caps_only_padded_sizes() {
        let (mut req, hs) = build_request(PSK, TIME, &mut client_rng()).unwrap();
        req[61..].fill(0x55);
        let (mut resp, mut srv) = accept_request(&req, PSK, TIME, &mut server_rng()).unwrap();
        resp[53..].fill(0xaa);
        let mut client = hs.complete(&resp).unwrap();
        for (size, target, wanted) in [(0, 0, 28), (1, 9999, VALVE_SDR_MAX_UDP), (1400, 0, 1428)] {
            let packet = client
                .seal(&vec![1; size], 13, target, &mut client_rng())
                .unwrap();
            assert_eq!(packet.len(), wanted);
            assert_eq!(srv.open(&packet), Ok((13, vec![1; size])));
        }
    }
    #[test]
    fn valve_connection_closed_echoes_challenge_conn_id() {
        // Real Valve 0x20 ChallengeRequest: [0]=0x20, [1..3]=u16 LE len (5), [3]=0x0d, [4..8]=conn_id, zero-padded to 512B
        let mut challenge = vec![0u8; 512];
        challenge[0] = VALVE_CHALLENGE_REQUEST;
        challenge[1..3].copy_from_slice(&5u16.to_le_bytes());
        challenge[3] = 0x0d;
        challenge[4..8].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        let reply = reject_for_packet(&challenge);
        assert_eq!(reply.len(), 512);
        assert_eq!(reply[0], VALVE_CONNECTION_CLOSED);
        let plen = u16::from_le_bytes([reply[1], reply[2]]) as usize;
        assert!(plen > 10 && 3 + plen <= 512);
        let proto = &reply[3..3 + plen];
        let mut needle = vec![0x25u8];
        needle.extend_from_slice(&0x1234_5678u32.to_le_bytes());
        assert!(proto.windows(5).any(|w| w == needle.as_slice()));
        assert_eq!(reject_packet().len(), 512);
        assert_eq!(reject_packet()[0], VALVE_CONNECTION_CLOSED);
    }
    #[test]
    fn rejects_noncontributory_public_keys_even_with_valid_mac() {
        let (mut req, _) = build_request(PSK, TIME, &mut client_rng()).unwrap();
        req[5..37].fill(0);
        let tag = hmac(
            &labelled_key(b"cs2world-hs-req:", PSK.as_bytes()),
            &[&req[..45]],
        );
        req[45..61].copy_from_slice(&tag[..16]);
        assert!(matches!(
            accept_request(&req, PSK, TIME, &mut server_rng()),
            Err(Error::Handshake)
        ));
    }
}
