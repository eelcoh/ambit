//! Wiring: which capabilities the app gets, and with what configuration. Human-owned.
//!
//! On Linux the server is locked to its database and its port: no other files, no outgoing
//! connections, no other programs.

use std::time::Duration;

use ambit_lockdown::{Grant, Lockdown};
use ambit_server_native::{Listen, NativeKv, OsRandom, StdClock};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ambit_server_native::init_tracing();

    let config = shortener::Config {
        base_url: "http://localhost:8080".into(),
        ttl: Duration::from_secs(7 * 24 * 3600),
    };
    let kv = NativeKv::open("data/app.redb")?;
    let listen = Listen {
        addr: ([127, 0, 0, 1], 8080).into(),
        threads: 8,
        queue: 256,
    };
    Lockdown::new()
        .allow(&kv)
        .allow(&Grant::Listen(listen.addr.port()))
        .apply_or_warn("shortener")?;

    let app = shortener::init(StdClock::new(), OsRandom, kv, config);
    ambit_server_native::serve(app, listen)?;
    Ok(())
}
