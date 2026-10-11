//! `Dir` on Linux/Mac, via cap-std: the tree is an open directory handle, and every path is
//! resolved relative to it by cap-std, which refuses `..` and symlinks that lead outside. No
//! check-then-open gap: what was checked is what gets opened.
//!
//! Writing goes through a handle on the parent directory, opened the same way, and names inside
//! it are single components: a rename there can't land outside the tree.
#![forbid(unsafe_code)]

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ambit_caps_dir::{
    Dir, DirError, DirErrorKind, DirModify, DirWrite, Entry, File, Meta, RelPath, STAGING_PREFIX,
};
use cap_std::fs::OpenOptions;

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
        io::ErrorKind::AlreadyExists => DirErrorKind::Exists,
        io::ErrorKind::StorageFull => DirErrorKind::Full,
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

impl NativeDir {
    /// The directory holding `path`, and `path`'s name in it.
    fn parent<'p>(&self, path: &'p RelPath) -> Result<(cap_std::fs::Dir, &'p str), DirError> {
        let (parent, name) = path
            .split_last()
            .ok_or_else(|| DirError::new(DirErrorKind::Invalid, "the tree itself is not a file"))?;
        let dir = self
            .dir
            .open_dir(rel(&parent))
            .map_err(|e| err(&parent, e))?;
        Ok((dir, name))
    }

    /// Writes `contents` to a new hidden file in `dir`, flushed and read back. Returns its name.
    fn stage(
        &self,
        dir: &cap_std::fs::Dir,
        path: &RelPath,
        contents: &[u8],
    ) -> Result<String, DirError> {
        static COUNT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let (name, mut file) = loop {
            let name = format!(
                "{STAGING_PREFIX}{nanos:x}-{:x}.part",
                COUNT.fetch_add(1, Ordering::Relaxed)
            );
            match dir.open_with(&name, &options) {
                Ok(file) => break (name, file),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(err(path, e)),
            }
        };
        let written = file
            .write_all(contents)
            .and_then(|()| file.sync_all())
            .and_then(|()| {
                drop(file);
                let mut back = Vec::with_capacity(contents.len());
                dir.open(&name)?.read_to_end(&mut back)?;
                if back == contents {
                    Ok(())
                } else {
                    Err(io::Error::other("read back differs from what was written"))
                }
            });
        match written {
            Ok(()) => Ok(name),
            Err(e) => {
                let _ = dir.remove_file(&name);
                Err(err(path, e))
            }
        }
    }
}

/// Flushes a directory's entries, so a rename in it survives a crash. Best effort: not every
/// file system can.
fn sync_dir(dir: &cap_std::fs::Dir) {
    let _ = rustix::fs::fsync(dir);
}

impl DirWrite for NativeDir {
    fn create_dir(&self, path: &RelPath) -> Result<(), DirError> {
        if path.is_root() {
            return Ok(());
        }
        // A file in the way: create_dir_all says AlreadyExists, or nothing when it's the last part.
        match self.dir.create_dir_all(rel(path)).map_err(|e| err(path, e)) {
            Err(e) if e.kind != DirErrorKind::Exists => Err(e),
            _ if self.metadata(path).is_ok_and(|m| m.is_dir) => Ok(()),
            _ => Err(DirError::new(
                DirErrorKind::WrongKind,
                format!("{path}: a file is in the way"),
            )),
        }
    }

    fn create_new(&self, path: &RelPath, contents: &[u8]) -> Result<(), DirError> {
        let (dir, name) = self.parent(path)?;
        let stage = self.stage(&dir, path, contents)?;
        // Atomic and refusing an existing name, also on FAT (Linux) and APFS. A file system
        // without it gets a hard link, which refuses an existing name too.
        let published = match rustix::fs::renameat_with(
            &dir,
            &stage,
            &dir,
            name,
            rustix::fs::RenameFlags::NOREPLACE,
        ) {
            Ok(()) => Ok(()),
            Err(e) if e == rustix::io::Errno::INVAL || e == rustix::io::Errno::NOSYS => dir
                .hard_link(&stage, &dir, name)
                .and_then(|()| dir.remove_file(&stage)),
            Err(e) => Err(e.into()),
        };
        if let Err(e) = published {
            let _ = dir.remove_file(&stage);
            return Err(err(path, e));
        }
        sync_dir(&dir);
        Ok(())
    }

    fn available_space(&self, path: &RelPath) -> Result<u64, DirError> {
        let dir = if path.is_root() {
            self.dir.try_clone()
        } else {
            self.dir.open_dir(rel(path))
        }
        .map_err(|e| err(path, e))?;
        let stat = rustix::fs::fstatvfs(&dir).map_err(|e| err(path, e.into()))?;
        Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
    }
}

impl DirModify for NativeDir {
    fn replace(&self, path: &RelPath, contents: &[u8]) -> Result<(), DirError> {
        let (dir, name) = self.parent(path)?;
        if dir.metadata(name).is_ok_and(|m| m.is_dir()) {
            return Err(DirError::new(
                DirErrorKind::WrongKind,
                format!("{path}: is a directory"),
            ));
        }
        let stage = self.stage(&dir, path, contents)?;
        if let Err(e) = dir.rename(&stage, &dir, name) {
            let _ = dir.remove_file(&stage);
            return Err(err(path, e));
        }
        sync_dir(&dir);
        Ok(())
    }

    fn remove_file(&self, path: &RelPath) -> Result<(), DirError> {
        let (dir, name) = self.parent(path)?;
        // Not following a symlink: removing one removes the link, never what it points to.
        let meta = dir.symlink_metadata(name).map_err(|e| err(path, e))?;
        if meta.is_dir() {
            return Err(DirError::new(
                DirErrorKind::WrongKind,
                format!("{path}: is a directory"),
            ));
        }
        dir.remove_file(name).map_err(|e| err(path, e))?;
        sync_dir(&dir);
        Ok(())
    }
}
