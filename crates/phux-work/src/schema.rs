//! Schema v2. `PRAGMA user_version` is 2 after a successful open.
//!
//! Version 2 is a marker for the objective states `done_unchecked`, `done`,
//! and `failed`. It adds no tables. A binary that only knows version 1
//! refuses the file at open with [`Error::UnsupportedSchema`] instead of
//! reading one of those rows as corrupt. Additive tables (mail, claims,
//! launch intent) still do not bump `user_version`.

use rusqlite::{Connection, Transaction};

use crate::error::Error;

/// The newest `user_version` this binary reads or writes.
pub(crate) const USER_VERSION: i64 = 2;

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

/// Open `conn` for a binary whose newest readable version is `supported`.
///
/// Production passes [`USER_VERSION`]. A test passes `1` to simulate the
/// previous binary, which refuses a version-2 file at open.
pub(crate) fn prepare_supported(conn: &Connection, supported: i64) -> Result<(), Error> {
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
            set_user_version(&tx, supported)?;
            tx.commit()?;
        }
        // Marker only: the new objective states add no tables. The stamp
        // makes a version-1 binary refuse the file at open.
        1 if supported >= USER_VERSION => {
            let tx = conn.unchecked_transaction()?;
            set_user_version(&tx, USER_VERSION)?;
            tx.commit()?;
        }
        found if found == supported => {}
        found => {
            return Err(Error::UnsupportedSchema { found, supported });
        }
    }
    // Additive tables leave `user_version` alone, so a binary that predates
    // them still opens the store and ignores them.
    conn.execute_batch(crate::coord::SCHEMA)?;
    conn.execute_batch(LAUNCH_INTENT)?;
    Ok(())
}

/// Install the pre-marker schema and stamp `user_version` 1.
///
/// # Errors
///
/// [`Error::Sqlite`] when the statements cannot be applied.
#[cfg(test)]
pub(crate) fn install_version_one(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(SCHEMA)?;
    conn.execute_batch(crate::coord::SCHEMA)?;
    conn.execute_batch(LAUNCH_INTENT)?;
    conn.pragma_update(None, "user_version", 1)?;
    Ok(())
}

/// Facts a queued run needs when a slot frees. Additive: no `user_version` bump.
const LAUNCH_INTENT: &str = "
CREATE TABLE IF NOT EXISTS launch_intent (
    run_id BLOB NOT NULL PRIMARY KEY REFERENCES run(id),
    model TEXT NOT NULL,
    max_agents INTEGER NOT NULL CHECK (max_agents >= 1),
    worktree_path TEXT NOT NULL,
    branch TEXT NOT NULL,
    repo_root TEXT NOT NULL
);
";

pub(crate) fn user_version(conn: &Connection) -> Result<i64, Error> {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(Error::from)
}

fn set_user_version(tx: &Transaction<'_>, version: i64) -> Result<(), Error> {
    tx.pragma_update(None, "user_version", version)?;
    Ok(())
}
