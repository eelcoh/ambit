//! Wiring for the keep example. Human-owned.
//!
//! What keep may reach: the files named on its command line (to read), the folders named there
//! (to read and add to), and nothing else. On Linux the operating system enforces that too.

use std::process::ExitCode;

use ambit_cli_native::{StdClock, open};
use ambit_lockdown::Lockdown;

fn main() -> ExitCode {
    let args = ambit_cli_native::args();
    let files = args.named(open::file);
    let folders = args.named(open::creating);

    // The folder after --into is made now, if it isn't there, so the lock can cover it.
    let lock = match args.option("into") {
        Some(into) => Lockdown::new().make(into),
        None => Ok(Lockdown::new()),
    }
    .map(|lock| lock.allow(&files.open_all()).allow(&folders.open_all()));
    match lock.and_then(Lockdown::apply) {
        Ok(locked) => {
            if let Some(warning) = locked.warning() {
                eprintln!("keep: {warning}");
            }
        }
        Err(e) => {
            eprintln!("keep: {e}");
            return ExitCode::FAILURE;
        }
    }

    let app = keep::init(StdClock::new(), files, folders);
    ExitCode::from(ambit_cli_native::run(app, &args).0)
}
