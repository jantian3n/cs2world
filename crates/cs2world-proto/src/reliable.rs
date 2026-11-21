//! Byte-stream KCP over kcp-go's optional per-datagram Reed–Solomon envelope.
//! The caller owns the monotonic millisecond clock and all network I/O.
use alloc::{
    collections::{BTreeMap, VecDeque},
    vec,
    vec::Vec,
};
use core::{fmt, mem};

use reed_solomon_erasure::galois_8::ReedSolomon;

const DATA: u16 = 0xf1;
const PARITY: u16 = 0xf2;
const SEGMENT_HEADER: usize = 24;
const MAX_PACKET: usize = 1472;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReliableError {
    InvalidConfig,
    InvalidPacket,
    Closed,
}
impl fmt::Display for ReliableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfig => "invalid KCP/FEC parameters",
            Self::InvalidPacket => "invalid KCP/FEC packet",
            Self::Closed => "KCP session closed",
        })
    }
}
impl core::error::Error for ReliableError {}

fn packet_type(buf: &[u8]) -> Option<u16> {
    let prefix = buf.get(..6)?;
    Some(u16::from_le_bytes([prefix[4], prefix[5]]))
}

/// A parity fragment alone cannot identify or open a KCP conversation.
pub fn extract_conv(wire: &[u8]) -> Option<u32> {
    let kcp = match packet_type(wire)? {
        DATA => wire.get(8..)?,
        PARITY | 0xf3 => return None,
        _ => wire,
    };
    let head = kcp.get(..SEGMENT_HEADER)?;
    if !(81..=84).contains(&head[4]) {
        return None;
    }
    Some(u32::from_le_bytes(head[..4].try_into().ok()?))
}

fn fec_code(d: usize, p: usize) -> Result<ReedSolomon, ReliableError> {
    if d == 0 || p == 0 || d.checked_add(p).filter(|n| *n <= 256).is_none() {
        return Err(ReliableError::InvalidConfig);
    }
    ReedSolomon::new(d, p).map_err(|_| ReliableError::InvalidConfig)
}
fn paws(total: usize) -> u32 {
    u32::MAX / (total as u32) * (total as u32)
}

struct FecEncoder {
    d: usize,
    p: usize,
    code: ReedSolomon,
    seq: u32,
    data: Vec<Vec<u8>>,
    last: Option<u32>,
}
impl FecEncoder {
    fn new(d: usize, p: usize) -> Result<Self, ReliableError> {
        Ok(Self {
            d,
            p,
            code: fec_code(d, p)?,
            seq: 0,
            data: Vec::with_capacity(d),
            last: None,
        })
    }
    fn advance(&mut self, count: usize) {
        self.seq = ((u64::from(self.seq) + count as u64) % u64::from(paws(self.d + self.p))) as u32;
    }
    fn frame(&mut self, kind: u16, payload: &[u8]) -> Vec<u8> {
        let mut wire = Vec::with_capacity(6 + payload.len());
        wire.extend_from_slice(&self.seq.to_le_bytes());
        wire.extend_from_slice(&kind.to_le_bytes());
        wire.extend_from_slice(payload);
        self.advance(1);
        wire
    }
    fn encode(&mut self, now: u32, packet: &[u8]) -> Vec<Vec<u8>> {
        let mut data = Vec::with_capacity(packet.len() + 2);
        data.extend_from_slice(&((packet.len() + 2) as u16).to_le_bytes());
        data.extend_from_slice(packet);
        let mut output = vec![self.frame(DATA, &data)];
        self.data.push(data);
        let gap = self.last.replace(now).map(|last| now.wrapping_sub(last));
        if self.data.len() < self.d {
            return output;
        }
        if gap.is_none_or(|elapsed| elapsed >= 500) {
            self.advance(self.p);
            self.data.clear();
            return output;
        }
        let width = self.data.iter().map(Vec::len).max().unwrap_or(0);
        for shard in &mut self.data {
            shard.resize(width, 0);
        }
        let mut parity = vec![vec![0; width]; self.p];
        // Constructor and equal-length allocation above satisfy the encoder invariants.
        self.code
            .encode_sep(&self.data, &mut parity)
            .expect("valid FEC group shape");
        for shard in parity {
            output.push(self.frame(PARITY, &shard));
        }
        self.data.clear();
        output
    }
}

