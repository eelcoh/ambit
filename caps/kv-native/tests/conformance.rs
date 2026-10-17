use ambit_caps_kv_native::NativeKv;

#[test]
fn kv() {
    let dir = std::env::temp_dir().join(format!("ambit-kv-{}", std::process::id()));
    let kv = NativeKv::open(dir.join("test.redb")).unwrap();
    ambit_caps_conformance::kv(&kv);
    drop(kv);
    std::fs::remove_dir_all(dir).unwrap();
}
