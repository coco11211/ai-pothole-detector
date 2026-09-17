//! CHAINNAME persistent storage.
//!
//! Backed by `redb`. See DECISIONS.md D-008 for why redb rather than MDBX or
//! reth's table schema: we need neither reth's schema nor its static-file
//! layout, and redb is pure Rust with no C toolchain dependency. The decision
//! is tagged OPEN — the backend sits behind [`BlockStore`] so it can be
//! swapped crate-locally if M8's soak shows write amplification hurting.

use std::path::{Path, PathBuf};

use alloy_primitives::B256;
use chainname_primitives::{BlockHash, Header};
use redb::{Database, ReadableTable, TableDefinition};

/// Raw header bytes, keyed by block hash.
const HEADERS: TableDefinition<'_, [u8; 32], Vec<u8>> = TableDefinition::new("headers");
/// Raw block body bytes, keyed by block hash.
const BODIES: TableDefinition<'_, [u8; 32], Vec<u8>> = TableDefinition::new("bodies");
/// Small singleton values (schema version, tip pointers), keyed by name.
const META: TableDefinition<'_, &str, Vec<u8>> = TableDefinition::new("meta");

/// On-disk schema version. Bumped whenever a table's layout changes.
pub const SCHEMA_VERSION: u32 = 1;

/// Meta key holding [`SCHEMA_VERSION`].
const META_SCHEMA_VERSION: &str = "schema_version";

/// Read/write access to persisted blocks.
///
/// Fronted as a trait so the backend stays swappable (DECISIONS.md D-008).
pub trait BlockStore {
    /// Persists a header.
    fn put_header(&self, hash: BlockHash, header: &Header) -> Result<(), StorageError>;
    /// Loads a header, if present.
    fn get_header(&self, hash: BlockHash) -> Result<Option<Header>, StorageError>;
    /// True if the header is present.
    fn has_header(&self, hash: BlockHash) -> Result<bool, StorageError>;
    /// Persists raw body bytes.
    fn put_body(&self, hash: BlockHash, body: &[u8]) -> Result<(), StorageError>;
    /// Loads raw body bytes, if present.
    fn get_body(&self, hash: BlockHash) -> Result<Option<Vec<u8>>, StorageError>;
}

/// A redb-backed store.
#[derive(Debug)]
pub struct RedbStore {
    db: Database,
    path: PathBuf,
}

impl RedbStore {
    /// Opens or creates the database at `path`, then initialises or verifies
    /// the schema version.
    ///
    /// A mismatched schema version is a hard error rather than a silent
    /// migration: consensus data must never be reinterpreted by guesswork.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Database::create(&path)?;
        let store = Self { db, path };
        store.init_schema()?;
        Ok(store)
    }

    /// Path this store was opened at.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn init_schema(&self) -> Result<(), StorageError> {
        let txn = self.db.begin_write()?;
        {
            // Opening each table creates it if absent.
            let _ = txn.open_table(HEADERS)?;
            let _ = txn.open_table(BODIES)?;
            let mut meta = txn.open_table(META)?;
            // Read into an owned value before touching `meta` mutably: redb's
            // AccessGuard borrows the table for as long as it is alive.
            let stored: Option<Vec<u8>> = meta.get(META_SCHEMA_VERSION)?.map(|found| found.value());
            match stored {
                Some(bytes) => {
                    let found: [u8; 4] = bytes
                        .as_slice()
                        .try_into()
                        .map_err(|_| StorageError::CorruptMeta { key: META_SCHEMA_VERSION })?;
                    let found = u32::from_le_bytes(found);
                    if found != SCHEMA_VERSION {
                        return Err(StorageError::SchemaMismatch {
                            found,
                            expected: SCHEMA_VERSION,
                        });
                    }
                }
                None => {
                    meta.insert(META_SCHEMA_VERSION, SCHEMA_VERSION.to_le_bytes().to_vec())?;
                }
            }
        }
        txn.commit()?;
        Ok(())
    }
}

impl BlockStore for RedbStore {
    fn put_header(&self, hash: BlockHash, header: &Header) -> Result<(), StorageError> {
        let bytes = header.encoded();
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(HEADERS)?;
            table.insert(key(hash), bytes)?;
        }
        txn.commit()?;
        Ok(())
    }

    fn get_header(&self, hash: BlockHash) -> Result<Option<Header>, StorageError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(HEADERS)?;
        let Some(found) = table.get(key(hash))? else { return Ok(None) };
        let bytes = found.value();
        let header = <Header as alloy_rlp::Decodable>::decode(&mut bytes.as_slice())
            .map_err(|source| StorageError::Decode { hash, source })?;
        Ok(Some(header))
    }

    fn has_header(&self, hash: BlockHash) -> Result<bool, StorageError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(HEADERS)?;
        Ok(table.get(key(hash))?.is_some())
    }

    fn put_body(&self, hash: BlockHash, body: &[u8]) -> Result<(), StorageError> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(BODIES)?;
            table.insert(key(hash), body.to_vec())?;
        }
        txn.commit()?;
        Ok(())
    }

    fn get_body(&self, hash: BlockHash) -> Result<Option<Vec<u8>>, StorageError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(BODIES)?;
        Ok(table.get(key(hash))?.map(|v| v.value()))
    }
}

