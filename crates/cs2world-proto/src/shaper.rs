//! Real-CS2-calibrated length selection and 64Hz bidirectional heartbeat eligibility.
//! Empirical histograms are derived from sanitized real CS2 Valve GNS/SDR match captures
//! (`configs/cs2_profile.json`).
use rand_core::RngCore;
use serde::{Deserialize, Serialize};

use crate::gns::{COMBAT, FAST_DL, OVERHEAD, VALVE_SDR_MAX_UDP};

/// Legacy 46ms poll interval retained for compatibility.
pub const DUMMY_POLL_MS: u64 = 46;
/// Real CS2 64Hz subtick cadence (~15.625ms -> 16ms timer tick).
pub const HEARTBEAT_POLL_MS: u64 = 16;
/// Empirical real CS2 uplink UDP payload ceiling (bytes).
pub const UPLINK_MAX_UDP: usize = 1069;
/// Empirical real CS2 downlink UDP payload ceiling (bytes).
pub const DOWNLINK_MAX_UDP: usize = VALVE_SDR_MAX_UDP;

/// Traffic direction used to select the matching real CS2 empirical packet-size profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Uplink,
    Downlink,
}

impl Direction {
    pub const fn max_udp(self) -> usize {
        match self {
            Self::Uplink => UPLINK_MAX_UDP,
            Self::Downlink => DOWNLINK_MAX_UDP,
        }
    }
}

/// Cumulative weight (out of 10_000) -> (bin_lo, bin_hi) from real CS2 uplink capture.
const UPLINK_CDF: &[(u32, usize, usize)] = &[
    (507, 32, 63),
    (516, 64, 191),
    (571, 192, 255),
    (654, 256, 319),
    (844, 320, 383),
    (904, 384, 511),
    (1297, 512, 575),
    (2973, 576, 639),
    (3680, 640, 703),
    (4418, 704, 767),
    (5849, 768, 831),
    (8832, 832, 895),
    (9852, 896, 959),
    (9990, 960, 1023),
    (10000, 1024, UPLINK_MAX_UDP),
];

/// Cumulative weight (out of 10_000) -> (bin_lo, bin_hi) from real CS2 downlink capture.
const DOWNLINK_CDF: &[(u32, usize, usize)] = &[
    (63, 32, 63),
    (236, 64, 127),
    (361, 128, 191),
    (558, 192, 255),
    (735, 256, 319),
    (1087, 320, 383),
    (1661, 384, 447),
    (2436, 448, 511),
    (3288, 512, 575),
    (4206, 576, 639),
    (5109, 640, 703),
    (5839, 704, 767),
    (6583, 768, 831),
    (7184, 832, 895),
    (7678, 896, 959),
    (8015, 960, 1023),
    (8291, 1024, 1087),
    (8503, 1088, 1151),
    (8778, 1152, 1215),
    (10000, 1216, DOWNLINK_MAX_UDP),
];

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub tick_rate_hz: u32,
    pub idle_sleep_seconds: u64,
    pub combat_mean_bytes: usize,
    pub fastdl_chunk_size: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            tick_rate_hz: 64,
            idle_sleep_seconds: 600,
            combat_mean_bytes: 720,
            fastdl_chunk_size: VALVE_SDR_MAX_UDP,
        }
    }
}
impl Config {
    pub fn normalize(&mut self) {
        let d = Self::default();
        if self.tick_rate_hz == 0 {
            self.tick_rate_hz = d.tick_rate_hz;
        }
        if self.idle_sleep_seconds == 0 {
            self.idle_sleep_seconds = d.idle_sleep_seconds;
        }
        if self.combat_mean_bytes == 0 {
            self.combat_mean_bytes = d.combat_mean_bytes;
        }
        if self.fastdl_chunk_size == 0 || self.fastdl_chunk_size > VALVE_SDR_MAX_UDP {
            self.fastdl_chunk_size = d.fastdl_chunk_size;
        }
    }
}

