//! NativeAuthorize: scope first; then the loopback redirect, driven by a fake browser.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

use ambit_caps_net::{AuthErrorKind, Authorize, Scope};
use ambit_caps_net_native::NativeAuthorize;

/// What a browser does after the user approves: GET the redirect URI. Retries until the
/// listener is up.
fn redirect(port: u16, target: &str) -> String {
    for _ in 0..100 {
        if let Ok(mut conn) = TcpStream::connect(("127.0.0.1", port)) {
            write!(conn, "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
            let mut page = String::new();
            conn.read_to_string(&mut page).unwrap();
            return page;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("listener never came up");
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn returns_the_decoded_callback_query_and_ignores_other_requests() {
    let port = free_port();
    let auth = NativeAuthorize::new(Scope::parse(["https://auth.invalid"]).unwrap(), port)
        .without_browser();
    assert_eq!(
        auth.redirect_uri(),
        format!("http://127.0.0.1:{port}/callback")
    );
    let browser = thread::spawn(move || {
        let favicon = redirect(port, "/favicon.ico");
        let done = redirect(port, "/callback?code=a%2Fb&state=s1");
        (favicon, done)
    });
    let params = auth
        .authorize(
            "https://auth.invalid/authorize?x=1",
            Duration::from_secs(10),
        )
        .unwrap();
    assert_eq!(
        params,
        [
            ("code".to_string(), "a/b".to_string()),
            ("state".to_string(), "s1".to_string())
        ]
    );
    let (favicon, done) = browser.join().unwrap();
    assert!(favicon.starts_with("HTTP/1.1 404"), "{favicon}");
    assert!(
        done.starts_with("HTTP/1.1 200") && done.contains("close this tab"),
        "{done}"
    );
}

#[test]
fn out_of_scope_urls_are_not_opened() {
    let auth = NativeAuthorize::new(
        Scope::parse(["https://accounts.spotify.com"]).unwrap(),
        free_port(),
    );
    for url in ["https://evil.example/", "file:///etc/passwd"] {
        let e = auth.authorize(url, Duration::from_secs(1)).unwrap_err();
        assert_eq!(e.kind, AuthErrorKind::Denied, "{url}");
    }
}

#[test]
fn times_out_without_a_redirect() {
    let auth = NativeAuthorize::new(Scope::parse(["https://auth.invalid"]).unwrap(), free_port())
        .without_browser();
    let e = auth
        .authorize("https://auth.invalid/", Duration::from_millis(200))
        .unwrap_err();
    assert_eq!(e.kind, AuthErrorKind::Timeout);
}

/// Under the lock, the browser still opens: its helper was started before. The locked program
/// itself can't start anything.
#[cfg(target_os = "linux")]
#[test]
fn the_browser_opens_from_a_locked_program() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    const CASE: &str = "AMBIT_AUTHORIZE_LOCKED";
    if let Some(dir) = std::env::var_os(CASE) {
        // The child: lock, ask for the page, and try to start a program directly.
        let dir = std::path::PathBuf::from(dir);
        let auth =
            NativeAuthorize::new(Scope::parse(["https://auth.invalid"]).unwrap(), free_port())
                .opening_with(dir.join("browser"));
        ambit_lockdown::Lockdown::new()
            .allow(&auth)
            .apply()
            .unwrap();
        let e = auth
            .authorize("https://auth.invalid/authorize", Duration::from_millis(300))
            .unwrap_err();
        assert_eq!(e.kind, AuthErrorKind::Timeout);
        assert!(
            Command::new("/bin/true").status().is_err(),
            "started a program"
        );
        println!("> done");
        return;
    }
    let dir = std::env::temp_dir().join(format!("ambit-authorize-locked-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let opened = dir.join("opened");
    let browser = dir.join("browser");
    std::fs::write(
        &browser,
        format!("#!/bin/sh\necho \"$1\" >> '{}'\n", opened.display()),
    )
    .unwrap();
    std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "the_browser_opens_from_a_locked_program",
            "--nocapture",
        ])
        .env(CASE, &dir)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && out.contains("> done"),
        "{out}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for _ in 0..200 {
        if std::fs::read_to_string(&opened).is_ok_and(|t| !t.is_empty()) {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::read_to_string(&opened).unwrap(),
        "https://auth.invalid/authorize\n"
    );
}
