//! Native backend for the cli platform, on Linux/Mac: the process's own arguments, stdin, stdout
//! and stderr, and openers that turn what the user named into folders and files.
#![forbid(unsafe_code)]

use std::io::{self, IsTerminal, Write};
use std::path::Path;

pub use ambit_caps_dir::OneFile;
pub use ambit_caps_dir_native::NativeDir;
pub use ambit_caps_native::{MemKv, NativeKv, OsRandom, StdClock};
use ambit_cli::{App, Args, Io, Terminal};

/// The command line. An argument that isn't valid UTF-8 ends the program here, with exit code 2:
/// an app could only see a mangled copy of it, which might name a different file.
pub fn args() -> Args {
    let mut list = Vec::new();
    for (index, arg) in std::env::args_os().enumerate() {
        match arg.into_string() {
            Ok(arg) => list.push(arg),
            Err(arg) => {
                eprintln!(
                    "argument {index} is not valid UTF-8: {}",
                    arg.to_string_lossy()
                );
                std::process::exit(2);
            }
        }
    }
    Args::new(list)
}

/// Runs the app once with this process's input and output, and hands back what it ended with.
/// Output is flushed before this returns.
pub fn run<A: App>(app: A, args: &Args) -> A::Done {
    let stdout = io::stdout();
    let terminal = stdout.is_terminal().then(|| Terminal {
        width: width(&stdout).unwrap_or(80),
        color: std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()),
    });
    let mut out = stdout.lock();
    let mut err = io::stderr().lock();
    let mut input = io::stdin().lock();
    let done = app.run(
        args.list(),
        &mut Io {
            input: &mut input,
            out: &mut out,
            err: &mut err,
            terminal,
        },
    );
    // A closed pipe (`| head`) is not the app's failure, and nothing is left to tell anyway.
    let _ = out.flush();
    done
}

fn width(stdout: &io::Stdout) -> Option<u16> {
    let size = rustix::termios::tcgetwinsize(stdout).ok()?;
    (size.ws_col > 0).then_some(size.ws_col)
}

/// What a name on the command line can open as, for [`Args::named`]. Relative paths are relative
/// to the directory the program was started in.
pub mod open {
    use super::*;

    /// A folder that is there now.
    pub fn folder(value: &str) -> Result<NativeDir, String> {
        NativeDir::open(value).map_err(|e| format!("Cannot open {value}: {e}"))
    }

    /// A folder opened again on every use: a card or a device that may come and go. Never fails
    /// here; while nothing is there, its calls fail with `Unavailable`.
    pub fn mount(value: &str) -> Result<NativeDir, String> {
        Ok(NativeDir::mount(value))
    }

    /// A folder created, with its parents, on the first write: where the app puts new things.
    pub fn creating(value: &str) -> Result<NativeDir, String> {
        Ok(NativeDir::creating(value))
    }

    /// One file, alone: the folder it's in stays out of reach.
    pub fn file(value: &str) -> Result<OneFile, String> {
        let path = Path::new(value);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("{value}: not a file name"))?;
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let folder = NativeDir::open(parent).map_err(|e| format!("Cannot open {value}: {e}"))?;
        OneFile::new(folder, name).map_err(|e| format!("{value}: {e}"))
    }
}
