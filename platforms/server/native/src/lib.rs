//! Native backend for the server platform: development, tests and debugging on Linux/Mac.
#![forbid(unsafe_code)]

use std::io::Read;
use std::net::SocketAddr;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use ambit_caps::{Clock, Entry, Kv, KvError, Random};
use ambit_server::http::{self, StatusCode};
use ambit_server::{Handler, Request, Response};
use redb::{Database, TableDefinition};

/// Requests with a larger body are rejected with 413.
pub const MAX_BODY: u64 = 1024 * 1024;

pub struct Listen {
    pub addr: SocketAddr,
    pub threads: usize,
}

/// Installs the tracing subscriber. The platform owns it; the app only uses the facade.
pub fn init_tracing() {
    tracing_subscriber::fmt().init();
}

/// Serves `handler` until the listener fails.
pub fn serve(handler: impl Handler, listen: Listen) -> std::io::Result<()> {
    let server = Arc::new(tiny_http::Server::http(listen.addr).map_err(std::io::Error::other)?);
    let handler = Arc::new(handler);
    tracing::info!(addr = %listen.addr, threads = listen.threads, "listening");

    let workers: Vec<_> = (0..listen.threads.max(1))
        .map(|_| {
            let server = Arc::clone(&server);
            let handler = Arc::clone(&handler);
            thread::spawn(move || {
                loop {
                    match server.recv() {
                        Ok(request) => handle_one(&*handler, request),
                        Err(e) => {
                            tracing::error!(error = %e, "accept failed");
                            break;
                        }
                    }
                }
            })
        })
        .collect();
    for worker in workers {
        let _ = worker.join();
    }
    Ok(())
}

fn handle_one(handler: &impl Handler, mut raw: tiny_http::Request) {
    let response = match to_http(&mut raw) {
        Ok(request) => match panic::catch_unwind(AssertUnwindSafe(|| handler.handle(request))) {
            Ok(response) => response,
            Err(_) => {
                tracing::error!(url = raw.url(), "handler panicked");
                status_only(StatusCode::INTERNAL_SERVER_ERROR)
            }
        },
        Err(status) => status_only(status),
    };
    if let Err(e) = raw.respond(from_http(response)) {
        tracing::warn!(error = %e, "failed to send response");
    }
}

fn to_http(raw: &mut tiny_http::Request) -> Result<Request, StatusCode> {
    let mut builder = http::Request::builder()
        .method(raw.method().as_str())
        .uri(raw.url());
    for header in raw.headers() {
        builder = builder.header(header.field.as_str().as_str(), header.value.as_str());
    }
    let mut body = Vec::new();
    raw.as_reader()
        .take(MAX_BODY + 1)
        .read_to_end(&mut body)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if body.len() as u64 > MAX_BODY {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    builder.body(body).map_err(|_| StatusCode::BAD_REQUEST)
}

fn from_http(response: Response) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let (parts, body) = response.into_parts();
    let mut out = tiny_http::Response::from_data(body).with_status_code(parts.status.as_u16());
    for (name, value) in &parts.headers {
        if let Ok(header) =
            tiny_http::Header::from_bytes(name.as_str().as_bytes(), value.as_bytes())
        {
            out.add_header(header);
        }
    }
    out
}

fn status_only(status: StatusCode) -> Response {
    let mut response = Response::new(Vec::new());
    *response.status_mut() = status;
    response
}

/// The system clock.
pub struct NativeClock {
    start: Instant,
}

impl NativeClock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for NativeClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for NativeClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
    fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}

/// The OS CSPRNG.
pub struct OsRandom;

impl Random for OsRandom {
    fn fill(&self, buf: &mut [u8]) {
        getrandom::fill(buf).expect("OS random source unavailable");
    }
}

const TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("kv");

/// A single-file store (redb).
pub struct NativeKv {
    db: Database,
}

impl NativeKv {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, KvError> {
        let path = path.as_ref();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(err)?;
        }
        let db = Database::create(path).map_err(err)?;
        let txn = db.begin_write().map_err(err)?;
        txn.open_table(TABLE).map_err(err)?;
        txn.commit().map_err(err)?;
        Ok(Self { db })
    }
}

fn err(e: impl std::fmt::Display) -> KvError {
    KvError::new(e.to_string())
}

impl Kv for NativeKv {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, KvError> {
        let txn = self.db.begin_read().map_err(err)?;
        let table = txn.open_table(TABLE).map_err(err)?;
        Ok(table.get(key).map_err(err)?.map(|v| v.value().to_vec()))
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), KvError> {
        let txn = self.db.begin_write().map_err(err)?;
        {
            let mut table = txn.open_table(TABLE).map_err(err)?;
            table.insert(key, value).map_err(err)?;
        }
        txn.commit().map_err(err)
    }

    fn delete(&self, key: &[u8]) -> Result<(), KvError> {
        let txn = self.db.begin_write().map_err(err)?;
        {
            let mut table = txn.open_table(TABLE).map_err(err)?;
            table.remove(key).map_err(err)?;
        }
        txn.commit().map_err(err)
    }

    fn scan(&self, prefix: &[u8]) -> Result<Vec<Entry>, KvError> {
        let txn = self.db.begin_read().map_err(err)?;
        let table = txn.open_table(TABLE).map_err(err)?;
        let mut out = Vec::new();
        for entry in table.range::<&[u8]>(prefix..).map_err(err)? {
            let (key, value) = entry.map_err(err)?;
            if !key.value().starts_with(prefix) {
                break;
            }
            out.push((key.value().to_vec(), value.value().to_vec()));
        }
        Ok(out)
    }
}
