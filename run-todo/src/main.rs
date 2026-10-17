//! Wiring: which capabilities the todo app gets on the native terminal backend. Human-owned.
//!
//! On Linux the program is locked to its database and the terminal: no other files, no network,
//! no other programs.

use std::path::PathBuf;

use ambit_caps_kv_native::NativeKv;
use ambit_lockdown::{Grant, Lockdown};
use ambit_tui_native::{Options, StdClock};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = data_dir().join("ambit-todo");
    std::fs::create_dir_all(&dir)?;
    // The log file is opened here, before the lock: writing to it needs no grant.
    ambit_tui_native::init_tracing(dir.join("todo.log"))?;
    let kv = NativeKv::open(dir.join("todo.redb"))?;
    Lockdown::new()
        .allow(&kv)
        .allow(&Grant::Terminal)
        .apply_or_warn("todo")?;
    ambit_tui_native::run(todo::init(StdClock::new(), kv), Options::default())?;
    Ok(())
}

fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME") {
        return dir.into();
    }
    let home = std::env::var_os("HOME").expect("HOME is set");
    PathBuf::from(home).join(".local/share")
}
