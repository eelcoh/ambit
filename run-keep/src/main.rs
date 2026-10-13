//! Wiring: what the keep example can open. Files the user names, to read one by one; folders
//! the user names, to add to (created when missing). Nothing else.

use std::process::ExitCode;

use ambit_cli_native::{StdClock, open};

fn main() -> ExitCode {
    let args = ambit_cli_native::args();
    let app = keep::init(
        StdClock::new(),
        args.named(open::file),
        args.named(open::creating),
    );
    ExitCode::from(ambit_cli_native::run(app, &args).0)
}
