//! [`SqliteOrgStore`]: the durable [`OrgStore`], one SQLite file.

use std::path::{Path, PathBuf};

use phux_core::{
    Arrangement, ArrangementOwner, Authority, AuthorityId, Delta, Key16 as _, Org, Receipt,
    Resource, Session, SessionKey, Stamp, Transition,
};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::{
    OrgStore, RECEIPT_CAP, RECEIPT_HORIZON_MS, StoreError, StoredOrg, TOMBSTONE_HORIZON_MS,
};

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
            // Fixed-depth fields: even a large member vector adds no JSON nesting.
            serde_json::to_string(s)?,
        ],
    )?;
    Ok(())
}

/// Check potentially recursive row JSON with the exact `load()` decoder.
///
/// Fixed-depth session/resource records need no repeated key deserialization.
/// Keep this check for arrangements and receipt payloads that can contain trees.
fn stored_json<T: serde::Serialize + serde::de::DeserializeOwned>(
    value: &T,
) -> Result<String, StoreError> {
    let encoded = serde_json::to_string(value)?;
    let _: T = serde_json::from_str(&encoded)?;
    Ok(encoded)
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
            // Launch restore data is opaque bytes, not a recursive JSON value.
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
            stored_json(a)?,
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

/// Membership payloads contain only keys/revisions, unless their inverse
/// restores a pruned layout. Other operation kinds keep the checked path.
fn receipt_json(receipt: &Receipt) -> Result<String, StoreError> {
    let membership = matches!(
        &receipt.op,
        phux_core::Op::ResourceMove { .. } | phux_core::Op::ResourceMoveMany { .. }
    );
    let layout_undo = receipt.undo.as_ref().is_some_and(|undo| {
        undo.inverse
            .iter()
            .any(|edit| matches!(edit, phux_core::InverseEdit::Arrangement { .. }))
    });
    if membership && !layout_undo {
        Ok(serde_json::to_string(receipt)?)
    } else {
        stored_json(receipt)
    }
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
            receipt_json(receipt)?,
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
