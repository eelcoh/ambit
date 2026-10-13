//! Simulation backend for the cli platform: a test gives the command line and the input, and
//! reads back what the app wrote and what it ended with. Deterministic, with no process, no
//! terminal and no disk.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use ambit_caps_dir::OneFile;
pub use ambit_caps_sim::{MemKv, SeededRandom, SimClock, SimDir};
use ambit_cli::{App, Args, Io, Terminal};

/// The command line `list`, the program name first: `args(&["keep", "a.txt"])`.
pub fn args(list: &[&str]) -> Args {
    Args::new(list.iter().map(|s| s.to_string()).collect())
}

/// An opener for [`Args::named`] that knows these folders by name. Any other name isn't there.
pub fn folders(
    known: &[(&str, &SimDir)],
) -> impl Fn(&str) -> Result<SimDir, String> + Send + Sync + 'static {
    let known: BTreeMap<String, SimDir> = known
        .iter()
        .map(|(name, dir)| (name.to_string(), (*dir).clone()))
        .collect();
    move |value| {
        known
            .get(value)
            .cloned()
            .ok_or_else(|| format!("Cannot open {value}: not found"))
    }
}

/// An opener for files named as `"<folder>/<file>"` (or a bare `"<file>"` for the folder `""`):
/// each opens alone, out of the folder of that name.
pub fn files(
    known: &[(&str, &SimDir)],
) -> impl Fn(&str) -> Result<OneFile, String> + Send + Sync + 'static {
    let folders = folders(known);
    move |value| {
        let (folder, name) = value.rsplit_once('/').unwrap_or(("", value));
        OneFile::new(folders(folder)?, name).map_err(|e| format!("{value}: {e}"))
    }
}

/// What a run ended with, and what it wrote.
#[derive(Debug)]
pub struct Run<D> {
    pub done: D,
    pub out: String,
    pub err: String,
}

/// Runs the app once on `args` with `input` as stdin. Output goes to a pipe, as when redirected;
/// [`run_on_terminal`] shows it on a terminal instead.
pub fn run<A: App>(app: A, args: &Args, input: &str) -> Run<A::Done> {
    run_with(app, args, input, None)
}

/// Like [`run`], with output on a terminal `width` columns wide, colours off.
pub fn run_on_terminal<A: App>(app: A, args: &Args, input: &str, width: u16) -> Run<A::Done> {
    run_with(
        app,
        args,
        input,
        Some(Terminal {
            width,
            color: false,
        }),
    )
}

fn run_with<A: App>(app: A, args: &Args, input: &str, terminal: Option<Terminal>) -> Run<A::Done> {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let done = app.run(
        args.list(),
        &mut Io {
            input: &mut input.as_bytes(),
            out: &mut out,
            err: &mut err,
            terminal,
        },
    );
    Run {
        done,
        out: String::from_utf8_lossy(&out).into_owned(),
        err: String::from_utf8_lossy(&err).into_owned(),
    }
}
