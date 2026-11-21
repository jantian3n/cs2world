#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod a2s;
pub mod gns;
pub mod mux;
pub mod reliable;
pub mod shaper;
#[cfg(test)]
mod test_rng;
pub mod uri;
