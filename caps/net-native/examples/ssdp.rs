//! Searches the local network for Sonos players:
//! `cargo run -p ambit-caps-net-native --example ssdp`.

use std::time::Duration;

use ambit_caps_net::{Discovery, Targets};
use ambit_caps_net_native::NativeDiscovery;

fn main() {
    let target = "urn:schemas-upnp-org:device:ZonePlayer:1";
    let discovery = NativeDiscovery::new(Targets::new([target]));
    match discovery.ssdp(target, Duration::from_secs(2)) {
        Ok(devices) => {
            println!("{} device(s) for {target}", devices.len());
            for d in devices {
                println!("  {}  {}", d.addr, d.location.unwrap_or_default());
            }
        }
        Err(e) => println!("{e}"),
    }
}
