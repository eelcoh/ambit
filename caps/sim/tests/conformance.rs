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

#[test]
fn dir() {
    let dir = ambit_caps_sim::SimDir::new();
    for (path, contents) in ambit_caps_conformance::DIR_FILES {
        dir.file(path, *contents);
    }
    dir.dir("empty");
    ambit_caps_conformance::dir(&dir);
}

#[test]
fn dir_modify() {
    let dir = ambit_caps_sim::SimDir::new();
    for (path, contents) in ambit_caps_conformance::DIR_FILES {
        dir.file(path, *contents);
    }
    dir.dir("empty");
    ambit_caps_conformance::dir_modify(&dir);
}

#[test]
fn dir_space_runs_out() {
    use ambit_caps_dir::{DirErrorKind, DirModify, DirWrite, RelPath};

    let dir = ambit_caps_sim::SimDir::new();
    dir.space(10);
    let path = |p: &str| RelPath::parse(p).unwrap();
    dir.create_new(&path("a"), b"123456").unwrap();
    assert_eq!(dir.available_space(&RelPath::root()).unwrap(), 4);
    let e = dir.create_new(&path("b"), b"12345").unwrap_err();
    assert_eq!(e.kind, DirErrorKind::Full);
    assert_eq!(dir.read("b"), None);
    dir.replace(&path("a"), b"1").unwrap();
    assert_eq!(dir.available_space(&RelPath::root()).unwrap(), 9);
    dir.remove_file(&path("a")).unwrap();
    assert_eq!(dir.available_space(&RelPath::root()).unwrap(), 10);
}

#[test]
fn dir_unplugged() {
    use ambit_caps_dir::{Dir, DirErrorKind, DirWrite, RelPath};

    let dir = ambit_caps_sim::SimDir::new();
    dir.file("book.epub", "b");
    dir.plugged(false);
    let e = dir.list(&RelPath::root()).unwrap_err();
    assert_eq!(e.kind, DirErrorKind::Unavailable);
    let e = dir
        .create_new(&RelPath::parse("x").unwrap(), b"x")
        .unwrap_err();
    assert_eq!(e.kind, DirErrorKind::Unavailable);
    dir.plugged(true);
    assert_eq!(dir.list(&RelPath::root()).unwrap().len(), 1);
}
