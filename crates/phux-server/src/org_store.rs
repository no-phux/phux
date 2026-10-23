//! The organization store (ADR-0162): where committed sessions, resources,
//! arrangements, and receipts live.
//!
//! [`OrgStore`] is the seam [`crate::ServerState::commit`] writes through:
//! every transition commits here, in one transaction with its receipt and
//! the journal position, before memory changes. [`SqliteOrgStore`] is the
//! durable store; [`MemOrgStore`] keeps nothing and serves in-process
//! servers and tests.
//!
//! Writes are incremental: each [`Delta`] upserts or deletes one row, so a
//! membership move touches a few rows however many records exist. Records
//! are stored as JSON beside the indexed columns queries need (session
//! membership, execution state, intake).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use phux_core::{
    Arrangement, ArrangementOwner, Authority, AuthorityId, Delta, Key16 as _, Org, Receipt,
    Resource, Session, SessionKey, Stamp, Transition,
};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

/// Receipts older than this are pruned (CONTRACTS §06).
pub const RECEIPT_HORIZON_MS: u64 = 24 * 60 * 60 * 1000;

/// At most this many receipts are kept; the oldest go first.
pub const RECEIPT_CAP: usize = 10_000;

/// Tombstones of removed resources are kept this long.
pub const TOMBSTONE_HORIZON_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// The migration receipt naming the first-start import of a live registry
/// (MIGRATION §07). The import runs only for a handoff that carries no
/// records; the receipt records that it happened.
pub const LIVE_IMPORT_MIGRATION: &str = "import-live-registry/v1";

/// Where a server keeps its organization store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum OrgStoreLocation {
    /// A SQLite file at this path.
    Path(PathBuf),
    /// Process memory only: records die with the server. In-process tests
    /// and embedders that persist nothing use it.
    #[default]
    Memory,
}

impl OrgStoreLocation {
    /// The production location for the server listening on `socket`.
    ///
    /// The profile's own socket (`<runtime_dir>/phux.sock`) keeps
    /// `<state_dir>/org/v1.sqlite3`. Any other socket gets a store of its
    /// own under `<state_dir>/org/sockets/`, named by the escaped socket
    /// path, so two servers of one profile never share records: each would
    /// otherwise write its own sessions into the other's store, and a
    /// session name both used would make the next start refuse the store.
    #[must_use]
    pub fn for_socket(socket: &Path) -> Self {
        let org = phux_config::instance::state_dir().join("org");
        if socket == phux_config::instance::runtime_dir().join("phux.sock") {
            return Self::Path(org.join("v1.sqlite3"));
        }
        Self::Path(
            org.join("sockets")
                .join(format!("{}.v1.sqlite3", escape_socket(socket))),
        )
    }

    /// Open the store this location names.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the file cannot be opened, migrated, or is a
    /// newer schema than this binary reads.
    pub fn open(&self) -> Result<Box<dyn OrgStore>, StoreError> {
        Ok(match self {
            Self::Path(path) => Box::new(SqliteOrgStore::open(path)?),
            Self::Memory => Box::new(MemOrgStore::default()),
        })
    }
}

/// A socket path as one file name: ASCII letters, digits, `-`, and `_`
/// stand for themselves; every other byte is `%XX`, so distinct paths
/// never share a name.
fn escape_socket(socket: &Path) -> String {
    use std::fmt::Write as _;
    use std::os::unix::ffi::OsStrExt as _;
    let mut name = String::new();
    for &byte in socket.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' {
            name.push(char::from(byte));
        } else {
            let _ = write!(name, "%{byte:02X}");
        }
    }
    name
}

