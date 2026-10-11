//! `Dir` on Linux/Mac, via cap-std: the tree is an open directory handle, and every path is
//! resolved relative to it by cap-std, which refuses `..` and symlinks that lead outside. No
//! check-then-open gap: what was checked is what gets opened.
#![forbid(unsafe_code)]

use std::io;
use std::path::{Path, PathBuf};

use ambit_caps_dir::{Dir, DirError, DirErrorKind, Entry, File, Meta, RelPath};

pub struct NativeDir {
    dir: cap_std::fs::Dir,
}

impl NativeDir {
    /// Opens `root`, an existing directory, as the whole tree the app can see.
    pub fn open(root: impl AsRef<Path>) -> io::Result<Self> {
        let dir = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
        Ok(Self { dir })
    }
}

fn rel(path: &RelPath) -> PathBuf {
    if path.is_root() {
        PathBuf::from(".")
    } else {
        path.parts().iter().collect()
    }
}

fn err(path: &RelPath, e: io::Error) -> DirError {
    let kind = match e.kind() {
        io::ErrorKind::NotFound => DirErrorKind::NotFound,
        // cap-std reports a path that leads outside the tree as PermissionDenied.
        io::ErrorKind::PermissionDenied => DirErrorKind::Denied,
        io::ErrorKind::NotADirectory | io::ErrorKind::IsADirectory => DirErrorKind::WrongKind,
        _ => DirErrorKind::Io,
    };
    DirError::new(kind, format!("{path}: {e}"))
}

impl Dir for NativeDir {
    fn metadata(&self, path: &RelPath) -> Result<Meta, DirError> {
        let meta = self.dir.metadata(rel(path)).map_err(|e| err(path, e))?;
        Ok(Meta {
            is_dir: meta.is_dir(),
            len: meta.len(),
            modified: meta.modified().ok().map(|t| t.into_std()),
        })
    }

    fn open(&self, path: &RelPath) -> Result<Box<dyn File>, DirError> {
        let file = self.dir.open(rel(path)).map_err(|e| err(path, e))?;
        let meta = file.metadata().map_err(|e| err(path, e))?;
        if meta.is_dir() {
            return Err(DirError::new(
                DirErrorKind::WrongKind,
                format!("{path}: is a directory"),
            ));
        }
        Ok(Box::new(io::BufReader::new(file.into_std())))
    }

    fn list(&self, path: &RelPath) -> Result<Vec<Entry>, DirError> {
        let entries = if path.is_root() {
            self.dir.entries()
        } else {
            self.dir.read_dir(rel(path))
        }
        .map_err(|e| err(path, e))?;
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| err(path, e))?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            // Follows symlinks inside the tree; one pointing outside reads as a file that
            // can't be opened, so it's listed as not a directory.
            let is_dir = self
                .dir
                .metadata(rel(path).join(&name))
                .is_ok_and(|m| m.is_dir());
            out.push(Entry { name, is_dir });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }
}
