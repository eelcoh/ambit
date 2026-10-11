//! Capability traits shared by all platforms. An app gets authority only through these.
#![forbid(unsafe_code)]

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Wall-clock and monotonic time.
pub trait Clock: Send + Sync + 'static {
    /// Current wall-clock time. Can jump (NTP, sim).
    fn now(&self) -> SystemTime;
    /// Monotonic time since the clock was created. Never goes backwards.
    fn elapsed(&self) -> Duration;
}

/// Random bytes: a CSPRNG on real backends, seeded and reproducible in sim.
pub trait Random: Send + Sync + 'static {
    fn fill(&self, buf: &mut [u8]);

    fn u64(&self) -> u64 {
        let mut buf = [0u8; 8];
        self.fill(&mut buf);
        u64::from_le_bytes(buf)
    }
}

/// A key and its value.
pub type Entry = (Vec<u8>, Vec<u8>);

/// Durable key-value storage, scoped to one app.
pub trait Kv: Send + Sync + 'static {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, KvError>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), KvError>;
    /// Deleting a missing key is not an error.
    fn delete(&self, key: &[u8]) -> Result<(), KvError>;
    /// All entries whose key starts with `prefix`, in key order.
    fn scan(&self, prefix: &[u8]) -> Result<Vec<Entry>, KvError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvError(String);

impl KvError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for KvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "kv: {}", self.0)
    }
}

impl std::error::Error for KvError {}

// Shared ownership of a capability is the same capability.

impl<T: Clock + ?Sized> Clock for Arc<T> {
    fn now(&self) -> SystemTime {
        (**self).now()
    }
    fn elapsed(&self) -> Duration {
        (**self).elapsed()
    }
}

impl<T: Random + ?Sized> Random for Arc<T> {
    fn fill(&self, buf: &mut [u8]) {
        (**self).fill(buf)
    }
}

impl<T: Kv + ?Sized> Kv for Arc<T> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, KvError> {
        (**self).get(key)
    }
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), KvError> {
        (**self).put(key, value)
    }
    fn delete(&self, key: &[u8]) -> Result<(), KvError> {
        (**self).delete(key)
    }
    fn scan(&self, prefix: &[u8]) -> Result<Vec<Entry>, KvError> {
        (**self).scan(prefix)
    }
}
