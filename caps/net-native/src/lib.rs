//! Network capabilities on Linux/Mac.
//!
//! HTTP(S) via ureq with rustls and the bundled Mozilla roots. No proxy from the environment,
//! no redirects, no cookies: the request the app builds is the request that's sent.
#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use ambit_caps_net::{
    Device, Discovery, DiscoveryError, Http, HttpError, HttpErrorKind, Request, Response, Scope,
    Targets, is_lan,
};

pub struct HttpOptions {
    /// For the whole request: connect, send and receive.
    pub timeout: Duration,
    /// Larger response bodies fail with `TooLarge`.
    pub max_body: u64,
}

impl Default for HttpOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            max_body: 16 * 1024 * 1024,
        }
    }
}

pub struct NativeHttp {
    scope: Scope,
    agent: ureq::Agent,
    max_body: u64,
}

impl NativeHttp {
    pub fn new(scope: Scope, options: HttpOptions) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(options.timeout))
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(None)
            .build()
            .into();
        Self {
            scope,
            agent,
            max_body: options.max_body,
        }
    }
}

impl Http for NativeHttp {
    fn send(&self, request: Request) -> Result<Response, HttpError> {
        self.scope.check(&request)?;
        let response = self.agent.run(request).map_err(error)?;
        let (parts, mut body) = response.into_parts();
        let mut bytes = Vec::new();
        // One byte over the limit tells "exactly at the limit" from "too large".
        body.as_reader()
            .take(self.max_body + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| HttpError::new(HttpErrorKind::Network, e.to_string()))?;
        if bytes.len() as u64 > self.max_body {
            return Err(HttpError::new(
                HttpErrorKind::TooLarge,
                format!("response body over {} bytes", self.max_body),
            ));
        }
        Ok(Response::from_parts(parts, bytes))
    }
}

fn error(e: ureq::Error) -> HttpError {
    let kind = match e {
        ureq::Error::Timeout(_) => HttpErrorKind::Timeout,
        ureq::Error::BadUri(_) => HttpErrorKind::Invalid,
        _ => HttpErrorKind::Network,
    };
    HttpError::new(kind, e.to_string())
}

const SSDP: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(239, 255, 255, 250)), 1900);

/// SSDP search over IPv4 multicast. Multicast TTL stays at the default of 1: the search never
/// leaves the local network.
pub struct NativeDiscovery {
    targets: Targets,
}

impl NativeDiscovery {
    pub fn new(targets: Targets) -> Self {
        Self { targets }
    }
}

impl Discovery for NativeDiscovery {
    fn ssdp(&self, target: &str, wait: Duration) -> Result<Vec<Device>, DiscoveryError> {
        self.targets.check(target)?;
        let failed = |e: std::io::Error| DiscoveryError {
            denied: false,
            message: e.to_string(),
        };
        let wait = wait.clamp(Duration::from_millis(100), Duration::from_secs(5));
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(failed)?;
        let search = format!(
            "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\n\
             MX: {}\r\nST: {target}\r\n\r\n",
            wait.as_secs().max(1)
        );
        // UDP is lossy: send twice.
        for _ in 0..2 {
            socket.send_to(search.as_bytes(), SSDP).map_err(failed)?;
        }
        let deadline = Instant::now() + wait;
        let mut seen = BTreeSet::new();
        let mut devices = Vec::new();
        let mut buf = [0u8; 2048];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            socket.set_read_timeout(Some(left)).map_err(failed)?;
            let (n, from) = match socket.recv_from(&mut buf) {
                Ok(r) => r,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(e) => return Err(failed(e)),
            };
            if !is_lan(from.ip()) || !seen.insert(from.ip()) {
                continue;
            }
            if let Some(device) = parse(&buf[..n], from.ip(), target) {
                devices.push(device);
            } else {
                seen.remove(&from.ip());
            }
        }
        Ok(devices)
    }
}

/// An SSDP answer: `HTTP/1.1 200 OK` with headers, for the target we asked about.
fn parse(datagram: &[u8], addr: IpAddr, target: &str) -> Option<Device> {
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut response = httparse::Response::new(&mut headers);
    response.parse(datagram).ok()?;
    if response.code != Some(200) {
        return None;
    }
    let headers: Vec<(String, String)> = response
        .headers
        .iter()
        .map(|h| {
            let value = String::from_utf8_lossy(h.value).trim().to_string();
            (h.name.to_ascii_lowercase(), value)
        })
        .collect();
    let get = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    };
    if get("st").as_deref() != Some(target) {
        return None;
    }
    Some(Device {
        addr,
        location: get("location"),
        headers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_ssdp_answer_for_our_target() {
        let answer = b"HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age = 1800\r\n\
            LOCATION: http://192.168.1.20:1400/xml/device_description.xml\r\n\
            ST: urn:schemas-upnp-org:device:ZonePlayer:1\r\nUSN: uuid:RINCON_1::x\r\n\r\n";
        let ip: IpAddr = "192.168.1.20".parse().unwrap();
        let device = parse(answer, ip, "urn:schemas-upnp-org:device:ZonePlayer:1").unwrap();
        assert_eq!(
            device.location.as_deref(),
            Some("http://192.168.1.20:1400/xml/device_description.xml")
        );
        assert!(
            device
                .headers
                .iter()
                .any(|(n, v)| n == "usn" && v == "uuid:RINCON_1::x")
        );
        assert!(parse(answer, ip, "ssdp:all").is_none());
        assert!(parse(b"NOTIFY * HTTP/1.1\r\n\r\n", ip, "ssdp:all").is_none());
    }
}
