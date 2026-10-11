//! Wiring: which capabilities the todo app gets on the native terminal backend.

use std::path::PathBuf;

use ambit_tui_native::{NativeKv, Options, StdClock};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = data_dir().join("ambit-todo");
    std::fs::create_dir_all(&dir)?;
    ambit_tui_native::init_tracing(dir.join("todo.log"))?;
    let kv = NativeKv::open(dir.join("todo.redb"))?;
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
