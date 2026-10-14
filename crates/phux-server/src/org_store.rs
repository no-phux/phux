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

/// Schema migrations, in order. `PRAGMA user_version` is the index of the
/// last one applied.
const MIGRATIONS: &[&str] = &[r"
CREATE TABLE meta (
    k TEXT PRIMARY KEY,
    v BLOB NOT NULL
);
CREATE TABLE sessions (
    key BLOB PRIMARY KEY,
    name TEXT NOT NULL,
    lifetime TEXT NOT NULL,
    awaiting INTEGER NOT NULL,
    revision INTEGER NOT NULL,
    record TEXT NOT NULL
);
CREATE INDEX sessions_awaiting ON sessions(awaiting) WHERE awaiting = 1;
CREATE TABLE resources (
    key BLOB PRIMARY KEY,
    session BLOB NOT NULL,
    kind TEXT NOT NULL,
    exec_state TEXT,
    revision INTEGER NOT NULL,
    record TEXT NOT NULL
);
CREATE INDEX resources_session ON resources(session);
CREATE INDEX resources_exec_state ON resources(exec_state);
CREATE TABLE arrangements (
    key BLOB PRIMARY KEY,
    owner BLOB NOT NULL,
    revision INTEGER NOT NULL,
    record TEXT NOT NULL
);
CREATE INDEX arrangements_owner ON arrangements(owner);
CREATE TABLE receipts (
    operation BLOB PRIMARY KEY,
    at INTEGER NOT NULL,
    receipt TEXT NOT NULL
);
CREATE INDEX receipts_at ON receipts(at);
CREATE TABLE tombstones (
    key BLOB PRIMARY KEY,
    removed_at INTEGER NOT NULL
);
CREATE INDEX tombstones_removed_at ON tombstones(removed_at);
CREATE TABLE migrations (
    name TEXT PRIMARY KEY,
    at INTEGER NOT NULL
);
"];

/// The schema version this binary writes.
pub const SCHEMA_VERSION: i64 = 1;

/// The durable store: one SQLite file in WAL mode with
/// `synchronous=FULL`.
#[derive(Debug)]
pub struct SqliteOrgStore {
    conn: Connection,
    path: PathBuf,
    /// Rows in `receipts`, kept beside the table so the cap check is not a
    /// scan per commit.
    receipts: u64,
}

impl SqliteOrgStore {
    /// Open (creating if needed) the store at `path`, migrating its schema
    /// forward. An existing file is copied to `<path>.bak-v<N>` before a
    /// migration touches it.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the file cannot be opened or migrated, or its
    /// schema is newer than [`SCHEMA_VERSION`].
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let existed = path.exists();
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::UnsupportedVersion {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        if version < SCHEMA_VERSION {
            if existed && version > 0 {
                let backup = path.with_extension(format!("sqlite3.bak-v{version}"));
                conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
                std::fs::copy(path, backup)?;
            }
            let tx = conn.transaction()?;
            for (index, sql) in MIGRATIONS
                .iter()
                .enumerate()
                .skip(usize::try_from(version).unwrap_or(0))
            {
                tx.execute_batch(sql)?;
                let next = i64::try_from(index + 1).unwrap_or(i64::MAX);
                tx.pragma_update(None, "user_version", next)?;
            }
            tx.commit()?;
        }
        let mut store = Self {
            conn,
            path: path.to_path_buf(),
            receipts: 0,
        };
        store.receipts = store.count("receipts")?;
        Ok(store)
    }

    /// How many rows a table holds, for tests and diagnostics.
    ///
    /// # Errors
    ///
    /// [`StoreError`] on a read failure or an unknown table.
    pub fn count(&self, table: &str) -> Result<u64, StoreError> {
        let sql = match table {
            "sessions" => "SELECT COUNT(*) FROM sessions",
            "resources" => "SELECT COUNT(*) FROM resources",
            "arrangements" => "SELECT COUNT(*) FROM arrangements",
            "receipts" => "SELECT COUNT(*) FROM receipts",
            "tombstones" => "SELECT COUNT(*) FROM tombstones",
            "migrations" => "SELECT COUNT(*) FROM migrations",
            other => return Err(StoreError::Corrupt(format!("unknown table {other}"))),
        };
        let n: i64 = self.conn.query_row(sql, [], |row| row.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    }
}

/// Run one statement through the connection's statement cache, so a
/// commit does not re-parse its SQL.
fn exec(
    tx: &Transaction<'_>,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<usize, StoreError> {
    Ok(tx.prepare_cached(sql)?.execute(params)?)
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn put_meta(tx: &Transaction<'_>, key: &str, value: &[u8]) -> Result<(), StoreError> {
    exec(
        tx,
        "INSERT INTO meta (k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        params![key, value],
    )?;
    Ok(())
}

fn put_authority(tx: &Transaction<'_>, authority: &Authority) -> Result<(), StoreError> {
    put_meta(tx, "authority", &authority.id.to_bytes())?;
    // Retain the legacy storage key; renaming Intake does not migrate identities.
    put_meta(tx, "inbox_session", &authority.intake_session().to_bytes())
}

fn put_session(tx: &Transaction<'_>, s: &Session) -> Result<(), StoreError> {
    exec(
        tx,
        "INSERT INTO sessions (key, name, lifetime, awaiting, revision, record)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(key) DO UPDATE SET name = excluded.name, lifetime = excluded.lifetime,
             awaiting = excluded.awaiting, revision = excluded.revision, record = excluded.record",
        params![
            s.key.as_bytes().as_slice(),
            s.name,
            s.lifetime.as_str(),
            s.awaiting_organization,
            to_i64(s.revision.get()),
            serde_json::to_string(s)?,
        ],
    )?;
    Ok(())
}

fn put_resource(tx: &Transaction<'_>, r: &Resource) -> Result<(), StoreError> {
    exec(
        tx,
        "INSERT INTO resources (key, session, kind, exec_state, revision, record)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(key) DO UPDATE SET session = excluded.session, kind = excluded.kind,
             exec_state = excluded.exec_state, revision = excluded.revision,
             record = excluded.record",
        params![
            r.key.as_bytes().as_slice(),
            r.session.as_bytes().as_slice(),
            r.kind.as_str(),
            r.activation.as_ref().map(|a| a.state.as_str()),
            to_i64(r.revision.get()),
            serde_json::to_string(r)?,
        ],
    )?;
    Ok(())
}

fn put_arrangement(tx: &Transaction<'_>, a: &Arrangement) -> Result<(), StoreError> {
    let owner: [u8; 16] = match a.owner {
        ArrangementOwner::Session(k) => k.to_bytes(),
        ArrangementOwner::Workspace(k) => k.to_bytes(),
    };
    exec(
        tx,
        "INSERT INTO arrangements (key, owner, revision, record) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(key) DO UPDATE SET owner = excluded.owner, revision = excluded.revision,
             record = excluded.record",
        params![
            a.key.as_bytes().as_slice(),
            owner.as_slice(),
            to_i64(a.revision.get()),
            serde_json::to_string(a)?,
        ],
    )?;
    Ok(())
}

fn apply_delta(tx: &Transaction<'_>, delta: &Delta, now: Stamp) -> Result<(), StoreError> {
    match delta {
        Delta::PutSession(s) => put_session(tx, s)?,
        Delta::DeleteSession(k) => {
            exec(
                tx,
                "DELETE FROM sessions WHERE key = ?1",
                [k.as_bytes().as_slice()],
            )?;
        }
        Delta::PutResource(r) => put_resource(tx, r)?,
        Delta::RemoveResource(k) => {
            exec(
                tx,
                "DELETE FROM resources WHERE key = ?1",
                [k.as_bytes().as_slice()],
            )?;
            exec(
                tx,
                "INSERT INTO tombstones (key, removed_at) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET removed_at = excluded.removed_at",
                params![k.as_bytes().as_slice(), to_i64(now.as_millis())],
            )?;
        }
        Delta::PutArrangement(a) => put_arrangement(tx, a)?,
        Delta::DeleteArrangement(k) => {
            exec(
                tx,
                "DELETE FROM arrangements WHERE key = ?1",
                [k.as_bytes().as_slice()],
            )?;
        }
    }
    Ok(())
}

/// Store `receipt`; returns whether it added a row rather than replacing
/// one.
fn put_receipt(tx: &Transaction<'_>, receipt: &Receipt) -> Result<bool, StoreError> {
    let existed = tx
        .prepare_cached("SELECT 1 FROM receipts WHERE operation = ?1")?
        .query_row([receipt.operation.as_bytes().as_slice()], |_| Ok(()))
        .optional()?
        .is_some();
    exec(
        tx,
        "INSERT INTO receipts (operation, at, receipt) VALUES (?1, ?2, ?3)
         ON CONFLICT(operation) DO UPDATE SET at = excluded.at, receipt = excluded.receipt",
        params![
            receipt.operation.as_bytes().as_slice(),
            to_i64(receipt.at.as_millis()),
            serde_json::to_string(receipt)?,
        ],
    )?;
    Ok(!existed)
}

/// Drop receipts past the horizon, then the oldest beyond the cap, and
/// tombstones past their horizon. `receipts` is the row count before;
/// returns the count after. Each delete walks only the rows it drops.
fn prune(tx: &Transaction<'_>, now: Stamp, receipts: u64) -> Result<u64, StoreError> {
    let receipt_floor = now.as_millis().saturating_sub(RECEIPT_HORIZON_MS);
    let expired = exec(
        tx,
        "DELETE FROM receipts WHERE at < ?1",
        [to_i64(receipt_floor)],
    )?;
    let mut receipts = receipts.saturating_sub(u64::try_from(expired).unwrap_or(u64::MAX));
    let excess = receipts.saturating_sub(u64::try_from(RECEIPT_CAP).unwrap_or(u64::MAX));
    if excess > 0 {
        let dropped = exec(
            tx,
            // The bound on `at` keeps the walk on the `receipts_at` index:
            // only the oldest `excess` rows and their ties are sorted.
            "DELETE FROM receipts WHERE operation IN (
                 SELECT operation FROM receipts
                 WHERE at <= (SELECT at FROM receipts ORDER BY at LIMIT 1 OFFSET ?1 - 1)
                 ORDER BY at, operation LIMIT ?1)",
            [to_i64(excess)],
        )?;
        receipts = receipts.saturating_sub(u64::try_from(dropped).unwrap_or(u64::MAX));
    }
    let tombstone_floor = now.as_millis().saturating_sub(TOMBSTONE_HORIZON_MS);
    exec(
        tx,
        "DELETE FROM tombstones WHERE removed_at < ?1",
        [to_i64(tombstone_floor)],
    )?;
    Ok(receipts)
}

fn key16(bytes: &[u8], what: &str) -> Result<[u8; 16], StoreError> {
    bytes
        .try_into()
        .map_err(|_| StoreError::Corrupt(format!("{what} is not 16 bytes")))
}

impl SqliteOrgStore {
    fn meta(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT v FROM meta WHERE k = ?1", [key], |row| row.get(0))
            .optional()?)
    }

    fn records<T: serde::de::DeserializeOwned>(&self, sql: &str) -> Result<Vec<T>, StoreError> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for text in rows {
            out.push(serde_json::from_str(&text?)?);
        }
        Ok(out)
    }
}

