//! Length selection and idle-heartbeat eligibility; the runtime schedules the 46 ms poll.
use rand_core::RngCore;
use serde::{Deserialize, Serialize};

use crate::gns::{COMBAT, FAST_DL, OVERHEAD};

pub const DUMMY_POLL_MS: u64 = 46;

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
            combat_mean_bytes: 210,
            fastdl_chunk_size: 1248,
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
        if self.fastdl_chunk_size == 0 {
            self.fastdl_chunk_size = d.fastdl_chunk_size;
        }
    }
}

/// Uniform integer in [0,bound). Reject the partial bin to keep all outcomes equiprobable.
pub(crate) fn bounded(rng: &mut impl RngCore, bound: u32) -> u32 {
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
pub struct Shaper {
    config: Config,
    last_real: u64,
    window_start: u64,
    recent_bytes: usize,
}
impl Shaper {
    pub fn new(mut config: Config, now_ms: u64) -> Self {
        config.normalize();
        Self {
            config,
            last_real: now_ms,
            window_start: now_ms,
            recent_bytes: 0,
        }
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
        if len > 480 || self.recent_bytes > 6000 {
            let chunk = self.config.fastdl_chunk_size;
            let size = if min > chunk {
                min
            } else if unit_open(rng) < 0.85 {
                chunk
            } else {
                min.saturating_add(bounded(rng, 64) as usize)
            };
            return (FAST_DL, size);
        }
        let sample =
            libm::round(self.config.combat_mean_bytes as f64 + normal(rng) * 38.0) as usize;
        (
            COMBAT,
            if sample < min {
                min.saturating_add(bounded(rng, 36) as usize)
            } else {
                sample
            },
        )
    }
    pub fn dummy(&mut self, now_ms: u64, rng: &mut impl RngCore) -> Option<usize> {
        let idle = now_ms.saturating_sub(self.last_real);
        if !self.config.enabled
            || idle < 35
            || idle > self.config.idle_sleep_seconds.saturating_mul(1000)
        {
            return None;
        }
        Some(libm::round(72.0 + normal(rng) * 10.0).clamp(54.0, 108.0) as usize)
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
        assert!(s.dummy(34, &mut rng).is_none());
        for t in [35, 46, 1000] {
            assert!((54..=108).contains(&s.dummy(t, &mut rng).unwrap()));
        }
        assert!(s.dummy(1001, &mut rng).is_none());
        assert_eq!(s.plan(481, 1100, &mut rng).0, FAST_DL);
        assert!(s.dummy(1134, &mut rng).is_none());
        assert!(s.dummy(1135, &mut rng).is_some());
        for _ in 0..12 {
            assert_eq!(s.plan(480, 1500, &mut rng).0, COMBAT);
        }
        assert_eq!(s.plan(240, 1750, &mut rng).0, COMBAT); // exactly 6000
        assert_eq!(s.plan(1, 1750, &mut rng).0, FAST_DL);
        assert_eq!(s.plan(1, 1751, &mut rng).0, COMBAT);
    }
    #[test]
    fn size_distributions_and_disabled() {
        let mut rng = TestRng::new(&[]);
        let mut off = Shaper::new(Config::default(), 0);
        assert_eq!(off.plan(800, 20, &mut rng), (COMBAT, 828));
        assert_eq!(off.dummy(200, &mut rng), None);
        let mut s = Shaper::new(
            Config {
                enabled: true,
                ..Default::default()
            },
            0,
        );
        let mut aligned = 0;
        for i in 0..2000 {
            let (m, n) = s.plan(500, i, &mut rng);
            assert_eq!(m, FAST_DL);
            if n == 1248 {
                aligned += 1;
            } else {
                assert!((528..592).contains(&n));
            }
        }
        assert!((1550..1850).contains(&aligned));
        assert_eq!(s.plan(1500, 2001, &mut rng), (FAST_DL, 1528));
    }
}
