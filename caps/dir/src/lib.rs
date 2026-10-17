//! Directory capabilities: the app works with files inside one tree and can't name anything
//! outside it. The wiring picks the tree, e.g. from a command-line argument: the terminal's
//! version of a file picker (DESIGN.md, "powerbox").
//!
//! Three levels, so the signature of `init` shows what an app can do to the files it's given:
//!
//! - [`Dir`] reads.
//! - [`DirWrite`] also adds new files and directories, and never changes or removes what's there.
//! - [`DirModify`] also replaces and removes files.
#![forbid(unsafe_code)]

use std::fmt;
use std::io::{Read, Seek};
use std::sync::Arc;
use std::time::SystemTime;

/// A path inside the tree, as plain components: no separators, no `.` or `..`. Checked on
/// construction, so a `RelPath` can't point outside the tree by itself; symlinks are the
/// implementation's job.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct RelPath(Vec<String>);

impl RelPath {
    /// The directory holding this path, and its last component; `None` for the root.
    pub fn split_last(&self) -> Option<(RelPath, &str)> {
        let (last, parent) = self.0.split_last()?;
        Some((RelPath(parent.to_vec()), last))
    }

    /// The tree itself.
    pub fn root() -> Self {
        Self(Vec::new())
    }

    /// From components; at least one (use [`RelPath::root`] for the tree itself).
    pub fn new<S: Into<String>>(parts: impl IntoIterator<Item = S>) -> Result<Self, DirError> {
        let parts: Vec<String> = parts.into_iter().map(Into::into).collect();
        if parts.is_empty() {
            return Err(DirError::invalid("empty path"));
        }
        for part in &parts {
            if part.is_empty() || part == "." || part == ".." || part.contains(['/', '\\', '\0']) {
                return Err(DirError::invalid(format!("bad path component {part:?}")));
            }
        }
        Ok(Self(parts))
    }

    /// `"a/b.flac"`: components separated by `/`. Leading, trailing or double slashes are
    /// invalid, like `.` and `..`.
    pub fn parse(path: &str) -> Result<Self, DirError> {
        Self::new(path.split('/'))
    }