struct FecDecoder {
    d: usize,
    p: usize,
    code: ReedSolomon,
    newest: Option<u32>,
    groups: BTreeMap<u32, Vec<Option<Vec<u8>>>>,
    samples: VecDeque<(u32, bool)>,
    tuning: bool,
}
impl FecDecoder {
    fn new(d: usize, p: usize) -> Result<Self, ReliableError> {
        Ok(Self {
            d,
            p,
            code: fec_code(d, p)?,
            newest: None,
            groups: BTreeMap::new(),
            samples: VecDeque::new(),
            tuning: false,
        })
    }
    fn detect(&self) -> Option<(usize, usize)> {
        let mut ordered: Vec<_> = self.samples.iter().copied().collect();
        // Rotate the signed wrapping order around the most recent sample.
        let origin = self.samples.back()?.0;
        ordered.sort_unstable_by_key(|&(seq, _)| seq.wrapping_sub(origin) as i32);
        let mut filtered = Vec::with_capacity(ordered.len());
        for entries in ordered.chunk_by(|a, b| a.0 == b.0) {
            if entries.iter().all(|s| s.1 == entries[0].1) {
                filtered.push(entries[0]);
            }
        }
        let period = |bit| {
            for left in 1..filtered.len() {
                if filtered[left].1 != bit
                    || filtered[left - 1].1 == bit
                    || filtered[left - 1].0.wrapping_add(1) != filtered[left].0
                {
                    continue;
                }
                for right in left + 1..filtered.len() {
                    if filtered[right - 1].0.wrapping_add(1) != filtered[right].0 {
                        break;
                    }
                    if filtered[right].1 != bit {
                        return Some(right - left);
                    }
                }
            }
            None
        };
        let (d, p) = (period(true)?, period(false)?);
        (d + p <= 256).then_some((d, p))
    }
    fn receive(&mut self, seq: u32, kind: u16, symbol: &[u8]) -> Vec<Vec<u8>> {
        let data = kind == DATA;
        if self.samples.len() == 258 {
            self.samples.pop_front();
        }
        self.samples.push_back((seq, data));
        let total = self.d + self.p;
        if seq >= paws(total) {
            return Vec::new();
        }
        let index = seq as usize % total;
        if data != (index < self.d) {
            self.tuning = true;
        }
        if self.tuning {
            if let Some((d, p)) = self.detect() {
                if (d, p) != (self.d, self.p) {
                    if let Ok(code) = fec_code(d, p) {
                        self.code = code;
                        self.d = d;
                        self.p = p;
                        self.groups.clear();
                        self.newest = None;
                    }
                }
                self.tuning = false;
            }
            return Vec::new();
        }
        let start = seq - (index as u32);
        let newest = *self.newest.get_or_insert(start);
        if start.wrapping_sub(newest) as i32 > 0 {
            self.newest = Some(start);
            self.groups
                .retain(|&key, _| (start.wrapping_sub(key) as i32) <= 3 * total as i32);
        } else if newest.wrapping_sub(start) as i32 > 3 * total as i32 {
            return Vec::new();
        }
        let group = self
            .groups
            .entry(start)
            .or_insert_with(|| vec![None; total]);
        if group[index].is_some() {
            return Vec::new();
        }
        group[index] = Some(symbol.to_vec());
        if group.iter().flatten().count() < self.d {
            return Vec::new();
        }
        let mut group = self.groups.remove(&start).expect("present FEC group");
        let missing: Vec<_> = (0..self.d).filter(|&i| group[i].is_none()).collect();
        if missing.is_empty() {
            return Vec::new();
        }
        let width = group.iter().flatten().map(Vec::len).max().unwrap_or(0);
        for s in group.iter_mut().flatten() {
            s.resize(width, 0);
        }
        if self.code.reconstruct_data(&mut group).is_err() {
            return Vec::new();
        }
        let mut recovered = Vec::with_capacity(missing.len());
        for idx in missing {
            if let Some(mut s) = group[idx].take() {
                let size = u16::from_le_bytes([s[0], s[1]]) as usize;
                if (2..=s.len()).contains(&size) {
                    s.truncate(size);
                    s.drain(..2);
                    recovered.push(s);
                }
            }
        }
        recovered
    }
}

