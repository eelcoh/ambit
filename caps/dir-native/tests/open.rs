//! What a name on the command line opens as, on a real disk.

use std::path::PathBuf;

use ambit_caps_dir::{Dir, DirErrorKind, DirWrite, RelPath};
use ambit_caps_dir_native::open;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ambit-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn text(dir: &impl Dir, path: &RelPath) -> String {
    let mut s = String::new();
    std::io::Read::read_to_string(&mut dir.open(path).unwrap(), &mut s).unwrap();
    s
}

#[test]
fn a_named_file_opens_alone() {
    let dir = scratch("file");
    std::fs::write(dir.join("a.txt"), "mine").unwrap();
    std::fs::write(dir.join("b.txt"), "not mine").unwrap();
    let file = open::file(dir.join("a.txt").to_str().unwrap()).unwrap();
    assert_eq!(text(&file, &file.path()), "mine");
    let names: Vec<_> = file
        .list(&RelPath::root())
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["a.txt"]);
    let b = RelPath::new(["b.txt"]).unwrap();
    assert_eq!(file.metadata(&b).unwrap_err().kind, DirErrorKind::NotFound);
    assert_eq!(
        file.open(&b).map(drop).unwrap_err().kind,
        DirErrorKind::NotFound
    );
    assert!(open::file(dir.join("..").to_str().unwrap()).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_named_folder_is_made_on_the_first_write_and_a_missing_one_is_refused() {
    let dir = scratch("folder");
    let out = dir.join("new/books");
    assert!(open::folder(out.to_str().unwrap()).is_err());
    let creating = open::creating(out.to_str().unwrap()).unwrap();
    assert!(!out.exists(), "nothing is made before a write");
    creating
        .create_new(&RelPath::new(["x.txt"]).unwrap(), b"x")
        .unwrap();
    assert_eq!(std::fs::read(out.join("x.txt")).unwrap(), b"x");
    std::fs::remove_dir_all(&dir).unwrap();
}
