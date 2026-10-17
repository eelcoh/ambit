//! Wiring for the up example. Human-owned.
//!
//! What up may reach: the site (scheme, host and port) of each address typed on its command
//! line, and nothing else. A redirect to another site isn't followed. On Linux the operating
//! system also holds it to those ports, and to no files.

use std::process::ExitCode;

use ambit_caps_net::{Origin, Scope};
use ambit_caps_net_native::{HttpOptions, NativeHttp};
use ambit_cli_native::StdClock;
use ambit_lockdown::Lockdown;

fn main() -> ExitCode {
    let args = ambit_cli_native::args();
    let sites = args.named(|url| {
        Ok(NativeHttp::new(
            Scope::new([Origin::of(url)?]),
            HttpOptions::default(),
        ))
    });

    match Lockdown::new().allow(&sites.open_all()).apply() {
        Ok(locked) => {
            if let Some(warning) = locked.warning() {
                eprintln!("up: {warning}");
            }
        }
        Err(e) => {
            eprintln!("up: {e}");
            return ExitCode::FAILURE;
        }
    }

    let app = up::init(StdClock::new(), sites);
    ExitCode::from(ambit_cli_native::run(app, &args).0)
}
