//! Deterministic, in-memory capabilities for tests. Handles are `Clone`, so a test keeps control
//! after handing one to `init`.
#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::IpAddr;

use ambit_caps::{Clock, Entry, Kv, KvError, Random};
use ambit_caps_dir::{
    Dir, DirError, DirErrorKind, DirModify, DirWrite, Entry as DirEntry, File as DirFile, Meta,
    RelPath,
};
use ambit_caps_net::{
    AuthError, AuthErrorKind, Authorize, Device, Discovery, DiscoveryError, Http, HttpError,
    HttpErrorKind, Params, Request, Response, Scope, Targets, WebSocket, WsConnection, WsMessage,
    http,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A clock that only moves when the test says so.
#[derive(Clone)]
pub struct SimClock {
    time: Arc<Mutex<(SystemTime, Duration)>>,
}

impl SimClock {
    pub fn at(now: SystemTime) -> Self {
        Self {
            time: Arc::new(Mutex::new((now, Duration::ZERO))),
        }
    }

    /// Moves wall and monotonic time forward together.
    pub fn advance(&self, by: Duration) {
        let mut time = lock(&self.time);
        time.0 += by;
        time.1 += by;
    }

    /// Sets the wall clock only (an NTP jump); monotonic time is unaffected.
    pub fn set_wall(&self, now: SystemTime) {
        lock(&self.time).0 = now;
    }
}

impl Clock for SimClock {
    fn now(&self) -> SystemTime {
        lock(&self.time).0
    }
    fn elapsed(&self) -> Duration {
        lock(&self.time).1
    }
}

/// Reproducible randomness (splitmix64). Not cryptographic.
#[derive(Clone)]
pub struct SeededRandom {
    state: Arc<Mutex<u64>>,
}

impl SeededRandom {
    pub fn new(seed: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(seed)),
        }
    }
}

impl Random for SeededRandom {
    fn fill(&self, buf: &mut [u8]) {
        let mut state = lock(&self.state);
        for chunk in buf.chunks_mut(8) {
            *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = *state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            chunk.copy_from_slice(&z.to_le_bytes()[..chunk.len()]);
        }
    }
}

/// Which `MemKv` operations fail, for testing how the app handles storage errors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Faults {
    #[default]
    None,
    /// Every operation fails: storage is down.
    All,
    /// Reads work, `put` and `delete` fail: e.g. a full disk.
    Writes,
}

/// An in-memory store, with injectable failures. Without failures it is `ambit_caps_std::MemKv`.
#[derive(Clone, Default)]
pub struct MemKv {
    inner: ambit_caps_std::MemKv,
    faults: Arc<Mutex<Faults>>,
}

impl MemKv {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// From now on, fail the operations `faults` selects.
    pub fn fail(&self, faults: Faults) {
        *lock(&self.faults) = faults;
    }

    fn check(&self, write: bool) -> Result<(), KvError> {
        match *lock(&self.faults) {
            Faults::All => Err(KvError::new("injected failure")),
            Faults::Writes if write => Err(KvError::new("injected write failure")),
            _ => Ok(()),
        }
    }
}

impl Kv for MemKv {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, KvError> {
        self.check(false)?;
        self.inner.get(key)
    }
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), KvError> {
        self.check(true)?;
        self.inner.put(key, value)
    }
    fn delete(&self, key: &[u8]) -> Result<(), KvError> {
        self.check(true)?;
        self.inner.delete(key)
    }
    fn scan(&self, prefix: &[u8]) -> Result<Vec<Entry>, KvError> {
        self.check(false)?;
        self.inner.scan(prefix)
    }
}

/// A request as `SimHttp` saw it, for assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub method: http::Method,
    pub uri: String,
    pub headers: http::HeaderMap,
    pub body: Vec<u8>,
}

type Handler = Arc<dyn Fn(&Request) -> Option<Result<Response, HttpError>> + Send + Sync>;

/// Fake servers behind the same scope check as production. Requests the scope denies fail with
/// `Denied` and never reach a handler; requests nothing answers fail with `Network`.
#[derive(Clone)]
pub struct SimHttp {
    scope: Arc<Scope>,
    handlers: Arc<Mutex<Vec<Handler>>>,
    sent: Arc<Mutex<Vec<Sent>>>,
    down: Arc<Mutex<Option<HttpErrorKind>>>,
}