fn le32(data: &[u8]) -> u32 {
    u32::from_le_bytes([data[0], data[1], data[2], data[3]])
}
fn delta(a: u32, b: u32) -> i32 {
    a.wrapping_sub(b) as i32
}

struct Sent {
    sn: u32,
    data: Vec<u8>,
    transmissions: u32,
    ts: u32,
    rto: u32,
    deadline: u32,
    newer_acks: u32,
    acked: bool,
}

struct Batch {
    packets: Vec<Vec<u8>>,
    current: Vec<u8>,
    mtu: usize,
    conv: u32,
    wnd: u16,
    una: u32,
}
impl Batch {
    fn segment(&mut self, cmd: u8, sn: u32, ts: u32, payload: &[u8]) {
        if self.current.len() + SEGMENT_HEADER + payload.len() > self.mtu {
            self.flush();
        }
        self.current.extend_from_slice(&self.conv.to_le_bytes());
        self.current.extend_from_slice(&[cmd, 0]);
        self.current.extend_from_slice(&self.wnd.to_le_bytes());
        self.current.extend_from_slice(&ts.to_le_bytes());
        self.current.extend_from_slice(&sn.to_le_bytes());
        self.current.extend_from_slice(&self.una.to_le_bytes());
        self.current
            .extend_from_slice(&(payload.len() as u32).to_le_bytes());
        self.current.extend_from_slice(payload);
    }
    fn flush(&mut self) {
        if !self.current.is_empty() {
            self.packets.push(mem::take(&mut self.current));
        }
    }
}

/// KCP stream-mode state with kcp-go's 15 ms no-delay profile. Call update after writes,
/// after input, and at least once every 15 ms while active to drive acknowledgments and timers.
pub struct Reliable {
    conv: u32,
    mtu: usize,
    snd_wnd: u16,
    rcv_wnd: u16,
    remote_wnd: u16,
    send_next: u32,
    recv_next: u32,
    queued: VecDeque<Vec<u8>>,
    sent: VecDeque<Sent>,
    reordered: BTreeMap<u32, Vec<u8>>,
    ready: VecDeque<Vec<u8>>,
    acks: VecDeque<(u32, u32)>,
    tell_window: bool,
    probe_wait: u32,
    probe_at: u32,
    srtt: i64,
    rttvar: i64,
    rto: u32,
    dead: bool,
    enc: Option<FecEncoder>,
    dec: Option<FecDecoder>,
}

