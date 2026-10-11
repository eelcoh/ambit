//! Behaviour every backend's capabilities must have. Each backend runs these from its tests.
#![forbid(unsafe_code)]

use std::time::UNIX_EPOCH;

use ambit_caps::{Clock, Kv, Random};

pub fn clock(clock: &impl Clock) {
    let a = clock.elapsed();
    let b = clock.elapsed();
    assert!(b >= a, "monotonic time went backwards");
    assert!(clock.now() > UNIX_EPOCH, "wall clock before 1970");
}

pub fn random(random: &impl Random) {
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    random.fill(&mut a);
    random.fill(&mut b);
    assert_ne!(a, [0u8; 32], "random returned zeroes");
    assert_ne!(a, b, "random repeated itself");
}

/// Expects an empty store.
pub fn kv(kv: &impl Kv) {
    assert_eq!(kv.get(b"missing").unwrap(), None);

    kv.put(b"k", b"v1").unwrap();
    assert_eq!(kv.get(b"k").unwrap().as_deref(), Some(&b"v1"[..]));
    kv.put(b"k", b"v2").unwrap();
    assert_eq!(
        kv.get(b"k").unwrap().as_deref(),
        Some(&b"v2"[..]),
        "put overwrites"
    );

    kv.delete(b"k").unwrap();
    assert_eq!(kv.get(b"k").unwrap(), None);
    kv.delete(b"k").unwrap();

    // Binary keys, and keys on both sides of the prefix boundary.
    for key in [&b"a"[..], b"a\x00", b"a/2", b"a/1", b"a\xff", b"b", b"\x00"] {
        kv.put(key, key).unwrap();
    }
    let keys: Vec<Vec<u8>> = kv.scan(b"a").unwrap().into_iter().map(|(k, _)| k).collect();
    assert_eq!(
        keys,
        [&b"a"[..], b"a\x00", b"a/1", b"a/2", b"a\xff"],
        "scan: prefix, in key order"
    );
    let all = kv.scan(b"").unwrap();
    assert_eq!(all.len(), 7, "empty prefix scans everything");
    assert!(all.iter().all(|(k, v)| k == v));
}
