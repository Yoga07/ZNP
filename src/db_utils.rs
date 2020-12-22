use rocksdb::{DBCompressionType, Error as DBError, Options, DB};
use std::collections::HashMap;
use std::fmt;
use tracing::warn;

/// Database that can store in memory or using rocksDB.
pub enum SimpleDb {
    File {
        options: Options,
        path: String,
        db: DB,
    },
    InMemory {
        key_values: HashMap<Vec<u8>, Vec<u8>>,
    },
}

impl fmt::Debug for SimpleDb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File { .. } => write!(f, "SimpleDb::File"),
            Self::InMemory { .. } => write!(f, "SimpleDb::InMemory"),
        }
    }
}

impl Drop for SimpleDb {
    fn drop(&mut self) {
        self.destroy();
    }
}

impl SimpleDb {
    /// Create rocksDB
    pub fn new_file(path: String) -> Result<Self, DBError> {
        let options = get_db_options();
        let db = DB::open(&options, path.clone())?;
        Ok(Self::File { options, path, db })
    }

    /// Create in memory db
    pub fn new_in_memory() -> Self {
        let key_values = Default::default();
        Self::InMemory { key_values }
    }

    fn destroy(&mut self) {
        match self {
            Self::File { options, path, .. } => {
                if let Err(e) = DB::destroy(options, path.clone()) {
                    // Note: This seem to always happen.
                    warn!("Db(path) Failed to destroy: {:?}", e);
                }
            }
            Self::InMemory { .. } => (),
        }
    }

    /// Add entry to database
    pub fn put<K, V>(&mut self, key: K, value: V) -> Result<(), DBError>
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        match self {
            Self::File { db, .. } => {
                db.put(key, value)?;
            }
            Self::InMemory { key_values } => {
                key_values.insert(key.as_ref().to_vec(), value.as_ref().to_vec());
            }
        }
        Ok(())
    }
}

/// Creates a set of DB opening options for rocksDB instances
pub fn get_db_options() -> Options {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.set_compression_type(DBCompressionType::Snappy);

    opts
}
