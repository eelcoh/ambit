//! Runs the capability conformance suite inside the unikernel. Exits 0 when everything passes.
//! `scripts/hermit-conformance.sh` boots it in QEMU and checks the exit code.

use ambit_server_hermit::{EphemeralKv, HermitRandom, StdClock};

fn main() {
    ambit_caps_conformance::clock(&StdClock::new());
    println!("conformance: clock ok");
    ambit_caps_conformance::random(&HermitRandom::new().expect("entropy"));
    println!("conformance: random ok");
    ambit_caps_conformance::kv(&EphemeralKv::new());
    println!("conformance: kv ok");
}
