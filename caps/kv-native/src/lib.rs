//! The single-file store (redb) for Linux/Mac, apart from the other native capabilities so a
//! program without one doesn't carry a database.
#![forbid(unsafe_code)]

use std::path::Path;

use ambit_caps::{Entry, Kv, KvError};
use redb::{Database, TableDefinition};

const TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("kv");

/// A single-file store (redb).
pub struct NativeKv {
    db: Database,
    /// For the lock.
    path: std::path::PathBuf,
}

/// The database file, read and written.
impl ambit_lockdown::Grants for NativeKv {
    fn grants(&self) -> Vec<ambit_lockdown::Grant> {
        vec![ambit_lockdown::Grant::ReadWrite(self.path.clone())]
    }
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
        Ok(Self {
            db,
            path: path.to_path_buf(),
        })
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