impl OrgStore for SqliteOrgStore {
    fn load(&mut self) -> Result<Option<StoredOrg>, StoreError> {
        let (Some(id), Some(inbox)) = (self.meta("authority")?, self.meta("inbox_session")?) else {
            return Ok(None);
        };
        let authority = Authority {
            id: AuthorityId::from_bytes(key16(&id, "authority")?),
            inbox_session: SessionKey::from_bytes(key16(&inbox, "inbox session")?),
        };
        let journal_seq = self
            .meta("journal_seq")?
            .map(|bytes| key8(&bytes))
            .transpose()?
            .unwrap_or(0);
        Ok(Some(StoredOrg {
            authority,
            sessions: self.records("SELECT record FROM sessions")?,
            resources: self.records("SELECT record FROM resources")?,
            arrangements: self.records("SELECT record FROM arrangements")?,
            receipts: self.records("SELECT receipt FROM receipts ORDER BY at")?,
            journal_seq,
        }))
    }

    fn commit(
        &mut self,
        transition: &Transition,
        journal_seq: u64,
        now: Stamp,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        for delta in &transition.deltas {
            apply_delta(&tx, delta, now)?;
        }
        let mut receipts = self.receipts;
        if let Some(receipt) = &transition.receipt
            && put_receipt(&tx, receipt)?
        {
            receipts += 1;
        }
        put_meta(&tx, "journal_seq", &journal_seq.to_be_bytes())?;
        let receipts = prune(&tx, now, receipts)?;
        tx.commit()?;
        self.receipts = receipts;
        Ok(())
    }

