//! Simulation backend for the server platform. Tests drive the app by calling `Handler::handle`
//! directly, with the deterministic capabilities from `ambit-caps-sim`.
#![forbid(unsafe_code)]

pub use ambit_caps_sim::{Faults, MemKv, SeededRandom, SimClock};
use ambit_server::{Request, http};

pub fn get(uri: &str) -> Request {
    request("GET", uri, Vec::new())
}

pub fn post(uri: &str, body: impl Into<Vec<u8>>) -> Request {
    request("POST", uri, body.into())
}

pub fn request(method: &str, uri: &str, body: Vec<u8>) -> Request {
    http::Request::builder()
        .method(method)
        .uri(uri)
        .body(body)
        .expect("valid test request")
}