impl SimHttp {
    pub fn new(scope: Scope) -> Self {
        Self {
            scope: Arc::new(scope),
            handlers: Arc::default(),
            sent: Arc::default(),
            down: Arc::default(),
        }
    }

    /// Answers `method uri` (exact, query included) with `status` and `body`.
    pub fn respond(&self, method: &str, uri: &str, status: u16, body: impl Into<Vec<u8>>) {
        let (method, uri, body) = (method.to_string(), uri.to_string(), body.into());
        self.handle(move |req| {
            (req.method().as_str() == method && req.uri().to_string() == uri).then(|| {
                Ok(http::Response::builder()
                    .status(status)
                    .body(body.clone())
                    .expect("valid sim response"))
            })
        });
    }

    /// A handler for anything more dynamic. `None` passes the request to the next handler.
    /// Later handlers take precedence over earlier ones.
    pub fn handle(
        &self,
        handler: impl Fn(&Request) -> Option<Result<Response, HttpError>> + Send + Sync + 'static,
    ) {
        lock(&self.handlers).push(Arc::new(handler));
    }

    /// From now on, every request in scope fails with `kind` (e.g. `Timeout`); `None` restores.
    pub fn fail(&self, kind: Option<HttpErrorKind>) {
        *lock(&self.down) = kind;
    }

    /// Requests that passed the scope check, in order.
    pub fn sent(&self) -> Vec<Sent> {
        lock(&self.sent).clone()
    }
}

impl Http for SimHttp {
    fn send(&self, request: Request) -> Result<Response, HttpError> {
        self.scope.check(&request)?;
        lock(&self.sent).push(Sent {
            method: request.method().clone(),
            uri: request.uri().to_string(),
            headers: request.headers().clone(),
            body: request.body().clone(),
        });
        if let Some(kind) = *lock(&self.down) {
            return Err(HttpError::new(kind, "injected failure"));
        }
        let handlers = lock(&self.handlers).clone();
        handlers
            .iter()
            .rev()
            .find_map(|h| h(&request))
            .unwrap_or_else(|| {
                Err(HttpError::new(
                    HttpErrorKind::Network,
                    format!(
                        "sim: nothing answers {} {}",
                        request.method(),
                        request.uri()
                    ),
                ))
            })
    }
}

/// One simulated WebSocket server connection: gets each message the app sends, returns the
/// messages it answers with.
pub type WsServer = Box<dyn FnMut(WsMessage) -> Vec<WsMessage> + Send>;

type WsAccept = Arc<dyn Fn(&str) -> Option<WsServer> + Send + Sync>;

/// WebSocket servers behind the same scope check as production. URLs the scope denies fail
/// with `Denied`; URLs nothing serves fail with `Network`. `recv` with nothing to receive is
/// `Timeout`: in a test, nothing more will come.
#[derive(Clone)]
pub struct SimWebSocket {
    scope: Arc<Scope>,
    servers: Arc<Mutex<Vec<WsAccept>>>,
    sent: Arc<Mutex<Vec<(String, WsMessage)>>>,
}

impl SimWebSocket {
    pub fn new(scope: Scope) -> Self {
        Self {
            scope: Arc::new(scope),
            servers: Arc::default(),
            sent: Arc::default(),
        }
    }

    /// Serves connections: `accept` gets the URL and returns a server for this connection, or
    /// `None` to leave it to the next. Later ones take precedence over earlier ones.
    pub fn serve(&self, accept: impl Fn(&str) -> Option<WsServer> + Send + Sync + 'static) {
        lock(&self.servers).push(Arc::new(accept));
    }

    /// Messages the app sent, with the URL of their connection, in order.
    pub fn sent(&self) -> Vec<(String, WsMessage)> {
        lock(&self.sent).clone()
    }
}

impl WebSocket for SimWebSocket {
    fn connect(&self, url: &str) -> Result<Box<dyn WsConnection>, HttpError> {
        let uri: http::Uri = url
            .parse()
            .map_err(|e| HttpError::new(HttpErrorKind::Invalid, format!("{url}: {e}")))?;
        self.scope.check_ws(&uri)?;
        let servers = lock(&self.servers).clone();
        let server = servers
            .iter()
            .rev()
            .find_map(|accept| accept(url))
            .ok_or_else(|| {
                HttpError::new(HttpErrorKind::Network, format!("sim: nothing serves {url}"))
            })?;
        Ok(Box::new(SimWsConnection {
            url: url.to_string(),
            server,
            inbox: VecDeque::new(),
            closed: false,
            sent: self.sent.clone(),
        }))
    }
}

