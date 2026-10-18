//! Network capabilities on Linux/Mac.
//!
//! HTTP(S) via ureq with rustls and the bundled Mozilla roots. No proxy from the environment,
//! no redirects, no cookies: the request the app builds is the request that's sent.
//! WebSocket (`ws://`) via tungstenite over a plain TCP connection.
#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, UdpSocket};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ambit_caps_net::{
    AuthError, AuthErrorKind, Authorize, Device, Discovery, DiscoveryError, Http, HttpError,
    HttpErrorKind, Origin, Params, Request, Response, Scope, Targets, is_lan, parse_query,
};
use ambit_lockdown::{Grant, Grants};

/// Each origin's port, and name lookups. Which hosts on those ports is the scope's job: the lock
/// allows TCP by port only.
fn scope_grants(scope: &Scope) -> Vec<Grant> {
    let mut grants = vec![Grant::Resolve];
    grants.extend(scope.origins().iter().map(|o| Grant::Connect(o.port())));
    grants
}

impl Grants for NativeHttp {
    fn grants(&self) -> Vec<Grant> {
        scope_grants(&self.scope)
    }
}

/// SSDP is UDP multicast on the local network.
impl Grants for NativeDiscovery {
    fn grants(&self) -> Vec<Grant> {
        vec![Grant::Udp]
    }
}

/// Listens on its loopback port while waiting. The browser is opened by its helper, outside the
/// lock, so no program is started from inside.
impl Grants for NativeAuthorize {
    fn grants(&self) -> Vec<Grant> {
        vec![Grant::Listen(self.port)]
    }
}

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

/// The loopback flow: opens the system browser (`xdg-open` on Linux, `open` on macOS) and
/// listens on `127.0.0.1:<port>` for the one redirect back, for as long as `authorize` waits.
///
/// The browser is opened by a helper started with the capability, so make it before the lock:
/// the helper stays outside the lock, and the program needs no grant to start programs. The
/// helper only opens addresses at the scope's exact origins, and checks each one again itself.
pub struct NativeAuthorize {
    scope: Scope,
    port: u16,
    browser: Option<Mutex<Opener>>,
}

const CALLBACK_PATH: &str = "/callback";
const DONE_PAGE: &str = "<!doctype html><title>Done</title>\
    <p>Done. You can close this tab and go back to the terminal.";

impl NativeAuthorize {
    /// `scope`: what may be opened in the browser (the provider's authorize page). `port`: the
    /// loopback port registered with the provider.
    pub fn new(scope: Scope, port: u16) -> Self {
        let program = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let browser = Opener::start(program, &scope).map(Mutex::new);
        Self {
            scope,
            port,
            browser,
        }
    }

    /// Doesn't start a browser: the user opens the URL the app shows. For remote sessions,
    /// and for tests.
    pub fn without_browser(mut self) -> Self {
        self.browser = None;
        self
    }

    /// Opens addresses with `program` (given the address as its one argument) instead of the
    /// system browser.
    pub fn opening_with(mut self, program: impl AsRef<std::ffi::OsStr>) -> Self {
        self.browser = Opener::start(program, &self.scope).map(Mutex::new);
        self
    }
}

impl Authorize for NativeAuthorize {
    fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}{CALLBACK_PATH}", self.port)
    }

    fn authorize(&self, url: &str, timeout: Duration) -> Result<Params, AuthError> {
        self.scope.check_url(url)?;
        let io = |e: std::io::Error| AuthError::new(AuthErrorKind::Io, e.to_string());
        // Listen before opening the browser, so the redirect can't arrive first.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, self.port)).map_err(io)?;
        listener.set_nonblocking(true).map_err(io)?;
        if let Some(browser) = &self.browser {
            browser.lock().unwrap_or_else(|e| e.into_inner()).open(url);
        }
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut conn, _)) => {
                    if let Some(params) = callback(&mut conn) {
                        return Ok(params);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(io(e)),
            }
        }
        Err(AuthError::new(
            AuthErrorKind::Timeout,
            "no redirect came back",
        ))
    }
}

/// Handles one connection: the callback gets the done page and its query; anything else
/// (a favicon request) gets a 404 and `None`.
fn callback(conn: &mut std::net::TcpStream) -> Option<Params> {
    conn.set_nonblocking(false).ok()?;
    conn.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 8192 {
        let n = conn.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        head.extend_from_slice(&buf[..n]);
    }
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut request = httparse::Request::new(&mut headers);
    request.parse(&head).ok()?;
    let target = request.path.unwrap_or("");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if request.method != Some("GET") || path != CALLBACK_PATH {
        let _ = conn
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return None;
    }
    let _ = write!(
        conn,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{DONE_PAGE}",
        DONE_PAGE.len()
    );
    Some(parse_query(query))
}

