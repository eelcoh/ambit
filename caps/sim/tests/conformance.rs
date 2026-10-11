use std::time::{Duration, UNIX_EPOCH};

use ambit_caps_sim::{MemKv, SeededRandom, SimClock};

#[test]
fn clock() {
    ambit_caps_conformance::clock(&SimClock::at(
        UNIX_EPOCH + Duration::from_secs(1_700_000_000),
    ));
}

#[test]
fn random() {
    ambit_caps_conformance::random(&SeededRandom::new(42));
}

#[test]
fn kv() {
    ambit_caps_conformance::kv(&MemKv::new());
}
