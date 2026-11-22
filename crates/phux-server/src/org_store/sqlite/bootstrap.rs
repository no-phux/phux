//! Open and migrate the organization database before an owner loads records.

use std::path::Path;

use rusqlite::Connection;

use super::StoreError;

/// The schema version this binary writes.
pub const SCHEMA_VERSION: i64 = 1;

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

pub(super) fn open(path: &Path) -> Result<Connection, StoreError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let existed = path.exists();
    let mut conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    migrate(&mut conn, path, existed)?;
    Ok(conn)
}

fn migrate(conn: &mut Connection, path: &Path, existed: bool) -> Result<(), StoreError> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(StoreError::UnsupportedVersion {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    backup(conn, path, existed, version)?;
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
    Ok(())
}

fn backup(conn: &Connection, path: &Path, existed: bool, version: i64) -> Result<(), StoreError> {
    if existed && version > 0 {
        let backup = path.with_extension(format!("sqlite3.bak-v{version}"));
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        std::fs::copy(path, backup)?;
    }
    Ok(())
}