/// Why a store operation failed. A failed commit leaves the store as it
/// was.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite reported an error.
    #[error("org store: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A record did not encode or decode.
    #[error("org store record codec: {0}")]
    Codec(#[from] serde_json::Error),
    /// Creating the store directory or its backup failed.
    #[error("org store io: {0}")]
    Io(#[from] std::io::Error),
    /// The file's schema is newer than this binary reads.
    #[error("org store schema version {found} is newer than this binary reads ({supported})")]
    UnsupportedVersion {
        /// The file's version.
        found: i64,
        /// The newest version this binary reads.
        supported: i64,
    },
    /// Stored data is inconsistent.
    #[error("org store corrupt: {0}")]
    Corrupt(String),
    /// A test asked the store to fail.
    #[error("org store write failed (injected)")]
    Injected,
}

/// Everything a store holds, as loaded at startup.
#[derive(Debug, Clone)]
pub struct StoredOrg {
    /// The authority record and its designated Intake.
    pub authority: Authority,
    /// Every session record.
    pub sessions: Vec<Session>,
    /// Every resource record.
    pub resources: Vec<Resource>,
    /// Every arrangement record.
    pub arrangements: Vec<Arrangement>,
    /// Every unexpired receipt.
    pub receipts: Vec<Receipt>,
    /// The journal position of the last commit.
    pub journal_seq: u64,
}

/// The durable side of [`crate::ServerState::commit`].
pub trait OrgStore: Send + fmt::Debug {
    /// The stored organization, or `None` when the store has never been
    /// written.
    ///
    /// # Errors
    ///
    /// [`StoreError`] on a read or decode failure.
    fn load(&mut self) -> Result<Option<StoredOrg>, StoreError>;

    /// Commit one transition, its receipt, and the journal position in one
    /// transaction, then prune expired receipts and tombstones.
    ///
    /// # Errors
    ///
    /// [`StoreError`]; nothing was written.
    fn commit(
        &mut self,
        transition: &Transition,
        journal_seq: u64,
        now: Stamp,
    ) -> Result<(), StoreError>;

    /// Write a whole organization in one transaction: the authority, every
    /// record, and, when given, a migration receipt. Records already stored
    /// under the same keys are replaced.
    ///
    /// # Errors
    ///
    /// [`StoreError`]; nothing was written.
    fn import(&mut self, org: &Org, migration: Option<&str>, now: Stamp) -> Result<(), StoreError>;

    /// Whether the named migration receipt is stored.
    ///
    /// # Errors
    ///
    /// [`StoreError`] on a read failure.
    fn has_migration(&mut self, name: &str) -> Result<bool, StoreError>;

    /// Where the store lives, for logs; `None` in memory.
    fn path(&self) -> Option<&Path>;
}

/// A store that keeps nothing. Commits succeed unless a test armed
/// [`MemOrgStore::fail_commits`].
#[derive(Debug, Clone, Default)]
pub struct MemOrgStore {
    fail: Arc<AtomicBool>,
    migrations: Vec<String>,
}

impl MemOrgStore {
    /// A handle that makes every later commit and import fail while set.
    #[must_use]
    pub fn fail_commits(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.fail)
    }

    fn check(&self) -> Result<(), StoreError> {
        if self.fail.load(Ordering::Relaxed) {
            Err(StoreError::Injected)
        } else {
            Ok(())
        }
    }
}

impl OrgStore for MemOrgStore {
    fn load(&mut self) -> Result<Option<StoredOrg>, StoreError> {
        Ok(None)
    }

    fn commit(&mut self, _: &Transition, _: u64, _: Stamp) -> Result<(), StoreError> {
        self.check()
    }

    fn import(&mut self, _: &Org, migration: Option<&str>, _: Stamp) -> Result<(), StoreError> {
        self.check()?;
        if let Some(name) = migration {
            self.migrations.push(name.to_owned());
        }
        Ok(())
    }

    fn has_migration(&mut self, name: &str) -> Result<bool, StoreError> {
        Ok(self.migrations.iter().any(|m| m == name))
    }

    fn path(&self) -> Option<&Path> {
        None
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests;

mod sqlite;
pub use sqlite::*;
