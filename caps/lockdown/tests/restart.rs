//! Restarting a locked program: the supervisor starts it again, as often as it asks, and passes
//! on how it ended. Each case runs in a child (this test binary again), which is the supervisor.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::Command;

use ambit_lockdown::{Grant, Lockdown};

const CASE: &str = "AMBIT_RESTART_CASE";

/// The child: under its supervisor, locked to `dir`, counts its runs there and restarts until
/// the third, then exits with 7.
#[test]
fn child() {
    if std::env::var_os(CASE).is_none() {
        return;
    }
    ambit_lockdown::restartable();
    assert!(ambit_lockdown::can_restart());
    let dir = PathBuf::from(std::env::var_os(CASE).unwrap());
    Lockdown::new()
        .allow(&Grant::ReadWrite(dir.clone()))
        .apply()
        .unwrap();
    let count = dir.join("runs");
    let runs = std::fs::read_to_string(&count).map_or(0, |t| t.trim().parse().unwrap()) + 1;
    std::fs::write(&count, runs.to_string()).unwrap();
    if runs < 3 {
        panic!("{}", ambit_lockdown::restart());
    }
    println!("> runs: {runs}");
    std::process::exit(7);
}

#[test]
fn a_locked_program_starts_again_until_it_is_done() {
    let dir = std::env::temp_dir().join(format!("ambit-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env(CASE, &dir)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(7), "{out}\n{err}");
    assert!(out.contains("> runs: 3"), "{out}\n{err}");
    assert_eq!(std::fs::read_to_string(dir.join("runs")).unwrap(), "3");
}

#[test]
fn without_the_supervisor_there_is_no_restart() {
    assert!(!ambit_lockdown::can_restart());
    assert!(ambit_lockdown::restart().0.contains("restartable()"));
}
