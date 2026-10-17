//! The lock, seen from inside a locked program: what it may do works, the rest is refused.
//!
//! A lock can't be lifted, so each case runs in a child process: this test binary again, with
//! `AMBIT_LOCKDOWN_CASE` naming the case. The child locks itself, tries, and prints what
//! happened; the parent checks the lines.

#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::Command;

use ambit_lockdown::{Grant, Lockdown, Locked};

const CASE: &str = "AMBIT_LOCKDOWN_CASE";

/// A fresh folder for one case: `docs/` (a.txt, b.txt), `box/`, and `outside.txt`.
fn world(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ambit-lockdown-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("docs")).unwrap();
    std::fs::create_dir_all(dir.join("box")).unwrap();
    std::fs::write(dir.join("docs/a.txt"), "a").unwrap();
    std::fs::write(dir.join("docs/b.txt"), "b").unwrap();
    std::fs::write(dir.join("outside.txt"), "o").unwrap();
    dir
}

/// Runs `case` locked in a child and returns its lines.
fn locked(case: &str, dir: &Path, extra: &[(&str, String)]) -> Vec<String> {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env(CASE, case)
        .env("AMBIT_LOCKDOWN_DIR", dir)
        .envs(extra.iter().map(|(k, v)| (*k, v)))
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{case}: {out}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    out.lines()
        // The test runner may print its own words on the same line first.
        .filter_map(|l| l.split_once("> ").map(|(_, said)| said))
        .map(str::to_string)
        .collect()
}

fn ok_or_refused<T>(result: std::io::Result<T>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => "refused",
        Err(e) => panic!("neither ok nor refused: {e}"),
    }
}

/// The child: one case, locked. Does nothing in an ordinary test run.
#[test]
fn child() {
    let Ok(case) = std::env::var(CASE) else {
        return;
    };
    let dir = PathBuf::from(std::env::var("AMBIT_LOCKDOWN_DIR").unwrap());
    let port = |name: &str| -> u16 { std::env::var(name).unwrap().parse().unwrap() };
    let say = |what: &str, result: &str| println!("> {what}: {result}");
    let lock = |lockdown: Lockdown| {
        let locked = lockdown.apply().unwrap();
        assert_ne!(locked, Locked::Not(String::new()));
        say(
            "locked",
            if locked == Locked::Fully {
                "fully"
            } else {
                "partly"
            },
        );
    };
    match case.as_str() {
        "files" => {
            lock(
                Lockdown::new()
                    .allow(&Grant::Read(dir.join("docs/a.txt")))
                    .allow(&Grant::ReadWrite(dir.join("box"))),
            );
            say(
                "read the granted file",
                ok_or_refused(std::fs::read(dir.join("docs/a.txt"))),
            );
            say(
                "read its neighbour",
                ok_or_refused(std::fs::read(dir.join("docs/b.txt"))),
            );
            say(
                "list its folder",
                ok_or_refused(std::fs::read_dir(dir.join("docs"))),
            );
            say(
                "change the granted file",
                ok_or_refused(std::fs::write(dir.join("docs/a.txt"), "x")),
            );
            say(
                "write in the folder",
                ok_or_refused(std::fs::write(dir.join("box/new.txt"), "n")),
            );
            say(
                "make a folder in it",
                ok_or_refused(std::fs::create_dir(dir.join("box/sub"))),
            );
            say(
                "read outside",
                ok_or_refused(std::fs::read(dir.join("outside.txt"))),
            );
            say(
                "write outside",
                ok_or_refused(std::fs::write(dir.join("new.txt"), "n")),
            );
            say(
                "read /etc/passwd",
                ok_or_refused(std::fs::read("/etc/passwd")),
            );
        }
        "network" => {
            lock(Lockdown::new().allow(&Grant::Connect(port("ALLOWED"))));
            let connect = |p: u16| ok_or_refused(TcpStream::connect(("127.0.0.1", p)));
            say("connect to the granted port", connect(port("ALLOWED")));
            say("connect to another port", connect(port("OTHER")));
            say("listen", ok_or_refused(TcpListener::bind("127.0.0.1:0")));
        }
        "nothing" => {
            lock(Lockdown::new());
            let connect = ok_or_refused(TcpStream::connect(("127.0.0.1", port("ALLOWED"))));
            say("connect", connect);
            say("udp", ok_or_refused(UdpSocket::bind("127.0.0.1:0")));
            say(
                "start a program",
                ok_or_refused(Command::new("/bin/true").status()),
            );
            say(
                "read the current folder",
                ok_or_refused(std::fs::read_dir(".")),
            );
        }
        other => panic!("no case {other}"),
    }
}

#[test]
fn only_granted_files_and_folders() {
    let dir = world("files");
    assert_eq!(
        locked("files", &dir, &[])[1..],
        [
            "read the granted file: ok",
            "read its neighbour: refused",
            "list its folder: refused",
            "change the granted file: refused",
            "write in the folder: ok",
            "make a folder in it: ok",
            "read outside: refused",
            "write outside: refused",
            "read /etc/passwd: refused",
        ]
    );
    assert_eq!(std::fs::read(dir.join("docs/a.txt")).unwrap(), b"a");
    assert!(!dir.join("new.txt").exists());
}

#[test]
fn only_granted_ports() {
    let dir = world("network");
    let allowed = TcpListener::bind("127.0.0.1:0").unwrap();
    let other = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = |l: &TcpListener| l.local_addr().unwrap().port().to_string();
    let lines = locked(
        "network",
        &dir,
        &[("ALLOWED", port(&allowed)), ("OTHER", port(&other))],
    );
    // Landlock's TCP rules need Linux 6.7; an older kernel locks files and system calls only.
    if lines[0] == "locked: partly" && lines[2] == "connect to another port: ok" {
        eprintln!("this kernel doesn't lock TCP ports: skipped");
        return;
    }
    assert_eq!(
        lines[1..],
        [
            "connect to the granted port: ok",
            "connect to another port: refused",
            "listen: refused",
        ]
    );
}

#[test]
fn nothing_granted_means_no_network_and_no_programs() {
    let dir = world("nothing");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let lines = locked("nothing", &dir, &[("ALLOWED", port)]);
    assert_eq!(
        lines[1..],
        [
            "connect: refused",
            "udp: refused",
            "start a program: refused",
            "read the current folder: refused",
        ]
    );
    // Keeps the listener alive until the child is done.
    let mut buf = [0u8; 1];
    listener.set_nonblocking(true).unwrap();
    let _ = listener.accept().map(|(mut s, _)| s.read(&mut buf));
    let _ = std::io::stdout().flush();
}
