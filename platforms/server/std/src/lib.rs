//! Shared by backends whose Rust std has real sockets and clocks: native and hermit.
//! A bounded, blocking HTTP/1.1 server.
#![forbid(unsafe_code)]

mod serve;

pub use ambit_caps_std::StdClock;
pub use serve::{Listen, MAX_BODY, MAX_HEAD, serve};