fn key(hash: B256) -> [u8; 32] {
    hash.0
}

/// Storage failures.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// Filesystem error opening or creating the database.
    #[error("storage io error: {0}")]
    Io(#[from] std::io::Error),
    // redb's error types are large (~160 bytes for the biggest), which would
    // make every `Result` in this crate expensive to move. Boxed so the happy
    // path stays cheap.
    /// redb failed to open the database file.
    #[error("database error: {0}")]
    Database(Box<redb::DatabaseError>),
    /// A redb transaction failed.
    #[error("transaction error: {0}")]
    Transaction(Box<redb::TransactionError>),
    /// A redb table operation failed.
    #[error("table error: {0}")]
    Table(Box<redb::TableError>),
    /// A redb read or write failed.
    #[error("storage error: {0}")]
    Storage(Box<redb::StorageError>),
    /// A redb commit failed.
    #[error("commit error: {0}")]
    Commit(Box<redb::CommitError>),
    /// Stored bytes did not decode as the expected type.
    #[error("failed to decode stored header {hash}: {source}")]
    Decode {
        /// Which block failed to decode.
        hash: BlockHash,
        /// The underlying RLP error.
        source: alloy_rlp::Error,
    },
    /// A meta value was present but malformed.
    #[error("corrupt meta value for key {key}")]
    CorruptMeta {
        /// The meta key.
        key: &'static str,
    },
    /// The database was written by a different schema version.
    #[error("schema version mismatch: database has {found}, this build expects {expected}")]
    SchemaMismatch {
        /// Version found on disk.
        found: u32,
        /// Version this build expects.
        expected: u32,
    },
}

macro_rules! boxed_from {
    ($($src:ty => $variant:ident),* $(,)?) => {
        $(
            impl From<$src> for StorageError {
                fn from(source: $src) -> Self {
                    Self::$variant(Box::new(source))
                }
            }
        )*
    };
}

boxed_from! {
    redb::DatabaseError => Database,
    redb::TransactionError => Transaction,
    redb::TableError => Table,
    redb::StorageError => Storage,
    redb::CommitError => Commit,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256, b256};
    use chainname_primitives::HEADER_VERSION;

    fn sample_header() -> Header {
        Header {
            version: HEADER_VERSION,
            parents: vec![b256!(
                "0000000000000000000000000000000000000000000000000000000000000001"
            )],
            timestamp_ms: 1_700_000_000_000,
            bits: 0x1d00_ffff,
            nonce: 42,
            miner: Address::repeat_byte(7),
            txs_root: B256::ZERO,
            deferred_height: 0,
            deferred_state_root: B256::ZERO,
            deferred_receipts_root: B256::ZERO,
            deferred_gas_used: 0,
        }
    }

    #[test]
    fn opens_and_initialises_schema() {
        let dir = tempfile::tempdir().unwrap();
        let store = RedbStore::open(dir.path().join("db").join("chain.redb")).unwrap();
        assert!(store.path().exists());
    }

    #[test]
    fn reopen_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chain.redb");
        drop(RedbStore::open(&path).unwrap());
        drop(RedbStore::open(&path).unwrap());
    }

    #[test]
    fn header_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let store = RedbStore::open(dir.path().join("chain.redb")).unwrap();
        let header = sample_header();
        let hash = header.hash();

        assert!(!store.has_header(hash).unwrap());
        store.put_header(hash, &header).unwrap();
        assert!(store.has_header(hash).unwrap());
        assert_eq!(store.get_header(hash).unwrap().unwrap(), header);
    }

    #[test]
    fn body_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let store = RedbStore::open(dir.path().join("chain.redb")).unwrap();
        let hash = sample_header().hash();
        assert_eq!(store.get_body(hash).unwrap(), None);
        store.put_body(hash, b"body-bytes").unwrap();
        assert_eq!(store.get_body(hash).unwrap().unwrap(), b"body-bytes".to_vec());
    }

    #[test]
    fn missing_header_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = RedbStore::open(dir.path().join("chain.redb")).unwrap();
        assert_eq!(store.get_header(B256::repeat_byte(9)).unwrap(), None);
    }
}