struct SimWsConnection {
    url: String,
    server: WsServer,
    inbox: VecDeque<WsMessage>,
    closed: bool,
    sent: Arc<Mutex<Vec<(String, WsMessage)>>>,
}

impl WsConnection for SimWsConnection {
    fn send(&mut self, message: WsMessage) -> Result<(), HttpError> {
        if self.closed {
            return Err(HttpError::new(HttpErrorKind::Closed, "closed"));
        }
        lock(&self.sent).push((self.url.clone(), message.clone()));
        let replies = (self.server)(message);
        self.inbox.extend(replies);
        Ok(())
    }

    fn recv(&mut self) -> Result<WsMessage, HttpError> {
        if self.closed {
            return Err(HttpError::new(HttpErrorKind::Closed, "closed"));
        }
        self.inbox
            .pop_front()
            .ok_or_else(|| HttpError::new(HttpErrorKind::Timeout, "sim: nothing more to receive"))
    }

    fn close(&mut self) {
        self.closed = true;
    }
}

/// Devices that answer discovery, behind the same target check as production.
#[derive(Clone)]
pub struct SimDiscovery {
    targets: Arc<Targets>,
    devices: Arc<Mutex<Vec<(String, Device)>>>,
}

impl SimDiscovery {
    pub fn new(targets: Targets) -> Self {
        Self {
            targets: Arc::new(targets),
            devices: Arc::default(),
        }
    }

    /// A device answering searches for `target`, with `LOCATION: location`.
    pub fn add(&self, target: &str, addr: IpAddr, location: &str) {
        let device = Device {
            addr,
            location: Some(location.to_string()),
            headers: vec![
                ("st".to_string(), target.to_string()),
                ("location".to_string(), location.to_string()),
            ],
        };
        lock(&self.devices).push((target.to_string(), device));
    }

    pub fn remove(&self, addr: IpAddr) {
        lock(&self.devices).retain(|(_, d)| d.addr != addr);
    }
}

impl Discovery for SimDiscovery {
    fn ssdp(&self, target: &str, _wait: Duration) -> Result<Vec<Device>, DiscoveryError> {
        self.targets.check(target)?;
        Ok(lock(&self.devices)
            .iter()
            .filter(|(t, _)| t == target)
            .map(|(_, d)| d.clone())
            .collect())
    }
}

/// An in-memory directory tree. Directories exist when created or when a file is in them.
#[derive(Clone, Default)]
pub struct SimDir {
    tree: Arc<Mutex<SimTree>>,
}

#[derive(Default)]
struct SimTree {
    files: BTreeMap<Vec<String>, Vec<u8>>,
    dirs: BTreeSet<Vec<String>>,
    /// Free bytes; `None` is unlimited.
    space: Option<u64>,
    /// Unplugged: every call fails with `Unavailable`.
    unplugged: bool,
}

impl SimDir {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces a file at `path` (`"a/b.flac"`), creating its directories.
    pub fn file(&self, path: &str, contents: impl Into<Vec<u8>>) -> &Self {
        let parts = RelPath::parse(path)
            .expect("valid sim path")
            .parts()
            .to_vec();
        let mut tree = lock(&self.tree);
        for i in 1..parts.len() {
            tree.dirs.insert(parts[..i].to_vec());
        }
        tree.files.insert(parts, contents.into());
        self
    }

    /// Adds an (empty) directory at `path`, and its parents.
    pub fn dir(&self, path: &str) -> &Self {
        let parts = RelPath::parse(path)
            .expect("valid sim path")
            .parts()
            .to_vec();
        let mut tree = lock(&self.tree);
        for i in 1..=parts.len() {
            tree.dirs.insert(parts[..i].to_vec());
        }
        self
    }

    /// Free space from now on: writes that don't fit fail with `Full`, and writes use it up.
    pub fn space(&self, bytes: u64) -> &Self {
        lock(&self.tree).space = Some(bytes);
        self
    }

    /// Unplugs the tree (`false`) or plugs it back in (`true`), like a card. While unplugged,
    /// every call fails with `Unavailable`; the contents are kept for when it's back.
    pub fn plugged(&self, plugged: bool) -> &Self {
        lock(&self.tree).unplugged = !plugged;
        self
    }

