use ambit_caps_native::{OsRandom, StdClock};

#[test]
fn clock() {
    ambit_caps_conformance::clock(&StdClock::new());
}

#[test]
fn random() {
    ambit_caps_conformance::random(&OsRandom);
}
