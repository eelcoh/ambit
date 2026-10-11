use std::fs;
use std::path::PathBuf;

use ambit_caps_dir::{Dir, DirErrorKind, DirModify, DirWrite, RelPath};
use ambit_caps_dir_native::NativeDir;

/// A fresh directory under the system temp dir, removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("ambit-dir-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture(name: &str) -> Temp {
    let root = Temp::new(name);
    for (path, contents) in ambit_caps_conformance::DIR_FILES {
        let full = root.0.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, contents).unwrap();
    }
    fs::create_dir(root.0.join("empty")).unwrap();
    root
}

#[test]
fn conformance() {
    let root = fixture("conformance");
    ambit_caps_conformance::dir(&NativeDir::open(&root.0).unwrap());
}

#[test]
fn conformance_modify() {
    let root = fixture("modify");
    ambit_caps_conformance::dir_modify(&NativeDir::open(&root.0).unwrap());
}

#[cfg(unix)]
#[test]
fn writes_may_not_lead_outside() {
    use std::os::unix::fs::symlink;

    let outside = Temp::new("write-outside");
    fs::write(outside.0.join("secret"), "no").unwrap();
    let root = fixture("write-symlinks");
    symlink(outside.0.join("secret"), root.0.join("file-link")).unwrap();
    symlink(&outside.0, root.0.join("dir-link")).unwrap();
    let dir = NativeDir::open(&root.0).unwrap();
    let path = |p: &str| RelPath::parse(p).unwrap();

    for (what, result) in [
        ("create_new", dir.create_new(&path("dir-link/new"), b"x")),
        ("replace", dir.replace(&path("dir-link/secret"), b"x")),
        ("create_dir", dir.create_dir(&path("dir-link/new"))),
        ("remove_file", dir.remove_file(&path("dir-link/secret"))),
    ] {
        let e = result.unwrap_err();
        assert_eq!(e.kind, DirErrorKind::Denied, "{what}: {e}");
    }
    assert!(dir.available_space(&path("dir-link")).is_err());
    assert_eq!(
        dir.create_new(&path("file-link"), b"x").unwrap_err().kind,
        DirErrorKind::Exists
    );

    // Replacing or removing a link changes the link, never what it points to.
    dir.replace(&path("file-link"), b"inside").unwrap();
    dir.remove_file(&path("file-link")).unwrap();
    assert_eq!(fs::read_to_string(outside.0.join("secret")).unwrap(), "no");
    assert_eq!(fs::read_dir(&outside.0).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn symlinks_may_not_lead_outside() {
    use std::os::unix::fs::symlink;

    let outside = Temp::new("outside");
    fs::write(outside.0.join("secret"), "no").unwrap();
    let root = fixture("symlinks");
    symlink(outside.0.join("secret"), root.0.join("file-link")).unwrap();
    symlink(&outside.0, root.0.join("dir-link")).unwrap();
    symlink("a/one.txt", root.0.join("inside-link")).unwrap();
    let dir = NativeDir::open(&root.0).unwrap();

    for escape in ["file-link", "dir-link/secret"] {
        let path = RelPath::parse(escape).unwrap();
        let e = dir.open(&path).map(drop).unwrap_err();
        assert_eq!(e.kind, DirErrorKind::Denied, "{escape}: {e}");
        let e = dir.metadata(&path).map(drop).unwrap_err();
        assert_eq!(e.kind, DirErrorKind::Denied, "{escape}: {e}");
    }
    let e = dir.list(&RelPath::parse("dir-link").unwrap()).unwrap_err();
    assert_eq!(e.kind, DirErrorKind::Denied, "{e}");

    // A symlink that stays inside works.
    let mut text = String::new();
    std::io::Read::read_to_string(
        &mut dir.open(&RelPath::parse("inside-link").unwrap()).unwrap(),
        &mut text,
    )
    .unwrap();
    assert_eq!(text, "one");
}
