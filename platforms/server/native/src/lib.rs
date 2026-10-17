//! Native backend for the server platform: development, tests and debugging on Linux/Mac.
#![forbid(unsafe_code)]

pub use ambit_caps_native::{MemKv, OsRandom, StdClock};
pub use ambit_server_std::{Listen, MAX_BODY, serve};

/// Installs the tracing subscriber. The platform owns it; the app only uses the facade.
pub fn init_tracing() {
    tracing_subscriber::fmt().init();
}
