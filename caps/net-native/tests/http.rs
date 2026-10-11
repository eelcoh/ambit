//! NativeHttp against a local one-shot server: scope first, then a real round trip.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use ambit_caps_net::{Http, HttpErrorKind, Scope, http};
use ambit_caps_net_native::{HttpOptions, NativeHttp};

/// Answers one connection with `response`, returns what it received.
fn serve_once(response: &'static [u8]) -> (u16, thread::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut received = Vec::new();
        let mut buf = [0; 4096];
        while !received.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = conn.read(&mut buf).unwrap();
            received.extend_from_slice(&buf[..n]);
        }
        conn.write_all(response).unwrap();
        received
    });
    (port, handle)
}

fn client(port: u16, max_body: u64) -> NativeHttp {
    let scope = Scope::parse([format!("http://127.0.0.1:{port}").as_str()]).unwrap();
    NativeHttp::new(
        scope,
        HttpOptions {
            max_body,
            ..HttpOptions::default()
        },
    )
}

fn get(uri: &str) -> ambit_caps_net::Request {
    http::Request::get(uri).body(Vec::new()).unwrap()
}

#[test]
fn round_trip_within_scope() {
    let (port, server) =
        serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Test: yes\r\n\r\nhello");
    let response = client(port, 1024)
        .send(get(&format!("http://127.0.0.1:{port}/path?q=1")))
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-test"], "yes");
    assert_eq!(response.body(), b"hello");
    let received = String::from_utf8(server.join().unwrap()).unwrap();
    assert!(
        received.starts_with("GET /path?q=1 HTTP/1.1\r\n"),
        "{received}"
    );
}

#[test]
fn redirects_come_back_to_the_app() {
    let (port, server) = serve_once(
        b"HTTP/1.1 302 Found\r\nLocation: https://evil.example/\r\nContent-Length: 0\r\n\r\n",
    );
    let response = client(port, 1024)
        .send(get(&format!("http://127.0.0.1:{port}/")))
        .unwrap();
    assert_eq!(response.status(), 302);
    server.join().unwrap();
}

#[test]
fn bodies_over_the_limit_fail() {
    let (port, server) = serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nhello world");
    let e = client(port, 10)
        .send(get(&format!("http://127.0.0.1:{port}/")))
        .unwrap_err();
    assert_eq!(e.kind, HttpErrorKind::TooLarge);
    server.join().unwrap();
}

#[test]
fn out_of_scope_is_denied_without_connecting() {
    // Nothing listens there; a connection attempt would be a Network error, not Denied.
    let e = client(9, 1024)
        .send(get("http://127.0.0.1:10/"))
        .unwrap_err();
    assert_eq!(e.kind, HttpErrorKind::Denied);
}

/// Needs the internet: `cargo test -p ambit-caps-net-native -- --ignored`.
#[test]
#[ignore]
fn real_https_and_certificate_checks() {
    let scope = Scope::parse(["https://www.rust-lang.org", "https://expired.badssl.com"]).unwrap();
    let http = NativeHttp::new(scope, HttpOptions::default());
    let ok = http.send(get("https://www.rust-lang.org/")).unwrap();
    // A TLS handshake and a real answer; a redirect counts (we never follow them).
    assert!(ok.status().as_u16() < 400, "{}", ok.status());
    let e = http.send(get("https://expired.badssl.com/")).unwrap_err();
    assert_eq!(e.kind, HttpErrorKind::Network, "{e}");
}