/// Uniform integer in [0,bound). Reject the partial bin to keep all outcomes equiprobable.
pub(crate) fn bounded(rng: &mut impl RngCore, bound: u32) -> u32 {
    if bound <= 1 {
        return 0;
    }
    let threshold = bound.wrapping_neg() % bound;
    loop {
        let value = rng.next_u32();
        if value >= threshold {
            return value % bound;
        }
    }
}
fn unit_open(rng: &mut impl RngCore) -> f64 {
    // Exactly representable midpoints strictly between 0 and 1 (52 random bits).
    ((rng.next_u64() >> 12) as f64 + 0.5) / 4503599627370496.0
}
fn normal(rng: &mut impl RngCore) -> f64 {
    libm::sqrt(-2.0 * libm::log(unit_open(rng)))
        * libm::cos(core::f64::consts::TAU * unit_open(rng))
}

fn sample_cdf(cdf: &[(u32, usize, usize)], rng: &mut impl RngCore) -> usize {
    let roll = bounded(rng, 10_000);
    for &(cum, lo, hi) in cdf {
        if roll < cum {
            let span = hi.saturating_sub(lo).saturating_add(1) as u32;
            return lo.saturating_add(bounded(rng, span) as usize);
        }
    }
    let &(_, lo, hi) = cdf.last().expect("non-empty CDF");
    let span = hi.saturating_sub(lo).saturating_add(1) as u32;
    lo.saturating_add(bounded(rng, span) as usize)
}

