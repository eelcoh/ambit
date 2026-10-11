//! SimHttp and SimDiscovery enforce the same scopes as the native ones.

use std::time::Duration;

use ambit_caps_net::{Discovery, Http, HttpErrorKind, Origin, Scheme, Scope, Targets, http};
use ambit_caps_sim::{SimDiscovery, SimHttp};

fn get(uri: &str) -> ambit_caps_net::Request {
    http::Request::get(uri).body(Vec::new()).unwrap()
}

#[test]
fn answers_in_scope_and_records_what_was_sent() {
    let sim = SimHttp::new(Scope::new([Origin::lan(Scheme::Http, 1400)]));
    sim.respond("GET", "http://192.168.1.20:1400/status", 200, "ok");
    let response = sim.send(get("http://192.168.1.20:1400/status")).unwrap();
    assert_eq!(response.body(), b"ok");
    assert_eq!(sim.sent()[0].uri, "http://192.168.1.20:1400/status");
}

#[test]
fn out_of_scope_is_denied_and_never_reaches_a_handler() {
    let sim = SimHttp::new(Scope::parse(["https://api.spotify.com"]).unwrap());
    sim.handle(|_| panic!("a denied request reached a handler"));
    let e = sim.send(get("https://evil.example/")).unwrap_err();
    assert_eq!(e.kind, HttpErrorKind::Denied);
    assert!(sim.sent().is_empty());
}

#[test]
fn unanswered_requests_and_injected_failures() {
    let sim = SimHttp::new(Scope::parse(["https://api.spotify.com"]).unwrap());
    let e = sim.send(get("https://api.spotify.com/v1/me")).unwrap_err();
    assert_eq!(e.kind, HttpErrorKind::Network);
    sim.respond("GET", "https://api.spotify.com/v1/me", 200, "{}");
    sim.fail(Some(HttpErrorKind::Timeout));
    let e = sim.send(get("https://api.spotify.com/v1/me")).unwrap_err();
    assert_eq!(e.kind, HttpErrorKind::Timeout);
    sim.fail(None);
    assert!(sim.send(get("https://api.spotify.com/v1/me")).is_ok());
}

#[test]
fn discovery_checks_targets() {
    let player = "urn:schemas-upnp-org:device:ZonePlayer:1";
    let sim = SimDiscovery::new(Targets::new([player]));
    let ip = "192.168.1.20".parse().unwrap();
    sim.add(
        player,
        ip,
        "http://192.168.1.20:1400/xml/device_description.xml",
    );
    assert_eq!(
        sim.ssdp(player, Duration::from_secs(1)).unwrap()[0].addr,
        ip
    );
    assert!(
        sim.ssdp("ssdp:all", Duration::from_secs(1))
            .unwrap_err()
            .denied
    );
    sim.remove(ip);
    assert!(sim.ssdp(player, Duration::from_secs(1)).unwrap().is_empty());
}

#[test]
fn authorize_echoes_state_and_checks_scope() {
    use ambit_caps_net::{AuthErrorKind, Authorize};
    use ambit_caps_sim::SimAuthorize;

    let auth = SimAuthorize::new(
        Scope::parse(["https://accounts.spotify.com"]).unwrap(),
        "http://127.0.0.1:8989/callback",
    );
    auth.approve(&[("code", "c1")]);
    let params = auth
        .authorize(
            "https://accounts.spotify.com/authorize?state=s9&x=1",
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(
        params,
        [
            ("code".to_string(), "c1".to_string()),
            ("state".to_string(), "s9".to_string())
        ]
    );
    let e = auth
        .authorize("https://evil.example/", Duration::from_secs(1))
        .unwrap_err();
    assert_eq!(e.kind, AuthErrorKind::Denied);
    let e = auth
        .authorize(
            "https://accounts.spotify.com/authorize",
            Duration::from_secs(1),
        )
        .unwrap_err();
    assert_eq!(e.kind, AuthErrorKind::Timeout, "nothing scripted");
    assert_eq!(auth.opened().len(), 2);
}
