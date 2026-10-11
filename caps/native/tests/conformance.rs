use ambit_caps_native::{NativeKv, OsRandom, StdClock};

#[test]
fn clock() {
    ambit_caps_conformance::clock(&StdClock::new());
}

#[test]
fn random() {
    ambit_caps_conformance::random(&OsRandom);
}

#[test]
fn kv() {
    let dir = std::env::temp_dir().join(format!("ambit-kv-{}", std::process::id()));
    let kv = NativeKv::open(dir.join("test.redb")).unwrap();
    ambit_caps_conformance::kv(&kv);
    drop(kv);
    std::fs::remove_dir_all(dir).unwrap();
}