impl Reliable {
    pub fn open(
        conv: u32,
        mtu: usize,
        snd_wnd: u16,
        rcv_wnd: u16,
        data_shards: usize,
        parity_shards: usize,
    ) -> Result<Self, ReliableError> {
        let fec = data_shards != 0 && parity_shards != 0;
        let mtu = if mtu == 0 { 1180 } else { mtu.min(MAX_PACKET) };
        // The Go session starts with MTU 1400 and preserves it if SetMtu rejects a small value.
        let header = if fec { 8 } else { 0 };
        let mtu = if mtu <= header + SEGMENT_HEADER {
            1400 - header
        } else {
            mtu - header
        };
        let (enc, dec) = if fec {
            (
                Some(FecEncoder::new(data_shards, parity_shards)?),
                Some(FecDecoder::new(data_shards, parity_shards)?),
            )
        } else {
            (None, None)
        };
        Ok(Self {
            conv,
            mtu,
            snd_wnd: if snd_wnd == 0 { 1024 } else { snd_wnd },
            rcv_wnd: if rcv_wnd == 0 { 1024 } else { rcv_wnd },
            remote_wnd: 32,
            send_next: 0,
            recv_next: 0,
            queued: VecDeque::new(),
            sent: VecDeque::new(),
            reordered: BTreeMap::new(),
            ready: VecDeque::new(),
            acks: VecDeque::new(),
            tell_window: false,
            probe_wait: 0,
            probe_at: 0,
            srtt: 0,
            rttvar: 0,
            rto: 200,
            dead: false,
            enc,
            dec,
        })
    }
    pub fn pending(&self) -> usize {
        self.queued.len() + self.sent.len()
    }
    pub fn is_dead(&self) -> bool {
        self.dead
    }
    pub fn send(&mut self, mut bytes: &[u8]) -> Result<(), ReliableError> {
        if self.dead {
            return Err(ReliableError::Closed);
        }
        let mss = self.mtu - SEGMENT_HEADER;
        if let Some(last) = self.queued.back_mut() {
            let append = (mss - last.len()).min(bytes.len());
            last.extend_from_slice(&bytes[..append]);
            bytes = &bytes[append..];
        }
        self.queued.extend(bytes.chunks(mss).map(<[u8]>::to_vec));
        Ok(())
    }
    fn receive_contiguous(&mut self) {
        while self.ready.len() < usize::from(self.rcv_wnd) {
            let Some(chunk) = self.reordered.remove(&self.recv_next) else {
                break;
            };
            self.ready.push_back(chunk);
            self.recv_next = self.recv_next.wrapping_add(1);
        }
    }
    fn sample_rtt(&mut self, sample: u32) {
        let rtt = i64::from(sample);
        if self.srtt == 0 {
            self.srtt = rtt;
            self.rttvar = rtt / 2;
        } else {
            let difference = rtt - self.srtt;
            self.srtt += difference >> 3;
            let weight = if rtt < self.srtt - self.rttvar { 5 } else { 2 };
            self.rttvar += (difference.abs() - self.rttvar) >> weight;
        }
        self.rto = (self.srtt + (self.rttvar * 4).max(15)).clamp(30, 60000) as u32;
    }
    fn input_segments(
        &mut self,
        now: u32,
        mut packet: &[u8],
        recovered: bool,
    ) -> Result<(), ReliableError> {
        if packet.len() < SEGMENT_HEADER {
            return Err(ReliableError::InvalidPacket);
        }
        let mut rtt = None;
        while packet.len() >= SEGMENT_HEADER {
            let cmd = packet[4];
            let size = le32(&packet[20..]) as usize;
            if le32(packet) != self.conv
                || !(81..=84).contains(&cmd)
                || packet[5] != 0
                || size > packet.len() - SEGMENT_HEADER
            {
                return Err(ReliableError::InvalidPacket);
            }
            let ts = le32(&packet[8..]);
            let sn = le32(&packet[12..]);
            let una = le32(&packet[16..]);
            let data = &packet[SEGMENT_HEADER..SEGMENT_HEADER + size];
            if !recovered {
                self.remote_wnd = u16::from_le_bytes([packet[6], packet[7]]);
            }
            if delta(una, self.send_next) <= 0 {
                while self.sent.front().is_some_and(|s| delta(s.sn, una) < 0) {
                    self.sent.pop_front();
                }
            }
            match cmd {
                81 => {
                    let distance = delta(sn, self.recv_next);
                    if distance < i32::from(self.rcv_wnd) {
                        // Bound pending ACK storage even when many identical packets arrive between ticks.
                        if self.acks.len() >= 2 * usize::from(self.rcv_wnd) {
                            self.acks.pop_front();
                        }
                        self.acks.push_back((sn, ts));
                        if distance >= 0 {
                            self.reordered.entry(sn).or_insert_with(|| data.to_vec());
                            self.receive_contiguous();
                        }
                    }
                }
                82 => {
                    if self.sent.front().is_some_and(|s| delta(sn, s.sn) >= 0)
                        && delta(sn, self.send_next) < 0
                    {
                        for s in &mut self.sent {
                            if s.sn == sn {
                                s.acked = true;
                                break;
                            }
                            if !s.acked && delta(ts, s.ts) >= 0 {
                                s.newer_acks = s.newer_acks.saturating_add(1);
                            }
                        }
                        while self.sent.front().is_some_and(|s| s.acked) {
                            self.sent.pop_front();
                        }
                    }
                    if !recovered && delta(now, ts) >= 0 {
                        rtt = Some(now.wrapping_sub(ts));
                    }
                }
                83 => self.tell_window = true,
                _ => {}
            }
            packet = &packet[SEGMENT_HEADER + size..];
        }
        if let Some(rtt) = rtt {
            self.sample_rtt(rtt);
        }
        Ok(())
    }
    pub fn input(&mut self, now: u32, wire: &[u8]) -> Result<(), ReliableError> {
        if wire.len() > MAX_PACKET {
            return Err(ReliableError::InvalidPacket);
        }
        match packet_type(wire).ok_or(ReliableError::InvalidPacket)? {
            kind @ (DATA | PARITY) => {
                let symbol = wire
                    .get(6..)
                    .filter(|s| s.len() >= 2)
                    .ok_or(ReliableError::InvalidPacket)?;
                if kind == DATA {
                    self.input_segments(now, &symbol[2..], false)?;
                }
                if self.dec.is_none() {
                    self.dec = Some(FecDecoder::new(1, 1)?);
                }
                let recovered = self.dec.as_mut().unwrap().receive(le32(wire), kind, symbol);
                for packet in recovered {
                    self.input_segments(now, &packet, true)?;
                }
            }
            0xf3 => {}
            _ => self.input_segments(now, wire, false)?,
        }
        Ok(())
    }
    pub fn update(&mut self, now: u32) -> Vec<Vec<u8>> {
        let mut batch = Batch {
            packets: Vec::new(),
            current: Vec::new(),
            mtu: self.mtu,
            conv: self.conv,
            wnd: self.rcv_wnd - self.ready.len() as u16,
            una: self.recv_next,
        };
        while let Some((sn, ts)) = self.acks.pop_front() {
            if delta(sn, self.recv_next) >= 0 || self.acks.is_empty() {
                batch.segment(82, sn, ts, &[]);
            }
        }
        if self.remote_wnd != 0 {
            self.probe_wait = 0;
        } else if self.probe_wait == 0 {
            self.probe_wait = 500;
            self.probe_at = now.wrapping_add(500);
        } else if delta(now, self.probe_at) >= 0 {
            self.probe_wait = (self.probe_wait + self.probe_wait / 2).min(120000);
            self.probe_at = now.wrapping_add(self.probe_wait);
            batch.segment(83, 0, 0, &[]);
        }
        if mem::take(&mut self.tell_window) {
            batch.segment(84, 0, 0, &[]);
        }
        let oldest = self.sent.front().map_or(self.send_next, |s| s.sn);
        let limit = u32::from(self.snd_wnd.min(self.remote_wnd));
        let mut new_data = false;
        while self.send_next.wrapping_sub(oldest) < limit {
            let Some(data) = self.queued.pop_front() else {
                break;
            };
            self.sent.push_back(Sent {
                sn: self.send_next,
                data,
                transmissions: 0,
                ts: now,
                rto: self.rto,
                deadline: now,
                newer_acks: 0,
                acked: false,
            });
            self.send_next = self.send_next.wrapping_add(1);
            new_data = true;
        }
        for sent in &mut self.sent {
            if sent.acked {
                continue;
            }
            if sent.transmissions == 0 {
                sent.rto = self.rto;
            } else if sent.newer_acks != u32::MAX
                && (sent.newer_acks >= 2 || (sent.newer_acks > 0 && !new_data))
            {
                sent.newer_acks = u32::MAX;
                sent.rto = self.rto;
            } else if delta(now, sent.deadline) >= 0 {
                sent.rto = sent.rto.saturating_add(self.rto / 2);
                sent.newer_acks = 0;
            } else {
                continue;
            }
            sent.ts = now;
            sent.deadline = now.wrapping_add(sent.rto);
            sent.transmissions = sent.transmissions.saturating_add(1);
            if sent.transmissions >= 20 {
                self.dead = true;
            }
            batch.segment(81, sent.sn, now, &sent.data);
        }
        batch.flush();
        match &mut self.enc {
            None => batch.packets,
            Some(enc) => batch
                .packets
                .into_iter()
                .flat_map(|p| enc.encode(now, &p))
                .collect(),
        }
    }
    pub fn recv(&mut self) -> Vec<u8> {
        if self.ready.len() == usize::from(self.rcv_wnd) {
            self.tell_window = true;
        }
        let mut output = Vec::new();
        while let Some(chunk) = self.ready.pop_front() {
            output.extend_from_slice(&chunk);
            self.receive_contiguous();
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_rs_variable_shard_vector() {
        let mut fec = FecEncoder::new(10, 3).unwrap();
        let mut packets = Vec::new();
        for i in 0..10 {
            packets = fec.encode(10, &alloc::vec![i+1;usize::from(i)+1]);
        }
        assert_eq!(packets.len(), 4);
        assert_eq!(
            &packets[1][6..],
            &[0x8e, 0, 0x45, 0xc4, 0xf5, 0x19, 0xc3, 0x7e, 0xdc, 0x1c, 0x0f, 0x14]
        );
        assert_eq!(
            &packets[2][6..],
            &[0x3d, 0, 0xf2, 0x64, 0x7b, 0xae, 0x28, 0xdb, 0x0d, 0xaf, 0x0c, 0x1e]
        );
        assert_eq!(
            &packets[3][6..],
            &[0xf9, 0, 0x12, 0xad, 0x1c, 0xba, 0x92, 0x8c, 0xf3, 0xd4, 0x05, 0x28]
        );
    }

    #[test]
    fn go_header_and_recovery() {
        let mut tx = Reliable::open(0x11223344, 1180, 1024, 1024, 10, 3).unwrap();
        let mut rx = Reliable::open(0x11223344, 1180, 1024, 1024, 10, 3).unwrap();
        let bytes: Vec<_> = (0..11480).map(|i| (i % 251) as u8).collect();
        tx.send(&bytes).unwrap();
        let packets = tx.update(10);
        assert_eq!(extract_conv(&packets[0]), Some(0x11223344));
        assert_eq!(
            &packets[0][4..16],
            &[0xf1, 0, 0x96, 4, 0x44, 0x33, 0x22, 0x11, 0x51, 0, 0, 4]
        );
        assert_eq!(packets.len(), 13);
        for (i, packet) in packets.iter().enumerate() {
            if i != 0 && i != 3 && i != 9 {
                rx.input(12, packet).unwrap();
            }
        }
        assert_eq!(rx.recv(), bytes);
        for ack in rx.update(12) {
            tx.input(14, &ack).unwrap();
        }
        assert_eq!(tx.pending(), 0);
    }
    #[test]
    fn retransmit_honors_deadline_across_clock_wrap() {
        let mut tx = Reliable::open(7, 80, 2, 4, 0, 0).unwrap();
        tx.send(&[9; 100]).unwrap();
        let start = u32::MAX - 50;
        assert_eq!(tx.update(start).len(), 2);
        assert!(tx.update(start.wrapping_add(199)).is_empty());
        assert_eq!(tx.update(start.wrapping_add(200)).len(), 2);
    }

    #[test]
    fn late_parity_and_adaptive_receive() {
        let mut encoder = FecEncoder::new(3, 2).unwrap();
        let mut decoder = FecDecoder::new(1, 1).unwrap();
        let mut recovered = Vec::new();
        for n in 0..18u8 {
            for wire in encoder.encode(u32::from(n), &[n]) {
                let seq = le32(&wire);
                if seq == 25 {
                    continue;
                }
                recovered.extend(decoder.receive(seq, packet_type(&wire).unwrap(), &wire[6..]));
            }
        }
        assert_eq!((decoder.d, decoder.p), (3, 2));
        assert_eq!(recovered, [vec![15]]);
        let mut skip = FecEncoder::new(2, 1).unwrap();
        assert_eq!(skip.encode(0, b"a").len(), 1);
        assert_eq!(skip.encode(500, b"b").len(), 1);
        assert_eq!(le32(&skip.encode(501, b"c")[0]), 3);
        skip.seq = paws(3) - 2;
        for wire in skip.encode(502, b"d") {
            assert!(le32(&wire) < paws(3));
        }
        assert_eq!(skip.seq, 0);
    }

    #[test]
    fn closed_window_probes_then_reopens() {
        let mut tx = Reliable::open(17, 80, 1, 1, 0, 0).unwrap();
        let mut rx = Reliable::open(17, 80, 2, 1, 0, 0).unwrap();
        tx.send(&[9; 100]).unwrap();
        // Establish zero remote credit. Losing the window reopening notification must be recoverable.
        for wire in tx.update(0) {
            rx.input(1, &wire).unwrap();
        }
        for wire in rx.update(1) {
            tx.input(2, &wire).unwrap();
        }
        assert!(tx.update(2).is_empty());
        assert_eq!(rx.recv(), [9; 56]);
        rx.update(3);
        assert_eq!(tx.update(502)[0][4], 83);
        let mut ask = vec![0; 24];
        ask[..4].copy_from_slice(&17u32.to_le_bytes());
        ask[4] = 83;
        rx.input(503, &ask).unwrap();
        let report = rx.update(503);
        assert_eq!(report[0][4], 84);
        tx.input(504, &report[0]).unwrap();
        for wire in tx.update(505) {
            rx.input(506, &wire).unwrap();
        }
        assert_eq!(rx.recv(), [9; 44]);
    }
}
