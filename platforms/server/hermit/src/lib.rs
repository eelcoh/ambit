//! Hermit unikernel backend for the server platform. The same app as native, in an image that
//! contains only the kernel features its capabilities need.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard};

use ambit_caps::{Entry, Kv, KvError, Random};

pub use ambit_server_std::{Listen, MAX_BODY, StdClock, serve};

// Links the kernel into every image built with this backend.
#[cfg(target_os = "hermit")]
use hermit as _;

/// Installs the tracing subscriber. Output goes to the serial console.
pub fn init_tracing() {
    tracing_subscriber::fmt().with_ansi(false).init();
}

/// The kernel CSPRNG: ChaCha20, seeded from RDSEED and reseeded every second.
///
/// Fails closed. Without RDSEED the kernel silently falls back to a timestamp-seeded LCG and
/// still reports success, so construction checks for it and refuses to start.
pub struct HermitRandom(());

#[derive(Debug)]
pub struct NoEntropy;

impl fmt::Display for NoEntropy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CPU has no RDSEED: the kernel would fall back to predictable randomness")
    }
}

impl std::error::Error for NoEntropy {}

impl HermitRandom {
    pub fn new() -> Result<Self, NoEntropy> {
        if cpu_has_rdseed() {
            Ok(Self(()))
        } else {
            Err(NoEntropy)
        }
    }
}

/// Asks the CPU at runtime. Not `is_x86_feature_detected!`: the hermit target enables `rdseed`
/// as a compile-time target feature, which makes that macro always return true.
#[cfg(target_arch = "x86_64")]
fn cpu_has_rdseed() -> bool {
    use std::arch::x86_64::__cpuid_count;
    // CPUID leaf 7, subleaf 0: EBX bit 18 is RDSEED.
    __cpuid_count(0, 0).eax >= 7 && __cpuid_count(7, 0).ebx & (1 << 18) != 0
}

#[cfg(not(target_arch = "x86_64"))]
fn cpu_has_rdseed() -> bool {
    false
}

impl Random for HermitRandom {
    fn fill(&self, buf: &mut [u8]) {
        getrandom::fill(buf).expect("kernel random source unavailable");
    }
}

/// An in-memory store. **Not durable**: everything is lost when the VM stops. Hermit has no
/// storage capability yet; wiring this is an explicit choice to accept that.
#[derive(Default)]
pub struct EphemeralKv {
    map: Mutex<BTreeMap<Vec<u8>, Vec<u8>>>,
}

impl EphemeralKv {
    pub fn new() -> Self {
        Self::default()
    }

    fn map(&self) -> MutexGuard<'_, BTreeMap<Vec<u8>, Vec<u8>>> {
        self.map
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Kv for EphemeralKv {
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
