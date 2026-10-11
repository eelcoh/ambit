use std::time::{Duration, UNIX_EPOCH};

use ambit_server::Handler;
use ambit_server::http::{StatusCode, header};
use ambit_server_sim::{Faults, MemKv, SeededRandom, SimClock, get, post};

const TTL: Duration = Duration::from_secs(3600);

fn setup(seed: u64) -> (impl Handler, SimClock, MemKv) {
    let clock = SimClock::at(UNIX_EPOCH + Duration::from_secs(1_700_000_000));
    let kv = MemKv::new();
    let config = shortener::Config {
        base_url: "https://s.test".into(),
        ttl: TTL,
    };
    let app = shortener::init(clock.clone(), SeededRandom::new(seed), kv.clone(), config);
    (app, clock, kv)
}

/// Creates a link; returns the id.
fn create(app: &impl Handler, url: &str) -> String {
    let response = app.handle(post("/links", url));
    assert_eq!(response.status(), StatusCode::CREATED);
    let short = String::from_utf8(response.into_body()).unwrap();
    short
        .trim()
        .strip_prefix("https://s.test/")
        .expect("short url uses base_url")
        .to_owned()
}

#[test]
fn created_link_redirects() {
    let (app, _, _) = setup(1);
    let id = create(&app, "https://example.com/some/page?q=1");
    let response = app.handle(get(&format!("/{id}")));
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(
        response.headers()[header::LOCATION],
        "https://example.com/some/page?q=1"
    );
}

#[test]
fn unknown_link_is_404() {
    let (app, _, _) = setup(1);
    assert_eq!(app.handle(get("/zzzzzzz")).status(), StatusCode::NOT_FOUND);
    assert_eq!(app.handle(get("/")).status(), StatusCode::NOT_FOUND);
}

#[test]
fn link_expires_after_ttl() {
    let (app, clock, _) = setup(1);
    let id = create(&app, "https://example.com");
    clock.advance(TTL - Duration::from_secs(1));
    assert_eq!(
        app.handle(get(&format!("/{id}"))).status(),
        StatusCode::FOUND
    );
    clock.advance(Duration::from_secs(1));
    assert_eq!(
        app.handle(get(&format!("/{id}"))).status(),
        StatusCode::GONE
    );
}

#[test]
fn invalid_url_is_rejected_and_not_stored() {
    let (app, _, kv) = setup(1);
    for body in [
        "",
        "not a url",
        "javascript:alert(1)",
        "https://x\r\nSet-Cookie: a=b",
    ] {
        assert_eq!(
            app.handle(post("/links", body)).status(),
            StatusCode::BAD_REQUEST,
            "{body:?}"
        );
    }
    assert!(kv.is_empty());
}

#[test]
fn ids_are_unique() {
    let (app, _, kv) = setup(7);
    let ids: std::collections::HashSet<_> = (0..200)
        .map(|i| create(&app, &format!("https://example.com/{i}")))
        .collect();
    assert_eq!(ids.len(), 200);
    assert_eq!(kv.len(), 200);
}

#[test]
fn same_seed_same_ids() {
    let (a, _, _) = setup(3);
    let (b, _, _) = setup(3);
    assert_eq!(
        create(&a, "https://example.com"),
        create(&b, "https://example.com")
    );
}

#[test]
fn storage_down_is_503_and_recovers() {
    let (app, _, kv) = setup(1);
    let id = create(&app, "https://example.com");
    kv.fail(Faults::All);
    assert_eq!(
        app.handle(post("/links", "https://example.org")).status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        app.handle(get(&format!("/{id}"))).status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    kv.fail(Faults::None);
    assert_eq!(
        app.handle(get(&format!("/{id}"))).status(),
        StatusCode::FOUND
    );
}

#[test]
fn failed_write_creates_nothing_but_reads_still_work() {
    let (app, _, kv) = setup(1);
    let id = create(&app, "https://example.com");
    kv.fail(Faults::Writes);
    assert_eq!(
        app.handle(post("/links", "https://example.org")).status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(kv.len(), 1);
    assert_eq!(
        app.handle(get(&format!("/{id}"))).status(),
        StatusCode::FOUND
    );
}
