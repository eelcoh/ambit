//! Capabilities that need only Rust std, for every backend with a real clock (native, hermit).
#![forbid(unsafe_code)]

use std::time::{Duration, Instant, SystemTime};

use ambit_caps::Clock;

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
