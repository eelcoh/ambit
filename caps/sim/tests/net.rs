//! SimHttp, SimDiscovery and SimWebSocket enforce the same scopes as the native ones.

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

#[test]
fn websocket_scope_server_and_record() {
    use ambit_caps_net::{WebSocket, WsMessage};
    use ambit_caps_sim::SimWebSocket;

    let ws = SimWebSocket::new(Scope::new([Origin::lan(Scheme::Ws, 81)]));
    ws.serve(|url| {
        url.starts_with("ws://192.168.1.7:81").then(|| {
            let mut received = 0;
            Box::new(move |message| match message {
                WsMessage::Text(t) if t.starts_with("START:") => {
                    vec![WsMessage::Text("READY".into())]
                }
                WsMessage::Binary(b) => {
                    received += b.len();
                    vec![WsMessage::Text(format!("PROGRESS:{received}"))]
                }
                _ => vec![],
            }) as ambit_caps_sim::WsServer
        })
    });

    let denied = ws.connect("ws://8.8.8.8:81/").map(drop).unwrap_err();
    assert_eq!(denied.kind, HttpErrorKind::Denied);
    let nobody = ws.connect("ws://192.168.1.8:81/").map(drop).unwrap_err();
    assert_eq!(nobody.kind, HttpErrorKind::Network);

    let mut conn = ws.connect("ws://192.168.1.7:81/").unwrap();
    conn.send(WsMessage::Text("START:a".into())).unwrap();
    assert_eq!(conn.recv().unwrap(), WsMessage::Text("READY".into()));
    conn.send(WsMessage::Binary(vec![0; 3])).unwrap();
    conn.send(WsMessage::Binary(vec![0; 4])).unwrap();
    assert_eq!(conn.recv().unwrap(), WsMessage::Text("PROGRESS:3".into()));
    assert_eq!(conn.recv().unwrap(), WsMessage::Text("PROGRESS:7".into()));
    assert_eq!(conn.recv().unwrap_err().kind, HttpErrorKind::Timeout);
    conn.close();
    assert_eq!(
        conn.send(WsMessage::Text("x".into())).unwrap_err().kind,
        HttpErrorKind::Closed
    );
    assert_eq!(ws.sent().len(), 3);
    assert_eq!(
        ws.sent()[0],
        (
            "ws://192.168.1.7:81/".to_string(),
            WsMessage::Text("START:a".into())
        )
    );
}
