use ambit_caps_std::{MemKv, StdClock};

#[test]
fn clock() {
    ambit_caps_conformance::clock(&StdClock::new());
}

#[test]
fn kv() {
    ambit_caps_conformance::kv(&MemKv::new());
}