    fn plugged_tree(&self) -> Result<MutexGuard<'_, SimTree>, DirError> {
        let tree = lock(&self.tree);
        if tree.unplugged {
            return Err(DirError::new(DirErrorKind::Unavailable, "sim: unplugged"));
        }
        Ok(tree)
    }

    /// The contents of the file at `path`, if there is one: what the app wrote.
    pub fn read(&self, path: &str) -> Option<Vec<u8>> {
        let parts = RelPath::parse(path).expect("valid sim path");
        lock(&self.tree).files.get(parts.parts()).cloned()
    }

    /// Removes the file or directory at `path`, and everything under it.
    pub fn remove(&self, path: &str) {
        let parts = RelPath::parse(path)
            .expect("valid sim path")
            .parts()
            .to_vec();
        let mut tree = lock(&self.tree);
        tree.files.retain(|p, _| !p.starts_with(&parts));
        tree.dirs.retain(|p| !p.starts_with(&parts));
    }
}

impl SimTree {
    fn is_dir(&self, parts: &[String]) -> bool {
        parts.is_empty() || self.dirs.contains(parts)
    }

    fn not_found(path: &RelPath) -> DirError {
        DirError::new(DirErrorKind::NotFound, path.to_string())
    }

    /// Checks what writing `contents` to `path` needs: an existing parent, no directory in the
    /// way, and room for any growth.
    fn check_write(&self, path: &RelPath, contents: &[u8]) -> Result<(), DirError> {
        let (parent, _) = path
            .split_last()
            .ok_or_else(|| DirError::new(DirErrorKind::Invalid, "the tree itself is not a file"))?;
        if !self.is_dir(parent.parts()) {
            let kind = if self.files.contains_key(parent.parts()) {
                DirErrorKind::WrongKind
            } else {
                DirErrorKind::NotFound
            };
            return Err(DirError::new(kind, parent.to_string()));
        }
        if self.is_dir(path.parts()) {
            return Err(DirError::new(
                DirErrorKind::WrongKind,
                format!("{path}: is a directory"),
            ));
        }
        let old = self.files.get(path.parts()).map_or(0, Vec::len);
        let growth = contents.len().saturating_sub(old) as u64;
        if self.space.is_some_and(|free| growth > free) {
            return Err(DirError::new(DirErrorKind::Full, path.to_string()));
        }
        Ok(())
    }

    fn write(&mut self, path: &RelPath, contents: &[u8]) {
        let old = self
            .files
            .insert(path.parts().to_vec(), contents.to_vec())
            .map_or(0, |f| f.len());
        if let Some(free) = &mut self.space {
            *free = (*free + old as u64).saturating_sub(contents.len() as u64);
        }
    }
}

impl Dir for SimDir {
    fn metadata(&self, path: &RelPath) -> Result<Meta, DirError> {
        let tree = self.plugged_tree()?;
        if tree.is_dir(path.parts()) {
            return Ok(Meta {
                is_dir: true,
                len: 0,
                modified: None,
            });
        }
        let file = tree
            .files
            .get(path.parts())
            .ok_or_else(|| SimTree::not_found(path))?;
        Ok(Meta {
            is_dir: false,
            len: file.len() as u64,
            modified: None,
        })
    }

    fn open(&self, path: &RelPath) -> Result<Box<dyn DirFile>, DirError> {
        let tree = self.plugged_tree()?;
        if tree.is_dir(path.parts()) {
            return Err(DirError::new(
                DirErrorKind::WrongKind,
                format!("{path}: is a directory"),
            ));
        }
        let file = tree
            .files
            .get(path.parts())
            .ok_or_else(|| SimTree::not_found(path))?;
        Ok(Box::new(std::io::Cursor::new(file.clone())))
    }

    fn list(&self, path: &RelPath) -> Result<Vec<DirEntry>, DirError> {
        let tree = self.plugged_tree()?;
        let parts = path.parts();
        if !tree.is_dir(parts) {
            let kind = if tree.files.contains_key(parts) {
                DirErrorKind::WrongKind
            } else {
                DirErrorKind::NotFound
            };
            return Err(DirError::new(kind, path.to_string()));
        }
        let child = |p: &Vec<String>| {
            (p.len() == parts.len() + 1 && p.starts_with(parts)).then(|| p[parts.len()].clone())
        };
        let mut entries: Vec<DirEntry> = tree
            .dirs
            .iter()
            .filter_map(child)
            .map(|name| DirEntry { name, is_dir: true })
            .chain(tree.files.keys().filter_map(child).map(|name| DirEntry {
                name,
                is_dir: false,
            }))
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }
}

