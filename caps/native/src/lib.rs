//! Capabilities on a desktop OS (Linux/Mac): the OS CSPRNG, and the clock and memory store they
//! share with other backends. The single-file store is `ambit-caps-kv-native`.
#![forbid(unsafe_code)]

use ambit_caps::Random;

pub use ambit_caps_std::{MemKv, StdClock};

/// The OS CSPRNG.
pub struct OsRandom;

impl Random for OsRandom {
    fn fill(&self, buf: &mut [u8]) {
        getrandom::fill(buf).expect("OS random source unavailable");
    }
}
