//! The cli platform: what a command-line tool is written against.
//!
//! The platform owns the process: the arguments, stdin, stdout and stderr, and the exit code. The
//! app gets the arguments as text, reads and writes through [`Io`], and returns what it ended
//! with. It parses the arguments itself, with any parser that only computes.
//!
//! A path or an address the user types is the user's choice, and so the user's grant. The wiring
//! hands the app [`Named`] capabilities: each opens what the user named on the command line, as
//! the wiring decides (a folder to read, a folder to add to, a device on the local network), and
//! nothing else. An app can't make up a path and open it.
#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::fmt;
use std::io::{BufRead, Write};
use std::sync::Arc;

/// A command-line tool.
pub trait App {
    /// What the run ends with, handed back to the wiring: usually an [`Exit`], or something the
    /// wiring acts on next, such as "start the terminal interface with these settings".
    type Done;

    /// Runs once. `args` are the arguments as typed, the program name first.
    fn run(self, args: &[String], io: &mut Io<'_>) -> Self::Done;
}

/// The program's own input and output.
pub struct Io<'a> {
    pub input: &'a mut dyn BufRead,
    /// The result: what a pipe or a redirect gets.
    pub out: &'a mut dyn Write,
    /// Messages, progress and errors, so the result stays clean.
    pub err: &'a mut dyn Write,
    /// `Some` when `out` is a terminal; `None` when it goes to a pipe or a file.
    pub terminal: Option<Terminal>,
}

/// The terminal `out` is shown on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terminal {
    /// Columns, for laying out tables and wrapping help.
    pub width: u16,
    /// Whether colours are welcome (`NO_COLOR` unset).
    pub color: bool,
}

/// The process exit code. The wiring returns it from `main`: `ExitCode::from(exit.0)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit(pub u8);

impl Exit {
    pub const SUCCESS: Self = Self(0);
    pub const FAILURE: Self = Self(1);
    /// The command line itself was wrong.
    pub const USAGE: Self = Self(2);
}

/// The command line, as the wiring sees it: what the user named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    list: Vec<String>,
}

impl Args {
    /// The arguments, the program name first.
    pub fn new(list: Vec<String>) -> Self {
        Self { list }
    }

    pub fn list(&self) -> &[String] {
        &self.list
    }

    /// The values the user named: every argument after the program name, and the value of each
    /// `--option=value` before a `--`. Short options with the value attached (`-ofile`) don't
    /// count: write `-o file`.
    pub fn values(&self) -> BTreeSet<String> {
        let mut values = BTreeSet::new();
        let mut options = true;
        for arg in self.list.iter().skip(1) {
            values.insert(arg.clone());
            if arg == "--" {
                options = false;
            } else if options
                && let Some((_, value)) = arg.strip_prefix("--").and_then(|a| a.split_once('='))
            {
                values.insert(value.to_owned());
            }
        }
        values
    }

    /// The value of `--name` (`--name value` or `--name=value`, the last one if repeated),
    /// before a `--`: for the wiring to read what it needs before the app exists, like
    /// `--config`. A quick read, not the app's parse: the app still declares the option, so
    /// help shows it and a mistake is reported.
    pub fn option(&self, name: &str) -> Option<&str> {
        let flag = format!("--{name}");
        let mut found = None;
        let mut list = self.list.iter().skip(1);
        while let Some(arg) = list.next() {
            if arg == "--" {
                break;
            } else if *arg == flag {
                found = list.next().map(String::as_str);
            } else if let Some(value) = arg.strip_prefix(&flag).and_then(|r| r.strip_prefix('=')) {
                found = Some(value);
            }
        }
        found
    }

    /// A capability that opens, with `open`, only what the user named. `open` is the wiring's:
    /// it decides what a name becomes (a folder to read, a folder to add to, a scoped connection)
    /// and the app's `init` shows which.
    pub fn named<T>(
        &self,
        open: impl Fn(&str) -> Result<T, String> + Send + Sync + 'static,
    ) -> Named<T> {
        Named {
            values: Arc::new(self.values()),
            open: Arc::new(open),
        }
    }
}

/// The wiring's opener: a name in, a capability (or why not) out.
type Opener<T> = dyn Fn(&str) -> Result<T, String> + Send + Sync;

/// Opens what the user named on the command line, and nothing else. Made by the wiring with
/// [`Args::named`].
pub struct Named<T> {
    values: Arc<BTreeSet<String>>,
    open: Arc<Opener<T>>,
}

impl<T> Clone for Named<T> {
    fn clone(&self) -> Self {
        Self {
            values: self.values.clone(),
            open: self.open.clone(),
        }
    }
}

impl<T> Named<T> {
    /// Opens nothing: for an app run without this capability.
    pub fn none() -> Self {
        Self {
            values: Arc::default(),
            open: Arc::new(|value| Err(format!("{value}: not available"))),
        }
    }

    /// Also opens these values, as if the user had named them: defaults the wiring takes from
    /// the user's saved settings.
    pub fn also(mut self, values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut all = (*self.values).clone();
        all.extend(values.into_iter().map(Into::into));
        self.values = Arc::new(all);
        self
    }

    /// Whether `value` was named, so `open` may open it.
    pub fn is_named(&self, value: &str) -> bool {
        self.values.contains(value)
    }

    /// Opens `value`, exactly as the user typed it.
    pub fn open(&self, value: &str) -> Result<T, NamedError> {
        if !self.is_named(value) {
            return Err(NamedError::NotNamed(value.to_owned()));
        }
        (self.open)(value).map_err(|message| NamedError::Failed {
            value: value.to_owned(),
            message,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamedError {
    /// The user didn't name this. Nothing was opened.
    NotNamed(String),
    /// Named, but it couldn't be opened.
    Failed { value: String, message: String },
}

impl fmt::Display for NamedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotNamed(value) => write!(f, "{value}: not named on the command line"),
            Self::Failed { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for NamedError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Args {
        Args::new(list.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn only_what_was_typed_opens() {
        let named = args(&["keep", "a.txt", "--into=box", "-o", "out", "--", "--x=y"])
            .named(|value| Ok(value.to_uppercase()));
        for value in ["a.txt", "box", "--into=box", "out", "-o", "--x=y"] {
            assert_eq!(named.open(value).unwrap(), value.to_uppercase());
        }
        for value in ["keep", "y", "/", "a.txt/..", "", "b.txt"] {
            assert_eq!(
                named.open(value),
                Err(NamedError::NotNamed(value.to_owned())),
                "{value}"
            );
        }
        let named = named.also(["saved"]);
        assert!(named.is_named("saved"));
        assert!(!Named::<()>::none().is_named("a.txt"));
    }

    #[test]
    fn the_wiring_reads_an_option_first() {
        let a = args(&[
            "x",
            "--config",
            "a.json",
            "books",
            "--config=b.json",
            "--state",
            "s",
        ]);
        assert_eq!(a.option("config"), Some("b.json"));
        assert_eq!(a.option("state"), Some("s"));
        assert_eq!(a.option("state-dir"), None);
        assert_eq!(args(&["x", "--", "--config", "c"]).option("config"), None);
        assert_eq!(args(&["x", "--config"]).option("config"), None);
    }

    #[test]
    fn a_failed_open_says_why() {
        let named =
            args(&["keep", "gone"]).named(|value| Err::<(), _>(format!("{value}: no such folder")));
        assert_eq!(
            named.open("gone").unwrap_err().to_string(),
            "gone: no such folder"
        );
    }
}
