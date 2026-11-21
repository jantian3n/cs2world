//! Deterministic RNG only compiled in tests.
use alloc::collections::VecDeque;
use rand_core::{CryptoRng, Error, RngCore};
pub struct TestRng {
    scripted: VecDeque<u8>,
    state: u64,
}
impl TestRng {
    pub fn new(prefix: &[u8]) -> Self {
        Self {
            scripted: prefix.iter().copied().collect(),
            state: 0x123456789abcdef,
        }
    }
}
impl RngCore for TestRng {
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }
    fn next_u64(&mut self) -> u64 {
        let mut s = self.state;
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        self.state = s;
        s
    }
    fn fill_bytes(&mut self, out: &mut [u8]) {
        for byte in out {
            *byte = self
                .scripted
                .pop_front()
                .unwrap_or_else(|| self.next_u32() as u8);
        }
    }
    fn try_fill_bytes(&mut self, out: &mut [u8]) -> Result<(), Error> {
        self.fill_bytes(out);
        Ok(())
    }
}
impl CryptoRng for TestRng {}