pub struct Shaper {
    config: Config,
    direction: Direction,
    last_real: u64,
    window_start: u64,
    recent_bytes: usize,
}
impl Shaper {
    pub fn new(config: Config, now_ms: u64) -> Self {
        Self::new_for_direction(config, now_ms, Direction::Downlink)
    }
    pub fn new_for_direction(mut config: Config, now_ms: u64, direction: Direction) -> Self {
        config.normalize();
        Self {
            config,
            direction,
            last_real: now_ms,
            window_start: now_ms,
            recent_bytes: 0,
        }
    }
    fn cdf(&self) -> &'static [(u32, usize, usize)] {
        match self.direction {
            Direction::Uplink => UPLINK_CDF,
            Direction::Downlink => DOWNLINK_CDF,
        }
    }
    fn cap(&self) -> usize {
        self.config
            .fastdl_chunk_size
            .min(self.direction.max_udp())
            .min(VALVE_SDR_MAX_UDP)
    }
    pub fn plan(&mut self, len: usize, now_ms: u64, rng: &mut impl RngCore) -> (u8, usize) {
        self.last_real = now_ms;
        if now_ms.saturating_sub(self.window_start) > 250 {
            self.recent_bytes = 0;
            self.window_start = now_ms;
        }
        self.recent_bytes = self.recent_bytes.saturating_add(len);
        let min = len.saturating_add(OVERHEAD);
        if !self.config.enabled {
            return (COMBAT, min);
        }
        let cap = self.cap();
        let is_fastdl = len > 480 || self.recent_bytes > 6000;
        if is_fastdl {
            if min >= cap {
                return (FAST_DL, min);
            }
            // Prevent uplink ACK amplification during heavy downloads: keep tiny KCP ACK/control
            // segments compact instead of bloating every ACK to 1248B.
            if len <= 96 {
                let jitter = bounded(rng, 64) as usize;
                let target = min.saturating_add(jitter).min(256).min(cap);
                return (FAST_DL, target);
            }
            // Sample from the real CS2 empirical distribution above `min` rather than pinning
            // 85% of frames to a single constant size.
            let sample = sample_cdf(self.cdf(), rng).min(cap);
            let target = if sample >= min {
                sample
            } else {
                let headroom = cap.saturating_sub(min);
                if headroom == 0 {
                    min
                } else {
                    min.saturating_add(bounded(rng, (headroom.min(192) + 1) as u32) as usize)
                        .min(cap)
                }
            };
            return (FAST_DL, target);
        }

        // Combat / normal interactive traffic: sample directly from real CS2 empirical profile.
        let sample = sample_cdf(self.cdf(), rng).min(cap);
        let target = if sample >= min {
            sample
        } else {
            let headroom = cap.saturating_sub(min);
            if headroom == 0 {
                min
            } else {
                min.saturating_add(bounded(rng, (headroom.min(64) + 1) as u32) as usize)
                    .min(cap)
            }
        };
        (COMBAT, target)
    }
    pub fn dummy(&mut self, now_ms: u64, rng: &mut impl RngCore) -> Option<usize> {
        let idle = now_ms.saturating_sub(self.last_real);
        if !self.config.enabled
            || idle < HEARTBEAT_POLL_MS
            || idle > self.config.idle_sleep_seconds.saturating_mul(1000)
        {
            return None;
        }
        // Keep idle heartbeats lightweight while still varying naturally like real CS2 ticks.
        Some(libm::round(72.0 + normal(rng) * 14.0).clamp(48.0, 128.0) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_rng::TestRng;
    #[test]
    fn idle_and_window_boundaries() {
        let mut rng = TestRng::new(&[]);
        let mut s = Shaper::new(
            Config {
                enabled: true,
                idle_sleep_seconds: 1,
                ..Default::default()
            },
            0,
        );
        assert!(s.dummy(15, &mut rng).is_none());
        for t in [16, 46, 1000] {
            assert!((48..=128).contains(&s.dummy(t, &mut rng).unwrap()));
        }
        assert!(s.dummy(1001, &mut rng).is_none());
        assert_eq!(s.plan(481, 1100, &mut rng).0, FAST_DL);
        assert!(s.dummy(1115, &mut rng).is_none());
        assert!(s.dummy(1116, &mut rng).is_some());
        for _ in 0..12 {
            assert_eq!(s.plan(480, 1500, &mut rng).0, COMBAT);
        }
        assert_eq!(s.plan(240, 1750, &mut rng).0, COMBAT); // exactly 6000
        assert_eq!(s.plan(1, 1750, &mut rng).0, FAST_DL);
        assert_eq!(s.plan(1, 1751, &mut rng).0, COMBAT);
    }
    #[test]
    fn empirical_distribution_eliminates_constant_spike_and_ack_bloat() {
        let mut rng = TestRng::new(&[]);
        let mut off = Shaper::new(Config::default(), 0);
        assert_eq!(off.plan(800, 20, &mut rng), (COMBAT, 828));
        assert_eq!(off.dummy(200, &mut rng), None);

        // Downlink FAST_DL should stay <= 1257B and NOT spike on a single constant size.
        let mut dn = Shaper::new_for_direction(
            Config {
                enabled: true,
                ..Default::default()
            },
            0,
            Direction::Downlink,
        );
        let mut at_1248 = 0usize;
        let mut sizes = alloc::vec::Vec::with_capacity(2000);
        for i in 0..2000 {
            let (m, n) = dn.plan(500, i, &mut rng);
            assert_eq!(m, FAST_DL);
            assert!((528..=DOWNLINK_MAX_UDP).contains(&n));
            if n == 1248 {
                at_1248 += 1;
            }
            sizes.push(n);
        }
        // Previously ~85% (1700/2000) hit 1248B; now must be spread naturally (<5%).
        assert!(at_1248 < 100, "unexpected spike at 1248B: {at_1248}");
        // Oversized unpadded payload passes through safely.
        assert_eq!(dn.plan(1500, 2001, &mut rng), (FAST_DL, 1528));

        // Uplink tiny ACK packets during FAST_DL must NOT be bloated to 1248B.
        let mut up = Shaper::new_for_direction(
            Config {
                enabled: true,
                ..Default::default()
            },
            0,
            Direction::Uplink,
        );
        // Trigger FAST_DL window
        let _ = up.plan(6500, 10, &mut rng);
        for i in 11..211 {
            let (m, n) = up.plan(24, i, &mut rng);
            assert_eq!(m, FAST_DL);
            assert!(n <= 256, "ACK bloated to {n}B during FAST_DL");
        }
    }
}
