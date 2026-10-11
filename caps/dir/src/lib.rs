//! A read-only directory capability: the app reads files inside one tree and can't name anything
//! outside it. The wiring picks the tree, e.g. from a command-line argument: the terminal's
//! version of a file picker (DESIGN.md, "powerbox").
//!
//! Read-only for now. Writing (for copying books to a reader) will be a separate capability, so
//! the signature of `init` shows which apps can change files.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirErrorKind {
    /// Not a valid path. Nothing was opened.
    Invalid,
    /// The path leads outside the tree (a symlink), or the OS refused. Nothing was opened.
    Denied,
    NotFound,
    /// A file where a directory was expected, or the other way round.
    WrongKind,
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
