//! Wiring: which capabilities the app gets, and with what configuration. Human-owned.

use std::time::Duration;

use ambit_server_hermit::{EphemeralKv, HermitRandom, Listen, StdClock};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ambit_server_hermit::init_tracing();

    let config = shortener::Config {
        base_url: "http://localhost:8080".into(),
        ttl: Duration::from_secs(7 * 24 * 3600),
    };
    // No storage on hermit yet: links are lost when the VM stops.
    let kv = EphemeralKv::new();
    let app = shortener::init(StdClock::new(), HermitRandom::new()?, kv, config);

    ambit_server_hermit::serve(
        app,
        Listen {
            addr: ([0, 0, 0, 0], 8080).into(),
            threads: 4,
            queue: 64,
        },
    )?;
    Ok(())
}
