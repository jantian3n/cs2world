//! Source server queries, on the same UDP socket as session datagrams.
use crate::shaper::bounded;
use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use core::fmt;
use rand_core::RngCore;
use serde::{Deserialize, Serialize};

const PREFIX: [u8; 4] = [0xff; 4];
pub const INFO_QUERY: &[u8] = b"\xff\xff\xff\xffTSource Engine Query\0";
const NAMES: [&str; 18] = [
    "s1mple_cn",
    "Niko_Peek",
    "m0NESY_fan",
    "DonkStyle",
    "ZywOo_AWP",
    "KarriganIGL",
    "ropz_lurk",
    "B1T_Headshot",
    "JameTime",
    "Twistzz_HK",
    "Sh1ro_Clutch",
    "Device_95",
    "XANTARESpeek",
    "Stewie2k_Smoke",
    "FalleN_BR",
    "Frozen_SK",
    "Rain_Entry",
    "Broky_Sniper",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server_name: String,
    pub maps: Vec<String>,
    pub max_players: u8,
    pub game_version: String,
    pub map_cycle_minutes: u32,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            server_name: "[Asia/HK] CS2 5v5 Competitive Practice #3 | 128Tick".into(),
            maps: [
                "de_mirage",
                "de_inferno",
                "de_dust2",
                "de_ancient",
                "de_nuke",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            max_players: 20,
            game_version: "1.40.4.2".into(),
            map_cycle_minutes: 30,
        }
    }
}
impl Config {
    pub fn normalize(&mut self) {
        let d = Self::default();
        if self.server_name.is_empty() {
            self.server_name = d.server_name;
        }
        if self.maps.is_empty() {
            self.maps = d.maps;
        }
        if self.max_players == 0 {
            self.max_players = d.max_players;
        }
        if self.game_version.is_empty() {
            self.game_version = d.game_version;
        }
        if self.map_cycle_minutes == 0 {
            self.map_cycle_minutes = d.map_cycle_minutes;
        }
    }
}
pub fn is_query(packet: &[u8]) -> bool {
    packet.len() >= 5 && packet[..4] == PREFIX
}
fn packet(kind: u8) -> Vec<u8> {
    let mut v = PREFIX.to_vec();
    v.push(kind);
    v
}
fn cstr(dst: &mut Vec<u8>, s: &str) {
    dst.extend_from_slice(s.as_bytes());
    dst.push(0);
}
fn count(max: u8, hour: u8) -> u8 {
    let phase = (f64::from(hour % 24) - 13.0) * core::f64::consts::PI / 12.0;
    let ratio = 0.5 + 0.42 * libm::sin(phase);
    let n = libm::round(ratio * f64::from(max.wrapping_sub(2))) as u8;
    n.max(2).min(max.saturating_sub(1))
}
struct Member {
    name: &'static str,
    score: i32,
    initial_seconds: u32,
}
pub struct Responder {
    cfg: Config,
    start_ms: u64,
    roster: Vec<Member>,
}
impl Responder {
    pub fn new(mut cfg: Config, now_ms: u64, local_hour: u8, rng: &mut impl RngCore) -> Self {
        cfg.normalize();
        let mut names = NAMES;
        for i in (1..names.len()).rev() {
            names.swap(i, bounded(rng, (i + 1) as u32) as usize);
        }
        let roster = names
            .iter()
            .take(count(cfg.max_players, local_hour) as usize)
            .map(|&name| Member {
                name,
                score: (2 + bounded(rng, 28)) as i32,
                initial_seconds: 120 + bounded(rng, 2400),
            })
            .collect();
        Self {
            cfg,
            start_ms: now_ms,
            roster,
        }
    }
    pub fn handle(
        &mut self,
        pkt: &[u8],
        now_ms: u64,
        local_hour: u8,
        rng: &mut impl RngCore,
    ) -> Option<Vec<u8>> {
        if !is_query(pkt) {
            return None;
        }
        let elapsed = now_ms.saturating_sub(self.start_ms);
        Some(match pkt[4] {
            b'T' => {
                let mut out = packet(b'I');
                out.push(17);
                cstr(&mut out, &self.cfg.server_name);
                let cycle = elapsed / (60_000 * u64::from(self.cfg.map_cycle_minutes));
                cstr(
                    &mut out,
                    &self.cfg.maps[(cycle % self.cfg.maps.len() as u64) as usize],
                );
                cstr(&mut out, "csgo");
                cstr(&mut out, "Counter-Strike 2");
                out.extend_from_slice(&730u16.to_le_bytes());
                out.extend_from_slice(&[
                    count(self.cfg.max_players, local_hour),
                    self.cfg.max_players,
                    0,
                    b'd',
                    b'l',
                    0,
                    1,
                ]);
                cstr(&mut out, &self.cfg.game_version);
                out.push(0);
                out
            }
            b'U' if pkt.get(5..9) != Some(PREFIX.as_slice()) => {
                let mut out = packet(b'D');
                out.push(self.roster.len() as u8);
                for (i, m) in self.roster.iter().enumerate() {
                    out.push(i as u8);
                    cstr(&mut out, m.name);
                    out.extend_from_slice(&m.score.to_le_bytes());
                    let duration = (f64::from(m.initial_seconds) + elapsed as f64 / 1000.0) as f32;
                    out.extend_from_slice(&duration.to_le_bytes());
                }
                out
            }
            _ => {
                let mut out = packet(b'A');
                out.extend_from_slice(&rng.next_u32().to_le_bytes());
                out
            }
        })
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidHeader,
    Truncated,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidHeader => "invalid A2S header",
            Self::Truncated => "truncated A2S response",
        })
    }
}
impl core::error::Error for Error {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Info {
    pub protocol: u8,
    pub server_name: String,
    pub map_name: String,
    pub folder: String,
    pub game: String,
    pub app_id: u16,
    pub players: u8,
    pub max_players: u8,
    pub bots: u8,
    pub server_type: u8,
    pub environment: u8,
    pub visibility: u8,
    pub vac: u8,
    pub game_version: String,
    pub edf: u8,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Player {
    pub index: u8,
    pub name: String,
    pub score: i32,
    pub duration: f32,
}

struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn new(pkt: &'a [u8], kind: u8) -> Result<Self, Error> {
        if !is_query(pkt) || pkt[4] != kind {
            return Err(Error::InvalidHeader);
        }
        Ok(Self(&pkt[5..]))
    }
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let (item, rest) = self.0.split_at_checked(N).ok_or(Error::Truncated)?;
        self.0 = rest;
        Ok(item.try_into().expect("fixed slice length"))
    }
    fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.bytes::<1>()?[0])
    }
    fn string(&mut self) -> Result<String, Error> {
        let i = self
            .0
            .iter()
            .position(|v| *v == 0)
            .ok_or(Error::Truncated)?;
        let s = String::from_utf8_lossy(&self.0[..i]).to_string();
        self.0 = &self.0[i + 1..];
        Ok(s)
    }
}
pub fn parse_info(pkt: &[u8]) -> Result<Info, Error> {
    let mut r = Reader::new(pkt, b'I')?;
    let info = Info {
        protocol: r.byte()?,
        server_name: r.string()?,
        map_name: r.string()?,
        folder: r.string()?,
        game: r.string()?,
        app_id: u16::from_le_bytes(r.bytes()?),
        players: r.byte()?,
        max_players: r.byte()?,
        bots: r.byte()?,
        server_type: r.byte()?,
        environment: r.byte()?,
        visibility: r.byte()?,
        vac: r.byte()?,
        game_version: r.string()?,
        edf: r.0.first().copied().unwrap_or(0),
    };
    Ok(info)
}
pub fn parse_players(pkt: &[u8]) -> Result<Vec<Player>, Error> {
    let mut r = Reader::new(pkt, b'D')?;
    let n = r.byte()?;
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        out.push(Player {
            index: r.byte()?,
            name: r.string()?,
            score: i32::from_le_bytes(r.bytes()?),
            duration: f32::from_le_bytes(r.bytes()?),
        });
    }
    Ok(out)
}
pub fn parse_challenge(pkt: &[u8]) -> Result<u32, Error> {
    Ok(u32::from_le_bytes(Reader::new(pkt, b'A')?.bytes()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_rng::TestRng;
    #[test]
    fn info_uses_hour_and_elapsed_map_cycle() {
        let mut rng = TestRng::new(&[]);
        let mut r = Responder::new(Config::default(), 500, 19, &mut rng);
        let peak = parse_info(&r.handle(INFO_QUERY, 500, 19, &mut rng).unwrap()).unwrap();
        assert_eq!(
            (peak.app_id, peak.protocol, peak.players, peak.max_players),
            (730, 17, 17, 20)
        );
        assert_eq!(
            (&*peak.game, &*peak.map_name),
            ("Counter-Strike 2", "de_mirage")
        );
        let off = parse_info(
            &r.handle(b"\xff\xff\xff\xffT", 1_800_500, 7, &mut rng)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(off.players, 2);
        assert_eq!(off.map_name, "de_inferno");
        assert_eq!(r.handle(&[255; 4], 0, 0, &mut rng), None);
    }
    #[test]
    fn player_and_rules_challenges_match_go() {
        let mut rng = TestRng::new(&[]);
        let mut r = Responder::new(Config::default(), 100, 19, &mut rng);
        let mut query = packet(b'U');
        query.extend_from_slice(&[255; 4]);
        let challenge = parse_challenge(&r.handle(&query, 100, 19, &mut rng).unwrap()).unwrap();
        query[5..9].copy_from_slice(&challenge.to_le_bytes());
        let list = parse_players(&r.handle(&query, 100, 19, &mut rng).unwrap()).unwrap();
        let later = parse_players(&r.handle(&packet(b'U'), 2600, 7, &mut rng).unwrap()).unwrap();
        assert_eq!(list.len(), 17);
        for (p, l) in list.iter().zip(&later) {
            assert_eq!(p.name, l.name);
            assert!((2..30).contains(&p.score));
            assert!((120.0..2520.0).contains(&p.duration));
            assert_eq!(p.duration + 2.5, l.duration);
        }
        let mut rules = packet(b'V');
        let n = parse_challenge(&r.handle(&rules, 100, 7, &mut rng).unwrap()).unwrap();
        rules.extend_from_slice(&n.to_le_bytes());
        assert!(parse_challenge(&r.handle(&rules, 100, 7, &mut rng).unwrap()).is_ok());
        assert!(parse_info(&[255, 255, 255, 255, b'I', 17, 1]).is_err());
        assert!(parse_players(&[255, 255, 255, 255, b'D', 3]).is_err());
    }
}