/// The helper that opens addresses in the browser: a shell reading one address per line, which
/// runs the opener for those that start with one of its prefixes (followed by nothing, `/`, `?`
/// or `#`) and ignores the rest. Started before the lock, it stays outside it; the program can
/// still ask it for anything, so the check is the helper's own. The prefixes and the address are
/// arguments and input, never part of the script.
struct Opener {
    child: Child,
    input: Option<ChildStdin>,
}

const OPENER_SCRIPT: &str = r#"opener=$1; shift
while IFS= read -r url; do
  for prefix in "$@"; do
    case $url in
      "$prefix" | "$prefix"/* | "$prefix"\?* | "$prefix"\#*)
        "$opener" "$url" </dev/null >/dev/null 2>&1 &
        break ;;
    esac
  done
done"#;

impl Opener {
    /// `None` if there's nothing to open (no exact origin) or no shell: the app shows the URL.
    fn start(program: impl AsRef<std::ffi::OsStr>, scope: &Scope) -> Option<Self> {
        let prefixes = opener_prefixes(scope);
        if prefixes.is_empty() {
            return None;
        }
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(OPENER_SCRIPT)
            .arg("sh")
            .arg(program)
            .args(prefixes)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let input = child.stdin.take();
        Some(Self { child, input })
    }

    /// Best effort; the app shows the URL as well.
    fn open(&mut self, url: &str) {
        if url.contains(['\n', '\r']) {
            return;
        }
        if let Some(input) = &mut self.input {
            let _ = writeln!(input, "{url}").and_then(|()| input.flush());
        }
    }
}

impl Drop for Opener {
    /// Closing its input ends the helper.
    fn drop(&mut self) {
        drop(self.input.take());
        let _ = self.child.wait();
    }
}

/// How the scope's exact origins start a URL: `https://host` for the default port, and
/// `https://host:port` always. Subdomain and local-network origins name no single address, so
/// the helper opens nothing there.
fn opener_prefixes(scope: &Scope) -> Vec<String> {
    let mut prefixes = Vec::new();
    for origin in scope.origins() {
        let shown = origin.to_string();
        let Some((scheme, rest)) = shown.split_once("://") else {
            continue;
        };
        if rest.starts_with('*') || rest.starts_with('<') {
            continue;
        }
        if Origin::parse(&format!("{scheme}://{rest}")).ok().as_ref() != Some(origin) {
            continue;
        }
        if let Some((host, port)) = rest.rsplit_once(':') {
            let default = if scheme == "https" { "443" } else { "80" };
            if port == default {
                prefixes.push(format!("{scheme}://{host}"));
            }
        }
        prefixes.push(shown);
    }
    prefixes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake browser: a script that writes each address it's given to `opened`.
    #[cfg(unix)]
    fn fake_browser(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ambit-opener-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let opened = dir.join("opened");
        let script = dir.join("browser");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho \"$1\" >> '{}'\n", opened.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (script, opened)
    }

    #[cfg(unix)]
    #[test]
    fn the_helper_opens_only_addresses_at_its_exact_origins() {
        let (browser, opened) = fake_browser("exact");
        let scope = Scope::parse([
            "https://auth.example.com",
            "http://127.0.0.1:8080",
            "https://*.cdn.example.com",
        ])
        .unwrap();
        let mut opener = Opener::start(&browser, &scope).unwrap();
        let tried = [
            ("https://auth.example.com/authorize?x=1", true),
            ("https://auth.example.com", true),
            ("https://auth.example.com:443?y", true),
            ("http://127.0.0.1:8080/#z", true),
            ("https://auth.example.com.evil.net/", false),
            ("https://auth.example.com@evil.net/", false),
            ("https://evil.net/?https://auth.example.com/", false),
            ("http://auth.example.com/", false),
            ("http://127.0.0.1/", false),
            ("https://a.cdn.example.com/", false),
            ("-x https://auth.example.com/", false),
        ];
        for (url, _) in tried {
            opener.open(url);
        }
        drop(opener);
        let expected: Vec<&str> = tried.iter().filter(|t| t.1).map(|t| t.0).collect();
        let deadline = Instant::now() + Duration::from_secs(10);
        let lines = loop {
            let text = std::fs::read_to_string(&opened).unwrap_or_default();
            let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
            if lines.len() >= expected.len() || Instant::now() > deadline {
                lines.sort();
                break lines;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut expected: Vec<String> = expected.into_iter().map(str::to_string).collect();
        expected.sort();
        assert_eq!(lines, expected);
    }

    #[test]
    fn no_helper_without_an_exact_origin() {
        let scope = Scope::parse(["https://*.example.com"]).unwrap();
        assert!(Opener::start("true", &scope).is_none());
        assert!(Opener::start("true", &Scope::default()).is_none());
    }

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
