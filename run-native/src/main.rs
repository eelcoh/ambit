//! Wiring: which capabilities the app gets, and with what configuration. Human-owned.

use std::time::Duration;

use ambit_server_native::{Listen, NativeKv, OsRandom, StdClock};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ambit_server_native::init_tracing();

    let config = shortener::Config {
        base_url: "http://localhost:8080".into(),
        ttl: Duration::from_secs(7 * 24 * 3600),
    };
    let kv = NativeKv::open("data/app.redb")?;
    let app = shortener::init(StdClock::new(), OsRandom, kv, config);

    ambit_server_native::serve(
        app,
        Listen {
            addr: ([127, 0, 0, 1], 8080).into(),
            threads: 8,
            queue: 256,
        },
    )?;
    Ok(())
}
