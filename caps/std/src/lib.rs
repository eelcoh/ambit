//! Capabilities that need only Rust std, for every backend with a real clock (native, hermit):
//! the std clock, and an in-memory store for when nothing should persist (a demo mode).
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use ambit_caps::{Clock, Entry, Kv, KvError};

/// The std clock: `SystemTime` for wall time, `Instant` for monotonic time.
pub struct StdClock {
    start: Instant,
}

impl StdClock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for StdClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for StdClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
    fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}

/// A store that lives as long as the process. Clones share the same contents.
#[derive(Clone, Default)]
pub struct MemKv {
    map: Arc<Mutex<BTreeMap<Vec<u8>, Vec<u8>>>>,
}

impl MemKv {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.map().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn map(&self) -> MutexGuard<'_, BTreeMap<Vec<u8>, Vec<u8>>> {
        self.map
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Kv for MemKv {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, KvError> {
        Ok(self.map().get(key).cloned())
    }
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), KvError> {
        self.map().insert(key.to_vec(), value.to_vec());
        Ok(())
    }
    fn delete(&self, key: &[u8]) -> Result<(), KvError> {
        self.map().remove(key);
        Ok(())
    }
    fn scan(&self, prefix: &[u8]) -> Result<Vec<Entry>, KvError> {
        Ok(self
            .map()
            .range(prefix.to_vec()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }
}
