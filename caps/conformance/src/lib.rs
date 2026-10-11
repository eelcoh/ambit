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

/// The tree every `Dir` under test must hold: files with their contents, and one empty
/// directory, `empty`.
pub const DIR_FILES: &[(&str, &str)] = &[
    ("two.txt", "two"),
    ("a/one.txt", "one"),
    ("a/b/deep.txt", "deep"),
];

/// Expects a `Dir` holding exactly [`DIR_FILES`] plus the empty directory `empty`.
pub fn dir(dir: &impl ambit_caps_dir::Dir) {
    use ambit_caps_dir::{DirErrorKind, Entry, RelPath};
    use std::io::{Read, Seek, SeekFrom};

    let path = |p: &str| RelPath::parse(p).unwrap();
    for (name, contents) in DIR_FILES {
        let mut text = String::new();
        dir.open(&path(name))
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(&text, contents, "{name}");
        let meta = dir.metadata(&path(name)).unwrap();
        assert!(!meta.is_dir, "{name}");
        assert_eq!(meta.len, contents.len() as u64, "{name}");
    }

    let mut file = dir.open(&path("a/b/deep.txt")).unwrap();
    file.seek(SeekFrom::Start(2)).unwrap();
    let mut rest = String::new();
    file.read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "ep");

    let entry = |name: &str, is_dir| Entry {
        name: name.to_string(),
        is_dir,
    };
    assert_eq!(
        dir.list(&RelPath::root()).unwrap(),
        [
            entry("a", true),
            entry("empty", true),
            entry("two.txt", false)
        ]
    );
    assert_eq!(
        dir.list(&path("a")).unwrap(),
        [entry("b", true), entry("one.txt", false)]
    );
    assert!(dir.list(&path("empty")).unwrap().is_empty());
    assert!(dir.metadata(&path("a")).unwrap().is_dir);
    assert!(dir.metadata(&RelPath::root()).unwrap().is_dir);

    let kind = |r: Result<(), ambit_caps_dir::DirError>| r.unwrap_err().kind;
    assert_eq!(
        kind(dir.open(&path("nope")).map(drop)),
        DirErrorKind::NotFound
    );
    assert_eq!(
        kind(dir.metadata(&path("a/nope")).map(drop)),
        DirErrorKind::NotFound
    );
    assert_eq!(
        kind(dir.open(&path("a")).map(drop)),
        DirErrorKind::WrongKind
    );
    assert_eq!(
        kind(dir.list(&path("two.txt")).map(drop)),
        DirErrorKind::WrongKind
    );
}
