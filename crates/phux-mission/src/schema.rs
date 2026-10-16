//! Schema v1. `PRAGMA user_version` is 1 after a successful open.

use rusqlite::{Connection, Transaction};

use crate::error::Error;

/// The only `user_version` this binary reads or writes.
pub(crate) const USER_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS objective (
    id BLOB NOT NULL PRIMARY KEY CHECK (length(id) = 16),
    repo TEXT NOT NULL,
    alias_number INTEGER NOT NULL CHECK (alias_number > 0),
    display_alias TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    state TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    archived INTEGER NOT NULL CHECK (archived IN (0, 1)),
    UNIQUE (repo, alias_number)
);
CREATE TABLE IF NOT EXISTS alias_counter (
    repo TEXT NOT NULL PRIMARY KEY,
    next_number INTEGER NOT NULL CHECK (next_number >= 1)
);
CREATE TABLE IF NOT EXISTS run (
    id BLOB NOT NULL PRIMARY KEY CHECK (length(id) = 16),
    objective_id BLOB NOT NULL REFERENCES objective(id),
    ordinal INTEGER NOT NULL CHECK (ordinal > 0),
    agent_kind TEXT NOT NULL,
    state TEXT NOT NULL,
    started_at TEXT,
    ended_at TEXT,
    UNIQUE (objective_id, ordinal)
);
CREATE TABLE IF NOT EXISTS binding (
    id BLOB NOT NULL PRIMARY KEY CHECK (length(id) = 16),
    run_id BLOB NOT NULL REFERENCES run(id),
    server_id BLOB,
    session_key TEXT,
    terminal_id TEXT,
    worktree_path TEXT,
    branch TEXT
);
CREATE TABLE IF NOT EXISTS signal (
    id BLOB NOT NULL PRIMARY KEY CHECK (length(id) = 16),
    objective_id BLOB NOT NULL REFERENCES objective(id),
    kind TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at TEXT NOT NULL,
    resolved_at TEXT
);
CREATE INDEX IF NOT EXISTS signal_objective_kind ON signal (objective_id, kind);
CREATE TABLE IF NOT EXISTS operation (
    id BLOB NOT NULL PRIMARY KEY CHECK (length(id) = 16),
    kind TEXT NOT NULL,
    digest TEXT NOT NULL,
    objective_id BLOB,
    result_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS event (
    seq INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    ts TEXT NOT NULL,
    actor TEXT NOT NULL,
    objective_id BLOB,
    operation_id BLOB NOT NULL,
    kind TEXT NOT NULL,
    json TEXT NOT NULL
);
";

pub(crate) fn prepare(conn: &Connection) -> Result<(), Error> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    // WAL so a reader does not block the writer. FULL makes the commit
    // durable before the call returns. `journal_mode` reports what was set.
    let mode: String =
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(Error::Corrupt(format!("journal_mode is {mode}")));
    }
    conn.pragma_update(None, "synchronous", "FULL")?;
    let version = user_version(conn)?;
    match version {
        0 => {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(SCHEMA)?;
            set_user_version(&tx, USER_VERSION)?;
            tx.commit()?;
        }
        USER_VERSION => {}
        found => {
            return Err(Error::UnsupportedSchema {
                found,
                supported: USER_VERSION,
            });
        }
    }
    Ok(())
}

pub(crate) fn user_version(conn: &Connection) -> Result<i64, Error> {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(Error::from)
}

fn set_user_version(tx: &Transaction<'_>, version: i64) -> Result<(), Error> {
    tx.pragma_update(None, "user_version", version)?;
    Ok(())
}
