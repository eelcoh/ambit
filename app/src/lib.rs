//! Example app: a link shortener.
//!
//! - `POST /links` with a URL as body: creates a short link, `201` with the short URL.
//! - `GET /<id>`: `302` to the URL, `404` if unknown, `410` once expired.
#![forbid(
    unsafe_code,
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::print_stdout,
    clippy::print_stderr
)]

use std::time::{Duration, UNIX_EPOCH};

use ambit_caps::{Clock, Kv, KvError, Random};
use ambit_server::http::{HeaderValue, Method, StatusCode, header};
use ambit_server::{Handler, Request, Response};

pub struct Config {
    /// Prefix for the short URLs this app hands out, e.g. `https://ambit.link`.
    pub base_url: String,
    /// How long a link stays valid.
    pub ttl: Duration,
}

/// The app's capabilities: this signature is everything it can do.
pub fn init(clock: impl Clock, random: impl Random, kv: impl Kv, config: Config) -> impl Handler {
    Shortener {
        clock,
        random,
        kv,
        config,
    }
}

struct Shortener<C, R, K> {
    clock: C,
    random: R,
    kv: K,
    config: Config,
}

const MAX_URL: usize = 2048;
const ID_LEN: usize = 7;
const ID_ATTEMPTS: usize = 5;

impl<C: Clock, R: Random, K: Kv> Handler for Shortener<C, R, K> {
    fn handle(&self, req: Request) -> Response {
        let path = req.uri().path();
        let result = match (req.method(), path) {
            (&Method::POST, "/links") => self.create(req.body()),
            (&Method::GET, _) if is_id(&path[1..]) => self.resolve(&path[1..]),
            _ => Ok(text(StatusCode::NOT_FOUND, "not found\n")),
        };
        result.unwrap_or_else(|e| {
            tracing::error!(error = %e, "storage failed");
            text(StatusCode::SERVICE_UNAVAILABLE, "storage unavailable\n")
        })
    }
}

impl<C: Clock, R: Random, K: Kv> Shortener<C, R, K> {
    fn create(&self, body: &[u8]) -> Result<Response, KvError> {
        let Some(url) = valid_url(body) else {
            return Ok(text(
                StatusCode::BAD_REQUEST,
                "body must be an http(s) URL\n",
            ));
        };
        let Some(id) = store(&self.kv, &self.clock, &self.random, url)? else {
            return Ok(text(
                StatusCode::SERVICE_UNAVAILABLE,
                "no free id, try again\n",
            ));
        };
        tracing::info!(%id, "link created");
        let short = format!("{}/{}", self.config.base_url, id);
        let mut response = text(StatusCode::CREATED, &format!("{short}\n"));
        if let Ok(location) = HeaderValue::from_str(&short) {
            response.headers_mut().insert(header::LOCATION, location);
        }
        Ok(response)
    }

    fn resolve(&self, id: &str) -> Result<Response, KvError> {
        let Some(link) = self.kv.get(&key(id))?.as_deref().and_then(Link::decode) else {
            return Ok(text(StatusCode::NOT_FOUND, "not found\n"));
        };
        if unix_secs(&self.clock) >= link.created.saturating_add(self.config.ttl.as_secs()) {
            return Ok(text(StatusCode::GONE, "expired\n"));
        }
        let mut response = text(StatusCode::FOUND, "");
        // Validated as a header value when stored.
        if let Ok(location) = HeaderValue::from_str(&link.url) {
            response.headers_mut().insert(header::LOCATION, location);
        }
        Ok(response)
    }
}

/// Stores `url` under a fresh id. `None` if no free id was found.
fn store(
    kv: &impl Kv,
    clock: &impl Clock,
    random: &impl Random,
    url: &str,
) -> Result<Option<String>, KvError> {
    for _ in 0..ID_ATTEMPTS {
        let id = new_id(random);
        if kv.get(&key(&id))?.is_none() {
            let link = Link {
                created: unix_secs(clock),
                url: url.to_owned(),
            };
            kv.put(&key(&id), &link.encode())?;
            return Ok(Some(id));
        }
    }
    Ok(None)
}

const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

fn new_id(random: &impl Random) -> String {
    let mut n = random.u64();
    (0..ID_LEN)
        .map(|_| {
            let c = ALPHABET[(n % 62) as usize] as char;
            n /= 62;
            c
        })
        .collect()
}

fn is_id(s: &str) -> bool {
    s.len() == ID_LEN && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn key(id: &str) -> Vec<u8> {
    format!("link/{id}").into_bytes()
}

fn valid_url(body: &[u8]) -> Option<&str> {
    let url = std::str::from_utf8(body).ok()?.trim();
    let ok = url.len() <= MAX_URL
        && (url.starts_with("https://") || url.starts_with("http://"))
        && HeaderValue::from_str(url).is_ok();
    ok.then_some(url)
}

fn unix_secs(clock: &impl Clock) -> u64 {
    clock
        .now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn text(status: StatusCode, body: &str) -> Response {
    let mut response = Response::new(body.as_bytes().to_vec());
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

/// Stored as `<created unix secs>\n<url>`.
struct Link {
    created: u64,
    url: String,
}

impl Link {
    fn encode(&self) -> Vec<u8> {
        format!("{}\n{}", self.created, self.url).into_bytes()
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let (created, url) = std::str::from_utf8(bytes).ok()?.split_once('\n')?;
        Some(Self {
            created: created.parse().ok()?,
            url: url.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ambit_server_sim::SeededRandom;

    #[test]
    fn ids_are_valid() {
        let random = SeededRandom::new(1);
        for _ in 0..1000 {
            assert!(is_id(&new_id(&random)));
        }
    }

    #[test]
    fn link_roundtrip() {
        let link = Link {
            created: 42,
            url: "https://example.com/a\nb".into(),
        };
        let back = Link::decode(&link.encode()).unwrap();
        assert_eq!(
            (back.created, back.url.as_str()),
            (42, "https://example.com/a\nb")
        );
    }

    #[test]
    fn rejects_bad_urls() {
        assert!(valid_url(b"ftp://x").is_none());
        assert!(valid_url(b"https://x\x01").is_none());
        assert!(valid_url(&[b'h'; 3000]).is_none());
        assert_eq!(valid_url(b" https://x.org \n"), Some("https://x.org"));
    }
}