    fn import(&mut self, org: &Org, migration: Option<&str>, now: Stamp) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        put_authority(&tx, org.authority())?;
        for s in org.sessions() {
            put_session(&tx, s)?;
        }
        for r in org.resources() {
            put_resource(&tx, r)?;
        }
        for a in org.arrangements() {
            put_arrangement(&tx, a)?;
        }
        let mut receipts = self.receipts;
        for receipt in org.receipts() {
            if put_receipt(&tx, receipt)? {
                receipts += 1;
            }
        }
        if let Some(name) = migration {
            exec(
                &tx,
                "INSERT INTO migrations (name, at) VALUES (?1, ?2) ON CONFLICT(name) DO NOTHING",
                params![name, to_i64(now.as_millis())],
            )?;
        }
        tx.commit()?;
        self.receipts = receipts;
        Ok(())
    }

    fn has_migration(&mut self, name: &str) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM migrations WHERE name = ?1", [name], |_| {
                Ok(())
            })
            .optional()?
            .is_some())
    }

    fn path(&self) -> Option<&Path> {
        Some(&self.path)
    }
}

fn key8(bytes: &[u8]) -> Result<u64, StoreError> {
    let array: [u8; 8] = bytes
        .try_into()
        .map_err(|_| StoreError::Corrupt("journal_seq is not 8 bytes".to_owned()))?;
    Ok(u64::from_be_bytes(array))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use phux_core::{
        Actor, Command, ExitPolicy, InstanceId, LaunchSpec, NewResource, Op, OperationId, Registry,
        ResourceKind, SequentialKeys, SessionLifetime, TerminalLaunch,
    };

    use super::*;

    #[test]
    fn socket_names_escape_to_distinct_file_names() {
        assert_eq!(
            escape_socket(Path::new("/tmp/phux-ci-42.sock")),
            "%2Ftmp%2Fphux-ci-42%2Esock"
        );
        assert_ne!(
            escape_socket(Path::new("/tmp/a_b.sock")),
            escape_socket(Path::new("/tmp/a/b.sock"))
        );
        assert_ne!(
            escape_socket(Path::new("/tmp/a%2Fb.sock")),
            escape_socket(Path::new("/tmp/a/b.sock"))
        );
    }

    #[test]
    fn only_the_profile_socket_uses_the_shared_store() {
        let home = phux_config::instance::runtime_dir().join("phux.sock");
        let other = Path::new("/tmp/phux-private.sock");
        let OrgStoreLocation::Path(home) = OrgStoreLocation::for_socket(&home) else {
            panic!("a socket names a file store");
        };
        let OrgStoreLocation::Path(other) = OrgStoreLocation::for_socket(other) else {
            panic!("a socket names a file store");
        };
        assert!(home.ends_with("org/v1.sqlite3"), "{}", home.display());
        assert_ne!(home, other);
        assert!(other.starts_with(home.parent().unwrap().join("sockets")));
    }

    fn org(keys: &mut SequentialKeys) -> Org {
        use phux_core::FreshKeys as _;
        Org::new(
            AuthorityId::from_bytes(keys.fresh()),
            SessionKey::from_bytes(keys.fresh()),
            InstanceId::from_bytes(keys.fresh()),
            Stamp(1_000),
        )
    }

    fn shell() -> NewResource {
        NewResource {
            kind: ResourceKind::Terminal,
            name: None,
            launch: Some(LaunchSpec::Terminal(TerminalLaunch::default())),
            on_exit: ExitPolicy::Remove,
            parent: None,
            launch_mode: phux_core::LaunchMode::Immediate,
        }
    }

    fn op_id(n: u8) -> OperationId {
        OperationId::from_bytes([n; 16])
    }

    #[test]
    fn intake_defaults_and_persisted_designation_names_keep_their_identity() {
        for name in ["intake", "inbox", "my queue"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("v1.sqlite3");
            let mut keys = SequentialKeys::new(*b"storetst");
            let fresh = org(&mut keys);
            let key = fresh.authority().intake_session();
            assert_eq!(fresh.session(&key).unwrap().name, "intake");
            let mut sessions: Vec<_> = fresh.sessions().cloned().collect();
            sessions[0].name = name.to_owned();
            let saved = Org::restore(
                *fresh.authority(),
                fresh.instance(),
                sessions,
                fresh.resources().cloned(),
                fresh.arrangements().cloned(),
                fresh.receipts().cloned(),
            )
            .unwrap();
            let mut store = SqliteOrgStore::open(&path).unwrap();
            store.import(&saved, None, Stamp(1_000)).unwrap();
            drop(store);
            let stored = SqliteOrgStore::open(&path)
                .unwrap()
                .load()
                .unwrap()
                .unwrap();
            assert_eq!(stored.authority.intake_session(), key);
            assert_eq!(stored.sessions[0].key, key);
            assert_eq!(stored.sessions[0].name, name);
            assert_eq!(
                stored.sessions[0].revision,
                saved.session(&key).unwrap().revision
            );
        }
    }

    #[test]
    fn a_fresh_file_loads_as_nothing_and_round_trips_commits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("org").join("v1.sqlite3");
        let mut keys = SequentialKeys::new(*b"storetst");
        let mut store = SqliteOrgStore::open(&path).unwrap();
        assert!(store.load().unwrap().is_none());

        let mut registry = Registry::new(org(&mut keys));
        store
            .import(registry.org(), Some(LIVE_IMPORT_MIGRATION), Stamp(1_000))
            .unwrap();
        let cmd = Command {
            operation: op_id(1),
            op: Op::SessionCreate {
                name: "work".to_owned(),
                lifetime: SessionLifetime::Persistent,
                attrs: phux_core::Attrs::default(),
                resource: Some(shell()),
            },
        };
        let applied = phux_core::apply(
            registry.org(),
            &cmd,
            &Actor::owner(),
            Stamp(2_000),
            &mut keys,
        )
        .unwrap();
        store.commit(&applied.transition, 7, Stamp(2_000)).unwrap();
        let _ = registry.commit(&applied.transition);
        drop(store);

        let mut reopened = SqliteOrgStore::open(&path).unwrap();
        assert!(reopened.has_migration(LIVE_IMPORT_MIGRATION).unwrap());
        let stored = reopened.load().unwrap().expect("written");
        assert_eq!(stored.journal_seq, 7);
        let back = Org::restore(
            stored.authority,
            registry.org().instance(),
            stored.sessions,
            stored.resources,
            stored.arrangements,
            stored.receipts,
        )
        .unwrap();
        assert_eq!(&back, registry.org());
    }

    #[test]
    fn a_removed_resource_leaves_a_tombstone_and_receipts_prune() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v1.sqlite3");
        let mut keys = SequentialKeys::new(*b"storetst");
        let mut store = SqliteOrgStore::open(&path).unwrap();
        let mut registry = Registry::new(org(&mut keys));
        store.import(registry.org(), None, Stamp(0)).unwrap();
        let create = Command {
            operation: op_id(1),
            op: Op::ResourceCreate {
                session: None,
                resource: shell(),
            },
        };
        let applied = phux_core::apply(
            registry.org(),
            &create,
            &Actor::owner(),
            Stamp(0),
            &mut keys,
        )
        .unwrap();
        store.commit(&applied.transition, 1, Stamp(0)).unwrap();
        let _ = registry.commit(&applied.transition);
        let resource = registry.org().resources().next().unwrap().key;
        let end = Command {
            operation: op_id(2),
            op: Op::ActivationEnded {
                resource,
                activation: phux_core::ActivationId::FIRST,
                state: phux_core::ExecState::Completed,
                outcome: phux_core::Outcome::Exit(phux_core::ExitOutcome::default()),
            },
        };
        let applied =
            phux_core::apply(registry.org(), &end, &Actor::system(), Stamp(5), &mut keys).unwrap();
        store.commit(&applied.transition, 2, Stamp(5)).unwrap();
        assert_eq!(store.count("resources").unwrap(), 0);
        assert_eq!(store.count("tombstones").unwrap(), 1);
        assert_eq!(store.count("receipts").unwrap(), 1);

        // A day later the receipt is gone; a month later the tombstone too.
        let later = Stamp(RECEIPT_HORIZON_MS + 10);
        store.commit(&Transition::default(), 3, later).unwrap();
        assert_eq!(store.count("receipts").unwrap(), 0);
        assert_eq!(store.count("tombstones").unwrap(), 1);
        store
            .commit(&Transition::default(), 4, Stamp(TOMBSTONE_HORIZON_MS + 10))
            .unwrap();
        assert_eq!(store.count("tombstones").unwrap(), 0);
    }

    #[test]
    fn the_cap_drops_the_receipts_memory_drops() {
        let dir = tempfile::tempdir().unwrap();
        let mut keys = SequentialKeys::new(*b"storetst");
        let mut registry = Registry::new(org(&mut keys));
        let create = Command {
            operation: op_id(1),
            op: Op::ResourceCreate {
                session: None,
                resource: shell(),
            },
        };
        registry
            .execute(&create, &Actor::owner(), Stamp(5), &mut keys)
            .unwrap();
        let template = registry.org().receipts().next().cloned().unwrap();
        // Every receipt shares one stamp, so only the operation id orders
        // them; ids descend so insertion order is not the answer.
        let receipts = (0..RECEIPT_CAP + 2).rev().map(|n| {
            let mut bytes = [0xee; 16];
            bytes[8..].copy_from_slice(&u64::try_from(n).unwrap().to_be_bytes());
            Receipt {
                operation: OperationId::from_bytes(bytes),
                ..template.clone()
            }
        });
        let o = registry.org();
        let mut org = Org::restore(
            *o.authority(),
            o.instance(),
            o.sessions().cloned(),
            o.resources().cloned(),
            o.arrangements().cloned(),
            receipts,
        )
        .unwrap();
        let mut store = SqliteOrgStore::open(&dir.path().join("v1.sqlite3")).unwrap();
        store.import(&org, None, Stamp(5)).unwrap();
        store.commit(&Transition::default(), 1, Stamp(5)).unwrap();
        let dropped = org.prune_receipts(Stamp(0), RECEIPT_CAP);
        assert_eq!(dropped.len(), 2);
        let kept: Vec<OperationId> = store
            .load()
            .unwrap()
            .unwrap()
            .receipts
            .iter()
            .map(|r| r.operation)
            .collect();
        assert_eq!(kept.len(), RECEIPT_CAP);
        assert!(dropped.iter().all(|op| !kept.contains(op)));
        let mut mirrored: Vec<OperationId> = org.receipts().map(|r| r.operation).collect();
        let mut kept = kept;
        mirrored.sort_unstable();
        kept.sort_unstable();
        assert_eq!(kept, mirrored);
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v1.sqlite3");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                .unwrap();
        }
        let err = SqliteOrgStore::open(&path).unwrap_err();
        assert!(
            matches!(err, StoreError::UnsupportedVersion { .. }),
            "{err}"
        );
    }

    #[test]
    fn the_memory_store_fails_on_request() {
        let mut store = MemOrgStore::default();
        store.commit(&Transition::default(), 0, Stamp(0)).unwrap();
        store.fail_commits().store(true, Ordering::Relaxed);
        assert!(matches!(
            store.commit(&Transition::default(), 0, Stamp(0)),
            Err(StoreError::Injected)
        ));
    }
}