impl DirWrite for SimDir {
    fn create_dir(&self, path: &RelPath) -> Result<(), DirError> {
        let mut tree = self.plugged_tree()?;
        let parts = path.parts();
        for i in 1..=parts.len() {
            if tree.files.contains_key(&parts[..i]) {
                return Err(DirError::new(
                    DirErrorKind::WrongKind,
                    format!("{}: is a file", parts[..i].join("/")),
                ));
            }
        }
        for i in 1..=parts.len() {
            tree.dirs.insert(parts[..i].to_vec());
        }
        Ok(())
    }

    fn create_new(&self, path: &RelPath, contents: &[u8]) -> Result<(), DirError> {
        let mut tree = self.plugged_tree()?;
        if path.is_root() {
            return Err(DirError::new(
                DirErrorKind::Invalid,
                "the tree itself is not a file",
            ));
        }
        if tree.files.contains_key(path.parts()) || tree.is_dir(path.parts()) {
            return Err(DirError::new(DirErrorKind::Exists, path.to_string()));
        }
        tree.check_write(path, contents)?;
        tree.write(path, contents);
        Ok(())
    }

    fn available_space(&self, path: &RelPath) -> Result<u64, DirError> {
        let tree = self.plugged_tree()?;
        if !tree.is_dir(path.parts()) {
            return Err(SimTree::not_found(path));
        }
        Ok(tree.space.unwrap_or(u64::MAX))
    }
}

impl DirModify for SimDir {
    fn replace(&self, path: &RelPath, contents: &[u8]) -> Result<(), DirError> {
        let mut tree = self.plugged_tree()?;
        tree.check_write(path, contents)?;
        tree.write(path, contents);
        Ok(())
    }

    fn remove_file(&self, path: &RelPath) -> Result<(), DirError> {
        let mut tree = self.plugged_tree()?;
        if tree.is_dir(path.parts()) {
            return Err(DirError::new(
                DirErrorKind::WrongKind,
                format!("{path}: is a directory"),
            ));
        }
        let old = tree
            .files
            .remove(path.parts())
            .ok_or_else(|| SimTree::not_found(path))?;
        if let Some(free) = &mut tree.space {
            *free += old.len() as u64;
        }
        Ok(())
    }
}

/// A user who approves in the browser, scripted. Behind the same scope check as production;
/// records what would have been opened.
#[derive(Clone)]
pub struct SimAuthorize {
    scope: Arc<Scope>,
    redirect_uri: String,
    answers: Arc<Mutex<VecDeque<Result<Params, AuthError>>>>,
    opened: Arc<Mutex<Vec<String>>>,
}

impl SimAuthorize {
    pub fn new(scope: Scope, redirect_uri: &str) -> Self {
        Self {
            scope: Arc::new(scope),
            redirect_uri: redirect_uri.to_string(),
            answers: Arc::default(),
            opened: Arc::default(),
        }
    }

    /// The next `authorize` returns these parameters, as if the browser came back with them.
    /// `state` from the opened URL is echoed back unless `params` has one.
    pub fn approve(&self, params: &[(&str, &str)]) {
        let params = params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        lock(&self.answers).push_back(Ok(params));
    }

    /// The next `authorize` fails with `kind` (e.g. `Timeout`: the user never came back).
    pub fn fail(&self, kind: AuthErrorKind) {
        lock(&self.answers).push_back(Err(AuthError::new(kind, "injected failure")));
    }

    /// URLs that passed the scope check, in order.
    pub fn opened(&self) -> Vec<String> {
        lock(&self.opened).clone()
    }
}

impl Authorize for SimAuthorize {
    fn redirect_uri(&self) -> String {
        self.redirect_uri.clone()
    }

    fn authorize(&self, url: &str, _timeout: Duration) -> Result<Params, AuthError> {
        self.scope.check_url(url)?;
        lock(&self.opened).push(url.to_string());
        let mut params = lock(&self.answers)
            .pop_front()
            .unwrap_or_else(|| Err(AuthError::new(AuthErrorKind::Timeout, "nobody approved")))?;
        // Echo `state` from the URL unless the test set one: what a real provider does.
        if !params.iter().any(|(k, _)| k == "state")
            && let Some((_, query)) = url.split_once('?')
            && let Some(state) = ambit_caps_net::parse_query(query)
                .into_iter()
                .find(|(k, _)| k == "state")
        {
            params.push(state);
        }
        Ok(params)
    }
}
