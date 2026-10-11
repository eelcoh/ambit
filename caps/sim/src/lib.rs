//! Deterministic, in-memory capabilities for tests. Handles are `Clone`, so a test keeps control
//! after handing one to `init`.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use std::net::IpAddr;

use ambit_caps::{Clock, Entry, Kv, KvError, Random};
use ambit_caps_net::{
    Device, Discovery, DiscoveryError, Http, HttpError, HttpErrorKind, Request, Response, Scope,
    Targets, http,
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

/// An in-memory store, with injectable failures.
#[derive(Clone, Default)]
pub struct MemKv {
    map: Arc<Mutex<BTreeMap<Vec<u8>, Vec<u8>>>>,
    faults: Arc<Mutex<Faults>>,
}

impl MemKv {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        lock(&self.map).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
        Ok(lock(&self.map).get(key).cloned())
    }
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), KvError> {
        self.check(true)?;
        lock(&self.map).insert(key.to_vec(), value.to_vec());
        Ok(())
    }
    fn delete(&self, key: &[u8]) -> Result<(), KvError> {
        self.check(true)?;
        lock(&self.map).remove(key);
        Ok(())
    }
    fn scan(&self, prefix: &[u8]) -> Result<Vec<Entry>, KvError> {
        self.check(false)?;
        Ok(lock(&self.map)
            .range(prefix.to_vec()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
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
