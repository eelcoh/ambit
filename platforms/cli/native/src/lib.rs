//! Native backend for the cli platform, on Linux/Mac: the process's own arguments, stdin, stdout
//! and stderr. Openers for what the user names are with the capabilities they open: folders and
//! files in `ambit_caps_dir_native::open`.
#![forbid(unsafe_code)]

pub use ambit_caps_native::{MemKv, OsRandom, StdClock};
use ambit_cli::{App, Args, Io, Terminal};
use std::io::{self, IsTerminal, Write};

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
