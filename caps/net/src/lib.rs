//! Network capabilities: outbound HTTP within a scope, and device discovery on the local network.
//!
//! The scope is set by the wiring (human-owned) and enforced by every implementation, sim
//! included, so a test fails the same way production would. Separate from `ambit-caps` so apps
//! without network access don't carry the `http` crate.
#![forbid(unsafe_code)]

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

pub use http;

pub type Request = http::Request<Vec<u8>>;
pub type Response = http::Response<Vec<u8>>;

/// Outbound HTTP. Blocking: call it from a task, not from `update`.
///
/// Redirects are not followed: a 3xx comes back to the app, and following it is a new request,
/// checked against the scope again.
pub trait Http: Send + Sync + 'static {
    fn send(&self, request: Request) -> Result<Response, HttpError>;
}

impl<T: Http + ?Sized> Http for std::sync::Arc<T> {
    fn send(&self, request: Request) -> Result<Response, HttpError> {
        (**self).send(request)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpErrorKind {
    /// Outside the scope the wiring granted. Nothing was sent.
    Denied,
    /// Not a request this capability can send (e.g. a relative URI).
    Invalid,
    Timeout,
    /// DNS, connection, TLS or protocol failure.
    Network,
    /// The response body is larger than the configured limit.
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpError {
    pub kind: HttpErrorKind,
    pub message: String,
}

impl HttpError {
    pub fn new(kind: HttpErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "http {:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for HttpError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Http,
    Https,
}

impl Scheme {
    fn default_port(self) -> u16 {
        match self {
            Scheme::Http => 80,
            Scheme::Https => 443,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Host {
    /// This host name or IP literal, case-insensitive.
    Exact(String),
    /// Any subdomain of this domain (not the domain itself).
    Subdomains(String),
    /// Any IP literal on the local network (see [`is_lan`]). Never a name: a name could resolve
    /// anywhere.
    Lan,
}

/// One scheme + host + port the app may reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    scheme: Scheme,
    host: Host,
    port: u16,
}

impl Origin {
    /// `https://api.example.com`, `http://192.168.1.10:8080`, or `https://*.example.com` for
    /// any subdomain. No path, query or user info.
    pub fn parse(origin: &str) -> Result<Self, String> {
        let uri: http::Uri = origin.parse().map_err(|e| format!("{origin}: {e}"))?;
        let scheme = match uri.scheme_str() {
            Some("http") => Scheme::Http,
            Some("https") => Scheme::Https,
            _ => return Err(format!("{origin}: scheme must be http or https")),
        };
        if uri.path_and_query().is_some_and(|p| p.as_str() != "/") {
            return Err(format!("{origin}: an origin has no path or query"));
        }
        let authority = uri
            .authority()
            .ok_or_else(|| format!("{origin}: no host"))?;
        if authority.as_str().contains('@') {
            return Err(format!("{origin}: no user info"));
        }
        let host = authority.host().to_ascii_lowercase();
        let host = match host.strip_prefix("*.") {
            Some(domain) if !domain.is_empty() && !domain.contains('*') => {
                Host::Subdomains(domain.to_string())
            }
            _ if host.contains('*') => {
                return Err(format!("{origin}: only a leading *. is allowed"));
            }
            _ => Host::Exact(host),
        };
        let port = authority.port_u16().unwrap_or(scheme.default_port());
        Ok(Self { scheme, host, port })
    }

    /// Any device on the local network, by IP address, on this port.
    pub fn lan(scheme: Scheme, port: u16) -> Self {
        Self {
            scheme,
            host: Host::Lan,
            port,
        }
    }

    fn allows(&self, scheme: Scheme, host: &str, port: u16) -> bool {
        if scheme != self.scheme || port != self.port {
            return false;
        }
        match &self.host {
            Host::Exact(h) => h == host,
            Host::Subdomains(d) => host
                .strip_suffix(d.as_str())
                .is_some_and(|rest| rest.len() > 1 && rest.ends_with('.')),
            Host::Lan => ip_literal(host).is_some_and(is_lan),
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = match self.scheme {
            Scheme::Http => "http",
            Scheme::Https => "https",
        };
        match &self.host {
            Host::Exact(h) => write!(f, "{scheme}://{h}:{}", self.port),
            Host::Subdomains(d) => write!(f, "{scheme}://*.{d}:{}", self.port),
            Host::Lan => write!(f, "{scheme}://<local network>:{}", self.port),
        }
    }
}

/// The origins an `Http` capability may reach. Empty allows nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    origins: Vec<Origin>,
}

impl Scope {
    pub fn new(origins: impl IntoIterator<Item = Origin>) -> Self {
        Self {
            origins: origins.into_iter().collect(),
        }
    }

    /// Parses each origin with [`Origin::parse`].
    pub fn parse<'a>(origins: impl IntoIterator<Item = &'a str>) -> Result<Self, String> {
        origins
            .into_iter()
            .map(Origin::parse)
            .collect::<Result<_, _>>()
            .map(|origins| Self { origins })
    }

    pub fn origins(&self) -> &[Origin] {
        &self.origins
    }

    /// Checks a request before anything is sent. Implementations call this first.
    pub fn check(&self, request: &Request) -> Result<(), HttpError> {
        let uri = request.uri();
        let scheme = match uri.scheme_str() {
            Some("http") => Scheme::Http,
            Some("https") => Scheme::Https,
            _ => {
                return Err(HttpError::new(
                    HttpErrorKind::Invalid,
                    format!("{uri}: needs an absolute http or https URI"),
                ));
            }
        };
        let authority = uri
            .authority()
            .ok_or_else(|| HttpError::new(HttpErrorKind::Invalid, format!("{uri}: no host")))?;
        if authority.as_str().contains('@') {
            return Err(HttpError::new(
                HttpErrorKind::Invalid,
                format!("{uri}: user info in the URI is not supported"),
            ));
        }
        let host = authority.host().to_ascii_lowercase();
        let port = authority.port_u16().unwrap_or(scheme.default_port());
        if self.origins.iter().any(|o| o.allows(scheme, &host, port)) {
            Ok(())
        } else {
            Err(HttpError::new(
                HttpErrorKind::Denied,
                format!("{uri}: outside the granted scope"),
            ))
        }
    }
}

fn ip_literal(host: &str) -> Option<IpAddr> {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .ok()
}

/// Private and link-local addresses: 10/8, 172.16/12, 192.168/16, 169.254/16, fc00::/7,
/// fe80::/10. Not loopback: that's this machine, not the local network.
pub fn is_lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// Finds devices on the local network. Blocking for about `wait`: call it from a task.
pub trait Discovery: Send + Sync + 'static {
    /// SSDP (UPnP) search for `target`, e.g. `urn:schemas-upnp-org:device:ZonePlayer:1`. Only
    /// targets the wiring granted; only answers from local-network addresses.
    fn ssdp(&self, target: &str, wait: Duration) -> Result<Vec<Device>, DiscoveryError>;
}

impl<T: Discovery + ?Sized> Discovery for std::sync::Arc<T> {
    fn ssdp(&self, target: &str, wait: Duration) -> Result<Vec<Device>, DiscoveryError> {
        (**self).ssdp(target, wait)
    }
}

/// A device that answered, once per address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub addr: IpAddr,
    /// The `LOCATION` header: where its description lives.
    pub location: Option<String>,
    /// All response headers, names lowercased, in order.
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryError {
    /// Nothing was sent: the target is outside the granted scope.
    pub denied: bool,
    pub message: String,
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "discovery: {}", self.message)
    }
}

impl std::error::Error for DiscoveryError {}

/// The SSDP search targets a `Discovery` capability may look for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Targets {
    targets: Vec<String>,
}

impl Targets {
    pub fn new<'a>(targets: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            targets: targets.into_iter().map(str::to_string).collect(),
        }
    }

    pub fn check(&self, target: &str) -> Result<(), DiscoveryError> {
        if self.targets.iter().any(|t| t == target) {
            Ok(())
        } else {
            Err(DiscoveryError {
                denied: true,
                message: format!("{target}: outside the granted scope"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(uri: &str) -> Request {
        http::Request::get(uri).body(Vec::new()).unwrap()
    }

    fn allowed(scope: &Scope, uri: &str) -> bool {
        match scope.check(&req(uri)) {
            Ok(()) => true,
            Err(e) if e.kind == HttpErrorKind::Denied => false,
            Err(e) => panic!("{uri}: {e}"),
        }
    }

    #[test]
    fn exact_origins_match_scheme_host_and_port() {
        let scope = Scope::parse(["https://api.spotify.com", "http://10.0.0.5:8080"]).unwrap();
        assert!(allowed(&scope, "https://api.spotify.com/v1/search?q=x"));
        assert!(allowed(&scope, "https://API.Spotify.com:443/"));
        assert!(allowed(&scope, "http://10.0.0.5:8080/x"));
        assert!(!allowed(&scope, "http://api.spotify.com/"));
        assert!(!allowed(&scope, "https://api.spotify.com:8443/"));
        assert!(!allowed(&scope, "https://api.spotify.com.evil.com/"));
        assert!(!allowed(&scope, "https://evil.com/api.spotify.com"));
        assert!(!allowed(&scope, "http://10.0.0.5/"));
    }

    #[test]
    fn subdomains_exclude_the_domain_and_lookalikes() {
        let scope = Scope::parse(["https://*.scdn.co"]).unwrap();
        assert!(allowed(&scope, "https://i.scdn.co/image"));
        assert!(allowed(&scope, "https://a.b.scdn.co/"));
        assert!(!allowed(&scope, "https://scdn.co/"));
        assert!(!allowed(&scope, "https://evilscdn.co/"));
    }

    #[test]
    fn lan_allows_private_ip_literals_only() {
        let scope = Scope::new([Origin::lan(Scheme::Http, 1400)]);
        assert!(allowed(&scope, "http://192.168.178.21:1400/status"));
        assert!(allowed(&scope, "http://10.1.2.3:1400/"));
        assert!(allowed(&scope, "http://172.16.0.1:1400/"));
        assert!(allowed(&scope, "http://[fe80::1]:1400/"));
        assert!(!allowed(&scope, "http://192.168.178.21:80/"));
        assert!(!allowed(&scope, "http://8.8.8.8:1400/"));
        assert!(!allowed(&scope, "http://127.0.0.1:1400/"));
        assert!(!allowed(&scope, "http://172.32.0.1:1400/"));
        assert!(!allowed(&scope, "http://speaker.local:1400/"));
        assert!(!allowed(&scope, "https://192.168.178.21:1400/"));
    }

    #[test]
    fn empty_scope_allows_nothing() {
        assert!(!allowed(&Scope::default(), "https://example.com/"));
    }

    #[test]
    fn unusable_requests_are_invalid_not_denied() {
        let scope = Scope::parse(["https://example.com"]).unwrap();
        for uri in [
            "/relative",
            "ftp://example.com/",
            "https://user@example.com/",
        ] {
            let e = scope.check(&req(uri)).unwrap_err();
            assert_eq!(e.kind, HttpErrorKind::Invalid, "{uri}");
        }
    }

    #[test]
    fn bad_origins_are_rejected() {
        for o in [
            "example.com",
            "ftp://example.com",
            "https://example.com/path",
            "https://user@example.com",
            "https://ex*ample.com",
            "https://*.",
        ] {
            assert!(Origin::parse(o).is_err(), "{o}");
        }
    }

    #[test]
    fn discovery_targets_are_exact() {
        let targets = Targets::new(["urn:schemas-upnp-org:device:ZonePlayer:1"]);
        assert!(
            targets
                .check("urn:schemas-upnp-org:device:ZonePlayer:1")
                .is_ok()
        );
        assert!(targets.check("ssdp:all").unwrap_err().denied);
    }
}