    pub fn parts(&self) -> &[String] {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// This path with `name` appended.
    pub fn join(&self, name: &str) -> Result<Self, DirError> {
        Self::new(self.0.iter().cloned().chain([name.to_string()]))
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            f.write_str(".")
        } else {
            f.write_str(&self.0.join("/"))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub is_dir: bool,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

/// One entry of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

/// An open file: read and seek, nothing else.
pub trait File: Read + Seek + Send {}
impl<T: Read + Seek + Send> File for T {}

/// Read-only access to one directory tree. Symlinks are followed only while they stay inside it.
pub trait Dir: Send + Sync + 'static {
    fn metadata(&self, path: &RelPath) -> Result<Meta, DirError>;
    fn open(&self, path: &RelPath) -> Result<Box<dyn File>, DirError>;
    /// The entries of a directory, sorted by name. Names that aren't valid UTF-8 are left out:
    /// a `RelPath` couldn't name them.
    fn list(&self, path: &RelPath) -> Result<Vec<Entry>, DirError>;
}

impl<T: Dir + ?Sized> Dir for Arc<T> {
    fn metadata(&self, path: &RelPath) -> Result<Meta, DirError> {
        (**self).metadata(path)
    }
    fn open(&self, path: &RelPath) -> Result<Box<dyn File>, DirError> {
        (**self).open(path)
    }
    fn list(&self, path: &RelPath) -> Result<Vec<Entry>, DirError> {
        (**self).list(path)
    }
}

/// One file, as a tree that holds only that file: what a file named on the command line opens
/// as. The folder it's in stays out of reach, apart from that one entry.
pub struct OneFile {
    folder: Box<dyn Dir>,
    name: RelPath,
    origin: Option<std::path::PathBuf>,
}

impl OneFile {
    /// The file `name` in `folder`. `name` is a single component.
    pub fn new(folder: impl Dir, name: &str) -> Result<Self, DirError> {
        Ok(Self {
            folder: Box::new(folder),
            name: RelPath::new([name])?,
            origin: None,
        })
    }

    /// Notes where the file is on this computer, for the wiring's lock (`ambit-lockdown`) to let
    /// the program read it. Set by the native wiring; it opens nothing.
    pub fn with_origin(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.origin = Some(path.into());
        self
    }

    /// Where the file is on this computer, if the wiring said.
    pub fn origin(&self) -> Option<&std::path::Path> {
        self.origin.as_deref()
    }

    /// The file's path inside this tree: its name.
    pub fn path(&self) -> RelPath {
        self.name.clone()
    }

    pub fn name(&self) -> &str {
        &self.name.0[0]
    }

    fn only(&self, path: &RelPath) -> Result<(), DirError> {
        if *path == self.name {
            Ok(())
        } else {
            Err(DirError::new(DirErrorKind::NotFound, path.to_string()))
        }
    }
}

impl Dir for OneFile {
    fn metadata(&self, path: &RelPath) -> Result<Meta, DirError> {
        if path.is_root() {
            return Ok(Meta {
                is_dir: true,
                len: 0,
                modified: None,
            });
        }
        self.only(path)?;
        self.folder.metadata(path)
    }
    fn open(&self, path: &RelPath) -> Result<Box<dyn File>, DirError> {
        self.only(path)?;
        self.folder.open(path)
    }
    /// The root lists the file, if it's there; the file itself has nothing below it to list.
    fn list(&self, path: &RelPath) -> Result<Vec<Entry>, DirError> {
        if !path.is_root() {
            self.only(path)?;
            return Err(DirError::new(DirErrorKind::WrongKind, path.to_string()));
        }
        Ok(match self.folder.metadata(&self.name) {
            Ok(meta) => vec![Entry {
                name: self.name().to_owned(),
                is_dir: meta.is_dir,
            }],
            Err(e) if e.kind == DirErrorKind::NotFound => vec![],
            Err(e) => return Err(e),
        })
    }
}

/// Adding to one directory tree. What's already there is never changed or removed: a book copied
/// onto a card can't overwrite one with the same name, whatever the app does.
///
/// New files are written under a hidden name starting with [`STAGING_PREFIX`] next to where they
/// go, and get their real name only when complete. An interrupted write can leave such a file
/// behind; it never shows up under the real name.
pub trait DirWrite: Dir {
    /// Creates the directory `path` and any missing parents. Already being a directory is fine;
    /// a file in the way is `WrongKind`.
    fn create_dir(&self, path: &RelPath) -> Result<(), DirError>;

    /// Creates the file `path` with `contents`, all or nothing: written aside, flushed to disk,
    /// read back and compared, and only then given its name, if nothing has that name yet
    /// (`Exists` otherwise; the existing file is untouched). The parent directory must exist.
    fn create_new(&self, path: &RelPath, contents: &[u8]) -> Result<(), DirError>;

    /// Free bytes on the volume holding the directory `path`, as far as this process may use them.
    fn available_space(&self, path: &RelPath) -> Result<u64, DirError>;
}

/// Name prefix of the files [`DirWrite::create_new`] and [`DirModify::replace`] write before they
/// get their real name. Apps can leave these out of listings.
pub const STAGING_PREFIX: &str = ".ambit-";

/// Also changing what's there: for files the app owns, like its settings, or when the user asks
/// for a file to be deleted.
pub trait DirModify: DirWrite {
    /// Sets the file `path` to `contents`, creating it if needed. All or nothing: after a crash
    /// the file has either its old or its new contents. A directory in the way is `WrongKind`.
    fn replace(&self, path: &RelPath, contents: &[u8]) -> Result<(), DirError>;

    /// Removes the file `path`. A directory is `WrongKind`: this never removes a tree.
    fn remove_file(&self, path: &RelPath) -> Result<(), DirError>;
}

impl<T: DirWrite + ?Sized> DirWrite for Arc<T> {
    fn create_dir(&self, path: &RelPath) -> Result<(), DirError> {
        (**self).create_dir(path)
    }
    fn create_new(&self, path: &RelPath, contents: &[u8]) -> Result<(), DirError> {
        (**self).create_new(path, contents)
    }
    fn available_space(&self, path: &RelPath) -> Result<u64, DirError> {
        (**self).available_space(path)
    }
}

impl<T: DirModify + ?Sized> DirModify for Arc<T> {
    fn replace(&self, path: &RelPath, contents: &[u8]) -> Result<(), DirError> {
        (**self).replace(path, contents)
    }
    fn remove_file(&self, path: &RelPath) -> Result<(), DirError> {
        (**self).remove_file(path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirErrorKind {
    /// Not a valid path. Nothing was opened.
    Invalid,
    /// The path leads outside the tree (a symlink), or the OS refused. Nothing was opened.
    Denied,
    NotFound,
    /// A file where a directory was expected, or the other way round.
    WrongKind,
    /// Something already has that name. Nothing was changed.
    Exists,
    /// The volume is full. Nothing was changed.
    Full,
    /// The tree isn't there right now, like a card that's unplugged. Nothing was changed.
    Unavailable,
    Io,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirError {
    pub kind: DirErrorKind,
    pub message: String,
}

impl DirError {
    pub fn new(kind: DirErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new(DirErrorKind::Invalid, message)
    }
}

impl fmt::Display for DirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "dir {:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for DirError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_cannot_climb_out() {
        for bad in [
            vec![],
            vec![""],
            vec!["."],
            vec![".."],
            vec!["a/b"],
            vec!["a\\b"],
            vec!["a\0"],
        ] {
            assert!(RelPath::new(bad.clone()).is_err(), "{bad:?}");
        }
        for bad in ["", "/a", "a/", "a//b", "../a", "a/./b"] {
            assert!(RelPath::parse(bad).is_err(), "{bad:?}");
        }
        assert_eq!(RelPath::parse("a/b.flac").unwrap().to_string(), "a/b.flac");
        assert_eq!(RelPath::root().join("x").unwrap().to_string(), "x");
        assert!(RelPath::root().join("..").is_err());
    }
}
