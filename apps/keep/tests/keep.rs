//! keep, as a user sees it: command lines in, output and folders out.

use std::time::{Duration, UNIX_EPOCH};

use ambit_cli::{Args, Exit};
use ambit_cli_sim::{SimClock, SimDir, args, files, folders, run, run_on_terminal};

/// 2026-10-13, 12:00 UTC.
fn clock() -> SimClock {
    SimClock::at(UNIX_EPOCH + Duration::from_secs(1_791_892_800))
}

struct World {
    docs: SimDir,
    archive: SimDir,
}

impl World {
    fn new() -> Self {
        let docs = SimDir::new();
        docs.file("report.pdf", "v1").file("notes.txt", "hello");
        Self {
            docs,
            archive: SimDir::new(),
        }
    }

    fn app(&self, args: &Args, clock: SimClock) -> keep::Keep<SimClock, SimDir> {
        keep::init(
            clock,
            args.named(files(&[("docs", &self.docs)])),
            args.named(folders(&[("archive", &self.archive)])),
        )
    }

    fn keep(&self, line: &[&str]) -> ambit_cli_sim::Run<Exit> {
        let args = args(line);
        run(self.app(&args, clock()), &args, "")
    }
}

#[test]
fn files_are_kept_under_today() {
    let world = World::new();
    let run = world.keep(&[
        "keep",
        "add",
        "docs/report.pdf",
        "docs/notes.txt",
        "--into",
        "archive",
    ]);
    assert_eq!(run.done, Exit::SUCCESS, "{}", run.err);
    assert_eq!(
        run.out,
        "kept     docs/report.pdf as 2026-10-13/report.pdf\n\
         kept     docs/notes.txt as 2026-10-13/notes.txt\n"
    );
    assert_eq!(world.archive.read("2026-10-13/report.pdf").unwrap(), b"v1");
}

#[test]
fn nothing_is_overwritten() {
    let world = World::new();
    world.keep(&["keep", "add", "docs/report.pdf", "--into=archive"]);
    // The same file again stays as it is; a changed one gets a number.
    let again = world.keep(&["keep", "add", "docs/report.pdf", "--into=archive"]);
    assert_eq!(
        again.out,
        "already  docs/report.pdf as 2026-10-13/report.pdf\n"
    );
    world.docs.file("report.pdf", "v2");
    let changed = world.keep(&["keep", "add", "docs/report.pdf", "--into=archive"]);
    assert_eq!(
        changed.out,
        "kept     docs/report.pdf as 2026-10-13/report (2).pdf\n"
    );
    assert_eq!(world.archive.read("2026-10-13/report.pdf").unwrap(), b"v1");
    assert_eq!(
        world.archive.read("2026-10-13/report (2).pdf").unwrap(),
        b"v2"
    );
}

#[test]
fn listed_by_day_lined_up_on_a_terminal_and_tab_separated_in_a_pipe() {
    let world = World::new();
    world.archive.file("2026-10-12/old.txt", "123");
    world.keep(&["keep", "add", "docs/notes.txt", "--into", "archive"]);
    let piped = world.keep(&["keep", "list", "archive"]);
    assert_eq!(
        piped.out,
        "2026-10-12\told.txt\t3\n2026-10-13\tnotes.txt\t5\n"
    );
    let args = args(&["keep", "list", "archive"]);
    let shown = run_on_terminal(world.app(&args, clock()), &args, "", 80);
    assert_eq!(
        shown.out,
        "2026-10-12  old.txt             3\n2026-10-13  notes.txt           5\n"
    );
}

#[test]
fn a_missing_file_fails_alone() {
    let world = World::new();
    let run = world.keep(&[
        "keep",
        "add",
        "docs/gone.txt",
        "docs/notes.txt",
        "--into",
        "archive",
    ]);
    assert_eq!(run.done, Exit::FAILURE);
    assert!(run.err.starts_with("keep: docs/gone.txt: "), "{}", run.err);
    assert_eq!(run.out, "kept     docs/notes.txt as 2026-10-13/notes.txt\n");
}

#[test]
fn a_wrong_command_line_is_a_usage_error_and_help_is_an_answer() {
    let world = World::new();
    let wrong = world.keep(&["keep", "add", "docs/notes.txt"]);
    assert_eq!(wrong.done, Exit::USAGE);
    assert!(wrong.err.contains("--into <FOLDER>"), "{}", wrong.err);
    assert!(wrong.out.is_empty());
    let help = world.keep(&["keep", "--help"]);
    assert_eq!(help.done, Exit::SUCCESS);
    assert!(help.out.contains("never overwriting"), "{}", help.out);
}

/// The app opens what the user named, and nothing else, even where the wiring could.
#[test]
fn only_what_the_user_named_is_opened() {
    let world = World::new();
    let run = world.keep(&["keep", "list", "elsewhere"]);
    assert_eq!(run.done, Exit::FAILURE);
    assert_eq!(run.err, "keep: Cannot open elsewhere: not found\n");

    // An app asking for a folder the user didn't type gets nothing, even one the wiring knows.
    let args = args(&["keep", "list", "docs"]);
    let named = args.named(folders(&[("archive", &world.archive)]));
    assert_eq!(
        named.open("archive").map(drop).unwrap_err().to_string(),
        "archive: not named on the command line"
    );
}
