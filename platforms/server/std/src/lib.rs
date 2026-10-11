//! Shared by backends whose Rust std has real sockets and clocks: native and hermit.
//! A bounded, blocking HTTP/1.1 server, and the std clock.
#![forbid(unsafe_code)]

mod serve;

use std::time::{Duration, Instant, SystemTime};

use ambit_caps::Clock;

pub use serve::{Listen, MAX_BODY, MAX_HEAD, serve};

/// The std clock: `SystemTime` for wall time, `Instant` for monotonic time.
pub struct StdClock {
    start: Instant,
}

impl StdClock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for StdClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for StdClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
    fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}
