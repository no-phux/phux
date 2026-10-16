//! The SQLite store: one writer, transactional mutations, replay by operation id.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::error::Error;
use crate::id::{
    Alias, ObjectiveState, OpaqueId, OperationId, SignalKind, parse_actor, parse_body,
    parse_description, parse_repo, parse_title,
};
use crate::lock::{self, FileLock};
use crate::schema;

/// File name under `work/`.
pub const DB_FILE: &str = "work-v1.sqlite";

/// `<state_dir>/work/work-v1.sqlite`.
#[must_use]
pub fn store_path(state_dir: &Path) -> PathBuf {
    state_dir.join("work").join(DB_FILE)
}

/// One objective row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Objective {
    /// Opaque id.
    pub id: OpaqueId,
    /// Display alias.
    pub alias: Alias,
    /// Repo the alias belongs to.
    pub repo: String,
    /// Title.
    pub title: String,
    /// Description. Empty when unset.
    pub description: String,
    /// Projected lifecycle.
    pub state: ObjectiveState,
    /// Structural revision. Starts at 1.
    pub revision: u64,
    /// Archived tasks stay in the store and drop out of the active list.
    pub archived: bool,
    /// RFC 3339 creation time.
    pub created_at: String,
    /// RFC 3339 time of the last structural change.
    pub updated_at: String,
}

/// A signal row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Signal {
    /// Opaque id.
    pub id: OpaqueId,
    /// Kind.
    pub kind: SignalKind,
    /// Free text.
    pub body: String,
    /// RFC 3339 creation time.
    pub created_at: String,
    /// Set once the signal is resolved. `None` while it is open.
    pub resolved_at: Option<String>,
}

/// Result of an admitted or replayed mutation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Applied {
    /// Objective after the write, or the snapshot stored for this operation.
    pub objective: Objective,
    /// The operation id the caller sent.
    pub operation_id: OperationId,
    /// True when the id was already on disk and no row changed.
    pub idempotent_replay: bool,
}

/// `add` arguments.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AddObjective {
    /// Repo id the new alias is allocated under.
    pub repo: String,
    /// Title.
    pub title: String,
    /// Description. Empty is allowed.
    pub description: String,
    /// Idempotency key.
    pub operation_id: OperationId,
    /// Recorded on the event row.
    pub actor: String,
}

/// `edit` arguments. At least one of title or description is required.
/// `expected_revision` is required: there is no last-write-wins fallback.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EditObjective {
    /// Task to edit.
    pub alias: Alias,
    /// New title, when set.
    pub title: Option<String>,
    /// New description, when set.
    pub description: Option<String>,
    /// Fail when the stored revision is not this value. Required.
    pub expected_revision: u64,
    /// Idempotency key.
    pub operation_id: OperationId,
    /// Recorded on the event row.
    pub actor: String,
}

/// `archive` and `restore` arguments.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RevisionEdit {
    /// Task to edit.
    pub alias: Alias,
    /// Fail when the stored revision is not this value. Required.
    pub expected_revision: u64,
    /// Idempotency key.
    pub operation_id: OperationId,
    /// Recorded on the event row.
    pub actor: String,
}

/// Direct state write. An unresolved `credential_lost` signal overrides
/// any requested state with `needs_you`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StateEdit {
    /// Task to edit.
    pub alias: Alias,
    /// Requested state, before projection.
    pub state: ObjectiveState,
    /// Fail when the stored revision is not this value. Required.
    pub expected_revision: u64,
    /// Idempotency key.
    pub operation_id: OperationId,
    /// Recorded on the event row.
    pub actor: String,
}

/// Record a signal. `credential_lost` projects the objective to `needs_you`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RaiseSignal {
    /// Task the signal belongs to.
    pub alias: Alias,
    /// Kind.
    pub kind: SignalKind,
    /// Free text.
    pub body: String,
    /// Fail when the stored revision is not this value. Required.
    pub expected_revision: u64,
    /// Idempotency key.
    pub operation_id: OperationId,
    /// Recorded on the event row.
    pub actor: String,
}

/// Which objectives [`Store::list`] returns.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ListFilter {
    /// Restrict to one repo. `None` lists every repo.
    pub repo: Option<String>,
    /// `false` lists active tasks. `true` lists archived tasks.
    pub archived: bool,
}

/// Open SQLite store. The lock is held until drop.
///
/// The keeper ([`crate::run_keeper`]) is the only process that opens a host's
/// store; everything else goes through [`crate::Client`].
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    /// `None` when the caller already holds the lock, as the keeper does.
    _lock: Option<FileLock>,
}

impl Store {
    /// Open `path`, creating the schema when the file is new.
    ///
    /// Blocks while another process holds the writer lock.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedSchema`] when `user_version` is not 0 or 1,
    /// and [`Error::Io`] or [`Error::Sqlite`] when the file cannot be opened.
    pub fn open(path: &Path) -> Result<Self, Error> {
        Self::open_locking(path, true)
    }

    /// Like [`Store::open`], but returns [`Error::Busy`] immediately when
    /// another writer holds the lock.
    ///
    /// # Errors
    ///
    /// [`Error::Busy`] when the lock is held, plus the errors of [`Store::open`].
    pub fn try_open(path: &Path) -> Result<Self, Error> {
        Self::open_locking(path, false)
    }

    /// Insert an objective at revision 1 in state `todo` and allocate `repo/n`.
    ///
    /// # Errors
    ///
    /// Validation, lock, and store errors. A replayed operation id returns
    /// the original objective.
    pub fn add(&mut self, request: AddObjective) -> Result<Applied, Error> {
        let AddObjective {
            repo,
            title,
            description,
            operation_id,
            actor,
        } = request;
        let repo = parse_repo(&repo)?;
        let title = parse_title(&title)?;
        let description = parse_description(&description)?;
        let actor = parse_actor(&actor)?;
        let digest = digest(
            "add",
            &serde_json::json!({
                "description": &description,
                "repo": &repo,
                "title": &title,
            }),
        )?;
        self.mutate("add", &digest, operation_id, &actor, |tx, now| {
            let id = OpaqueId::generate()?;
            let alias = allocate_alias(tx, &repo)?;
            insert_objective(tx, &id, &alias, &title, &description, now)?;
            Ok(MutationBody {
                objective: load_alias(tx, &alias)?,
                event_kind: "objective_created",
            })
        })
    }

    /// Active or archived objectives, ordered by repo then alias number.
    ///
    /// # Errors
    ///
    /// Store errors, and [`Error::InvalidRepo`] when the filter repo is unusable.
    pub fn list(&self, filter: &ListFilter) -> Result<Vec<Objective>, Error> {
        if let Some(repo) = &filter.repo {
            parse_repo(repo)?;
        }
        let archived: i64 = i64::from(filter.archived);
        let repo = filter.repo.clone().unwrap_or_default();
        let mut statement = self.conn.prepare(
            "SELECT id, repo, alias_number, display_alias, title, description,
                    state, revision, created_at, updated_at, archived
             FROM objective
             WHERE archived = ?1 AND (?2 = '' OR repo = ?2)
             ORDER BY repo, alias_number",
        )?;
        let rows = statement.query_map(params![archived, repo], map_objective)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    }

    /// Read one objective by alias.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] when the alias is absent.
    pub fn show(&self, alias: &Alias) -> Result<Objective, Error> {
        load_alias_conn(&self.conn, alias)
    }

    /// Replace the title, the description, or both.
    ///
    /// # Errors
    ///
    /// [`Error::EmptyEdit`] when neither field is set, plus revision and
    /// operation conflicts.
    pub fn edit(&mut self, request: EditObjective) -> Result<Applied, Error> {
        if request.title.is_none() && request.description.is_none() {
            return Err(Error::EmptyEdit);
        }
        let title = request.title.map(|title| parse_title(&title)).transpose()?;
        let description = request
            .description
            .map(|description| parse_description(&description))
            .transpose()?;
        let actor = parse_actor(&request.actor)?;
        let digest = digest(
            "edit",
            &serde_json::json!({
                "alias": request.alias.display(),
                "description": &description,
                "expected_revision": request.expected_revision,
                "title": &title,
            }),
        )?;
        let alias = request.alias;
        let expected = request.expected_revision;
        self.mutate("edit", &digest, request.operation_id, &actor, |tx, now| {
            let current = load_checked(tx, &alias, expected)?;
            let next_title = title.unwrap_or_else(|| current.title.clone());
            let next_description = description.unwrap_or_else(|| current.description.clone());
            let changed = next_title != current.title || next_description != current.description;
            let objective = if changed {
                let previous = current.revision;
                let mut updated = current;
                updated.title = next_title;
                updated.description = next_description;
                updated.revision = bump(previous)?;
                now.clone_into(&mut updated.updated_at);
                write_objective(tx, &updated, previous)?;
                updated
            } else {
                current
            };
            Ok(MutationBody {
                objective,
                event_kind: "objective_updated",
            })
        })
    }

    /// Mark the objective archived.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`], [`Error::RevisionConflict`], and operation conflicts.
    pub fn archive(&mut self, request: RevisionEdit) -> Result<Applied, Error> {
        self.set_archived(request, true, "archive", "objective_archived")
    }

    /// Clear the archived flag.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`], [`Error::RevisionConflict`], and operation conflicts.
    pub fn restore(&mut self, request: RevisionEdit) -> Result<Applied, Error> {
        self.set_archived(request, false, "restore", "objective_restored")
    }

    /// Write a lifecycle state. An unresolved `credential_lost` signal
    /// stores `needs_you` instead of the requested state, including `working`.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`], [`Error::RevisionConflict`], and operation conflicts.
    pub fn record_state(&mut self, request: StateEdit) -> Result<Applied, Error> {
        let actor = parse_actor(&request.actor)?;
        let digest = digest(
            "state",
            &serde_json::json!({
                "alias": request.alias.display(),
                "expected_revision": request.expected_revision,
                "state": request.state.as_str(),
            }),
        )?;
        let alias = request.alias;
        let expected = request.expected_revision;
        let requested = request.state;
        self.mutate("state", &digest, request.operation_id, &actor, |tx, now| {
            let current = load_checked(tx, &alias, expected)?;
            let projected = project_state(tx, current.id, requested)?;
            let objective = if projected == current.state {
                current
            } else {
                let previous = current.revision;
                let mut updated = current;
                updated.state = projected;
                updated.revision = bump(previous)?;
                now.clone_into(&mut updated.updated_at);
                write_objective(tx, &updated, previous)?;
                updated
            };
            Ok(MutationBody {
                objective,
                event_kind: "objective_state",
            })
        })
    }

    /// Append an unresolved signal and project state.
    ///
    /// `credential_lost` sets the objective to `needs_you`. The row is not
    /// left `working`.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`], [`Error::RevisionConflict`], and operation conflicts.
    pub fn raise_signal(&mut self, request: RaiseSignal) -> Result<Applied, Error> {
        let body = parse_body(&request.body)?;
        let actor = parse_actor(&request.actor)?;
        let digest = digest(
            "signal",
            &serde_json::json!({
                "alias": request.alias.display(),
                "body": &body,
                "expected_revision": request.expected_revision,
                "kind": request.kind.as_str(),
            }),
        )?;
        let alias = request.alias;
        let expected = request.expected_revision;
        let kind = request.kind;
        self.mutate(
            "signal",
            &digest,
            request.operation_id,
            &actor,
            |tx, now| {
                let current = load_checked(tx, &alias, expected)?;
                insert_signal(tx, current.id, kind, &body, now)?;
                let projected = project_state(tx, current.id, current.state)?;
                let previous = current.revision;
                let mut updated = current;
                updated.state = projected;
                updated.revision = bump(previous)?;
                now.clone_into(&mut updated.updated_at);
                write_objective(tx, &updated, previous)?;
                Ok(MutationBody {
                    objective: updated,
                    event_kind: "signal_raised",
                })
            },
        )
    }

    /// Signals recorded for `alias`, oldest first.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] when the alias is absent.
    pub fn signals(&self, alias: &Alias) -> Result<Vec<Signal>, Error> {
        let objective = load_alias_conn(&self.conn, alias)?;
        let mut statement = self.conn.prepare(
            "SELECT id, kind, body, created_at, resolved_at
             FROM signal WHERE objective_id = ?1 ORDER BY rowid",
        )?;
        let rows = statement.query_map(params![objective.id.as_slice()], map_signal)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    }

    #[cfg(test)]
    pub(crate) fn user_version(&self) -> Result<i64, Error> {
        schema::user_version(&self.conn)
    }

    #[cfg(test)]
    pub(crate) fn durability(&self) -> Result<(String, i64), Error> {
        let journal: String = self
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        let synchronous: i64 = self
            .conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))?;
        Ok((journal, synchronous))
    }

    #[cfg(test)]
    pub(crate) fn table_names(&self) -> Result<Vec<String>, Error> {
        let mut statement = self.conn.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    }

    #[cfg(test)]
    pub(crate) fn event_kinds(&self) -> Result<Vec<String>, Error> {
        let mut statement = self.conn.prepare("SELECT kind FROM event ORDER BY seq")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    }

    fn set_archived(
        &mut self,
        request: RevisionEdit,
        archived: bool,
        kind: &'static str,
        event_kind: &'static str,
    ) -> Result<Applied, Error> {
        let actor = parse_actor(&request.actor)?;
        let digest = digest(
            kind,
            &serde_json::json!({
                "alias": request.alias.display(),
                "expected_revision": request.expected_revision,
            }),
        )?;
        let alias = request.alias;
        let expected = request.expected_revision;
        self.mutate(kind, &digest, request.operation_id, &actor, |tx, now| {
            let current = load_checked(tx, &alias, expected)?;
            let objective = if current.archived == archived {
                current
            } else {
                let previous = current.revision;
                let mut updated = current;
                updated.archived = archived;
                updated.revision = bump(previous)?;
                now.clone_into(&mut updated.updated_at);
                write_objective(tx, &updated, previous)?;
                updated
            };
            Ok(MutationBody {
                objective,
                event_kind,
            })
        })
    }

    fn open_locking(path: &Path, block: bool) -> Result<Self, Error> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
            lock::set_private_dir(parent)?;
        }
        let lock = lock::acquire(&lock_path(path), block)?;
        let Self { conn, .. } = Self::open_held(path)?;
        Ok(Self {
            conn,
            _lock: Some(lock),
        })
    }

    /// Open `path` while the caller holds the lock at [`lock_path`].
    pub(crate) fn open_held(path: &Path) -> Result<Self, Error> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
            lock::set_private_dir(parent)?;
        }
        let _file = lock::create_private(path)?;
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        schema::prepare(&conn)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self { conn, _lock: None })
    }

    fn mutate(
        &mut self,
        kind: &str,
        digest: &str,
        operation_id: OperationId,
        actor: &str,
        apply: impl FnOnce(&Transaction<'_>, &str) -> Result<MutationBody, Error>,
    ) -> Result<Applied, Error> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(objective) = replay(&tx, &operation_id, digest)? {
            return Ok(Applied {
                objective,
                operation_id,
                idempotent_replay: true,
            });
        }
        let now = timestamp();
        let body = apply(&tx, &now)?;
        let result_json = serde_json::to_string(&body.objective)?;
        tx.execute(
            "INSERT INTO operation (id, kind, digest, objective_id, result_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                operation_id.as_slice(),
                kind,
                digest,
                body.objective.id.as_slice(),
                &result_json,
                now,
            ],
        )?;
        tx.execute(
            "INSERT INTO event (ts, actor, objective_id, operation_id, kind, json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                now,
                actor,
                body.objective.id.as_slice(),
                operation_id.as_slice(),
                body.event_kind,
                &result_json,
            ],
        )?;
        tx.commit()?;
        Ok(Applied {
            objective: body.objective,
            operation_id,
            idempotent_replay: false,
        })
    }
}

struct MutationBody {
    objective: Objective,
    event_kind: &'static str,
}

fn bump(revision: u64) -> Result<u64, Error> {
    revision
        .checked_add(1)
        .ok_or_else(|| Error::Corrupt("revision wrapped".to_owned()))
}

pub(crate) fn lock_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push(".lock");
    db.with_file_name(name)
}

fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn digest(kind: &str, body: &serde_json::Value) -> Result<String, Error> {
    Ok(serde_json::to_string(&serde_json::json!({
        "body": body,
        "kind": kind,
    }))?)
}

fn replay(
    tx: &Transaction<'_>,
    operation_id: &OperationId,
    digest: &str,
) -> Result<Option<Objective>, Error> {
    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT digest, result_json FROM operation WHERE id = ?1",
            params![operation_id.as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match row {
        None => Ok(None),
        Some((stored, result)) if stored == digest => Ok(Some(serde_json::from_str(&result)?)),
        Some(_) => Err(Error::OperationConflict {
            operation_id: operation_id.to_hex(),
        }),
    }
}

fn allocate_alias(tx: &Transaction<'_>, repo: &str) -> Result<Alias, Error> {
    let number: i64 = tx.query_row(
        "INSERT INTO alias_counter (repo, next_number) VALUES (?1, 1)
         ON CONFLICT(repo) DO UPDATE SET next_number = next_number + 1
         RETURNING next_number",
        params![repo],
        |row| row.get(0),
    )?;
    let number = u64::try_from(number).map_err(|_| Error::Corrupt("alias counter".to_owned()))?;
    Ok(Alias {
        repo: repo.to_owned(),
        number,
    })
}

fn insert_objective(
    tx: &Transaction<'_>,
    id: &OpaqueId,
    alias: &Alias,
    title: &str,
    description: &str,
    now: &str,
) -> Result<(), Error> {
    let number =
        i64::try_from(alias.number).map_err(|_| Error::Corrupt("alias number".to_owned()))?;
    tx.execute(
        "INSERT INTO objective (
            id, repo, alias_number, display_alias, title, description,
            state, revision, created_at, updated_at, archived
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?8, 0)",
        params![
            id.as_slice(),
            alias.repo,
            number,
            alias.display(),
            title,
            description,
            ObjectiveState::Todo.as_str(),
            now,
        ],
    )?;
    Ok(())
}

fn insert_signal(
    tx: &Transaction<'_>,
    objective_id: OpaqueId,
    kind: SignalKind,
    body: &str,
    now: &str,
) -> Result<(), Error> {
    let id = OpaqueId::generate()?;
    tx.execute(
        "INSERT INTO signal (id, objective_id, kind, body, created_at, resolved_at)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
        params![
            id.as_slice(),
            objective_id.as_slice(),
            kind.as_str(),
            body,
            now
        ],
    )?;
    Ok(())
}

fn project_state(
    tx: &Transaction<'_>,
    objective_id: OpaqueId,
    requested: ObjectiveState,
) -> Result<ObjectiveState, Error> {
    if unresolved_credential_lost(tx, objective_id)? {
        Ok(ObjectiveState::NeedsYou)
    } else {
        Ok(requested)
    }
}

fn unresolved_credential_lost(tx: &Transaction<'_>, objective_id: OpaqueId) -> Result<bool, Error> {
    let found: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM signal
             WHERE objective_id = ?1 AND kind = 'credential_lost' AND resolved_at IS NULL
             LIMIT 1",
            params![objective_id.as_slice()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

fn load_checked(tx: &Transaction<'_>, alias: &Alias, expected: u64) -> Result<Objective, Error> {
    let objective = load_alias(tx, alias)?;
    if expected != objective.revision {
        return Err(Error::RevisionConflict {
            alias: alias.display(),
            expected,
            actual: objective.revision,
        });
    }
    Ok(objective)
}

fn write_objective(
    tx: &Transaction<'_>,
    objective: &Objective,
    previous: u64,
) -> Result<(), Error> {
    let revision =
        i64::try_from(objective.revision).map_err(|_| Error::Corrupt("revision".into()))?;
    let previous = i64::try_from(previous).map_err(|_| Error::Corrupt("revision".into()))?;
    let archived = i64::from(objective.archived);
    let changed = tx.execute(
        "UPDATE objective
         SET title = ?1, description = ?2, state = ?3, archived = ?4, revision = ?5, updated_at = ?6
         WHERE id = ?7 AND revision = ?8",
        params![
            objective.title,
            objective.description,
            objective.state.as_str(),
            archived,
            revision,
            objective.updated_at,
            objective.id.as_slice(),
            previous,
        ],
    )?;
    if changed == 0 {
        let actual = load_by_id(tx, objective.id)?.revision;
        let expected =
            u64::try_from(previous).map_err(|_| Error::Corrupt("revision".to_owned()))?;
        return Err(Error::RevisionConflict {
            alias: objective.alias.display(),
            expected,
            actual,
        });
    }
    Ok(())
}

fn load_alias(tx: &Transaction<'_>, alias: &Alias) -> Result<Objective, Error> {
    load_display(tx, &alias.display())
}

fn load_alias_conn(conn: &Connection, alias: &Alias) -> Result<Objective, Error> {
    load_display(conn, &alias.display())
}

fn load_display(conn: &Connection, display: &str) -> Result<Objective, Error> {
    conn.query_row(
        "SELECT id, repo, alias_number, display_alias, title, description,
                state, revision, created_at, updated_at, archived
         FROM objective WHERE display_alias = ?1",
        params![display],
        map_objective,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound {
        alias: display.to_owned(),
    })
}

fn load_by_id(tx: &Transaction<'_>, id: OpaqueId) -> Result<Objective, Error> {
    tx.query_row(
        "SELECT id, repo, alias_number, display_alias, title, description,
                state, revision, created_at, updated_at, archived
         FROM objective WHERE id = ?1",
        params![id.as_slice()],
        map_objective,
    )
    .pipe_missing()
}

fn map_objective(row: &rusqlite::Row<'_>) -> rusqlite::Result<Objective> {
    let id: Vec<u8> = row.get(0)?;
    let repo: String = row.get(1)?;
    let number: i64 = row.get(2)?;
    let state: String = row.get(6)?;
    let revision: i64 = row.get(7)?;
    let archived: i64 = row.get(10)?;
    let id = OpaqueId::from_slice(&id).map_err(sqlite_corrupt)?;
    let number =
        u64::try_from(number).map_err(|_| sqlite_corrupt(Error::Corrupt("alias".into())))?;
    let revision =
        u64::try_from(revision).map_err(|_| sqlite_corrupt(Error::Corrupt("revision".into())))?;
    let state = ObjectiveState::parse(&state).map_err(sqlite_corrupt)?;
    Ok(Objective {
        id,
        alias: Alias {
            repo: repo.clone(),
            number,
        },
        repo,
        title: row.get(4)?,
        description: row.get(5)?,
        state,
        revision,
        archived: archived != 0,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn map_signal(row: &rusqlite::Row<'_>) -> rusqlite::Result<Signal> {
    let id: Vec<u8> = row.get(0)?;
    let kind: String = row.get(1)?;
    Ok(Signal {
        id: OpaqueId::from_slice(&id).map_err(sqlite_corrupt)?,
        kind: SignalKind::parse(&kind).map_err(sqlite_corrupt)?,
        body: row.get(2)?,
        created_at: row.get(3)?,
        resolved_at: row.get(4)?,
    })
}

fn sqlite_corrupt(err: Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(err))
}

trait MissingObjective {
    fn pipe_missing(self) -> Result<Objective, Error>;
}

impl MissingObjective for rusqlite::Result<Objective> {
    fn pipe_missing(self) -> Result<Objective, Error> {
        self.optional()?
            .ok_or_else(|| Error::Corrupt("objective disappeared".to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::process::Command;

    use super::{
        AddObjective, EditObjective, ListFilter, RaiseSignal, RevisionEdit, StateEdit, Store,
    };
    use crate::error::Error;
    use crate::{ObjectiveState, OperationId, SignalKind};

    fn open() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(&dir.path().join("work-v1.sqlite")).expect("open");
        (dir, store)
    }

    fn add_op(repo: &str, title: &str, operation_id: OperationId) -> AddObjective {
        AddObjective {
            repo: repo.to_owned(),
            title: title.to_owned(),
            description: String::new(),
            operation_id,
            actor: "tester".to_owned(),
        }
    }

    fn add(repo: &str, title: &str) -> AddObjective {
        add_op(repo, title, OperationId::generate().expect("id"))
    }

    #[test]
    fn schema_is_created_at_user_version_one_and_mode_0600() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("work-v1.sqlite");
        let store = Store::open(&path).expect("open");
        assert_eq!(store.user_version().expect("version"), 1);
        let (journal, synchronous) = store.durability().expect("pragmas");
        assert_eq!(journal.to_ascii_lowercase(), "wal");
        assert_eq!(synchronous, 2);
        let tables = store.table_names().expect("tables");
        for name in [
            "alias_counter",
            "binding",
            "event",
            "objective",
            "operation",
            "run",
            "signal",
        ] {
            assert!(
                tables.iter().any(|table| table == name),
                "{tables:?} missing {name}"
            );
        }
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(dir.path()), 0o700);
        let lock = {
            let mut name = path.as_os_str().to_os_string();
            name.push(".lock");
            path.with_file_name(name)
        };
        assert_eq!(mode(&lock), 0o600);
        drop(store);
        let again = Store::open(&path).expect("reopen");
        assert_eq!(again.user_version().expect("version"), 1);
    }

    #[test]
    fn an_unknown_user_version_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("work-v1.sqlite");
        {
            let conn = rusqlite::Connection::open(&path).expect("sqlite");
            conn.pragma_update(None, "user_version", 2_i64)
                .expect("version");
        }
        match Store::open(&path) {
            Err(Error::UnsupportedSchema {
                found: 2,
                supported: 1,
            }) => {}
            other => panic!("expected unsupported schema, got {other:?}"),
        }
        let conn = rusqlite::Connection::open(&path).expect("sqlite");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, 2);
    }

    #[test]
    fn aliases_allocate_per_repo_beside_opaque_ids() {
        let (_dir, mut store) = open();
        let first = store.add(add("phux", "one")).expect("add");
        let second = store.add(add("phux", "two")).expect("add");
        let other = store.add(add("other", "one")).expect("add");
        assert_eq!(first.objective.alias.display(), "phux/1");
        assert_eq!(second.objective.alias.display(), "phux/2");
        assert_eq!(other.objective.alias.display(), "other/1");
        assert_ne!(first.objective.id, second.objective.id);
        assert_eq!(first.objective.id.to_hex().len(), 32);
        assert_eq!(first.objective.revision, 1);
        assert_eq!(first.objective.state, ObjectiveState::Todo);
    }

    #[test]
    fn replaying_an_operation_id_is_a_no_op_and_a_different_body_conflicts() {
        let (_dir, mut store) = open();
        let operation_id = OperationId::generate().expect("id");
        let first = store.add(add_op("phux", "one", operation_id)).expect("add");
        assert!(!first.idempotent_replay);
        let replay = store
            .add(add_op("phux", "one", operation_id))
            .expect("replay");
        assert!(replay.idempotent_replay);
        assert_eq!(replay.objective.id, first.objective.id);
        assert_eq!(
            store.event_kinds().expect("events"),
            ["objective_created".to_owned()]
        );
        match store.add(add_op("phux", "different", operation_id)) {
            Err(Error::OperationConflict { .. }) => {}
            other => panic!("expected operation conflict, got {other:?}"),
        }
        let second = store.add(add("phux", "two")).expect("second");
        assert_eq!(second.objective.alias.display(), "phux/2");
        assert_eq!(
            store
                .list(&ListFilter {
                    repo: None,
                    archived: false,
                })
                .expect("list")
                .len(),
            2
        );
    }

    #[test]
    fn a_stale_expected_revision_fails_without_writing() {
        let (_dir, mut store) = open();
        let added = store.add(add("phux", "one")).expect("add");
        let alias = added.objective.alias;
        let edited = store
            .edit(EditObjective {
                alias: alias.clone(),
                title: Some("renamed".to_owned()),
                description: None,
                expected_revision: 1,
                operation_id: OperationId::generate().expect("id"),
                actor: "tester".to_owned(),
            })
            .expect("edit");
        assert_eq!(edited.objective.revision, 2);
        assert_eq!(edited.objective.title, "renamed");
        match store.edit(EditObjective {
            alias: alias.clone(),
            title: Some("nope".to_owned()),
            description: None,
            expected_revision: 1,
            operation_id: OperationId::generate().expect("id"),
            actor: "tester".to_owned(),
        }) {
            Err(Error::RevisionConflict {
                expected: 1,
                actual: 2,
                ..
            }) => {}
            other => panic!("expected revision conflict, got {other:?}"),
        }
        let shown = store.show(&alias).expect("show");
        assert_eq!(shown.title, "renamed");
        assert_eq!(shown.revision, 2);
    }

    #[test]
    fn unresolved_credential_lost_puts_a_working_objective_in_needs_you() {
        let (_dir, mut store) = open();
        let added = store.add(add("phux", "one")).expect("add");
        let alias = added.objective.alias;
        let working = store
            .record_state(StateEdit {
                alias: alias.clone(),
                state: ObjectiveState::Working,
                expected_revision: 1,
                operation_id: OperationId::generate().expect("id"),
                actor: "tester".to_owned(),
            })
            .expect("state");
        assert_eq!(working.objective.state, ObjectiveState::Working);
        let raised = store
            .raise_signal(RaiseSignal {
                alias: alias.clone(),
                kind: SignalKind::CredentialLost,
                body: "grant is gone".to_owned(),
                expected_revision: 2,
                operation_id: OperationId::generate().expect("id"),
                actor: "tester".to_owned(),
            })
            .expect("signal");
        assert_eq!(raised.objective.state, ObjectiveState::NeedsYou);
        assert_ne!(raised.objective.state, ObjectiveState::Working);
        let signals = store.signals(&alias).expect("signals");
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].kind, SignalKind::CredentialLost);
        assert!(signals[0].resolved_at.is_none());
        let again = store
            .record_state(StateEdit {
                alias: alias.clone(),
                state: ObjectiveState::Working,
                expected_revision: raised.objective.revision,
                operation_id: OperationId::generate().expect("id"),
                actor: "tester".to_owned(),
            })
            .expect("state again");
        assert_eq!(again.objective.state, ObjectiveState::NeedsYou);
        assert_eq!(
            store.show(&alias).expect("show").state,
            ObjectiveState::NeedsYou
        );
    }

    #[test]
    fn archive_and_restore_bump_revision_once_each() {
        let (_dir, mut store) = open();
        let added = store.add(add("phux", "one")).expect("add");
        let alias = added.objective.alias;
        let archived = store
            .archive(RevisionEdit {
                alias: alias.clone(),
                expected_revision: 1,
                operation_id: OperationId::generate().expect("id"),
                actor: "tester".to_owned(),
            })
            .expect("archive");
        assert!(archived.objective.archived);
        assert_eq!(archived.objective.revision, 2);
        assert!(
            store
                .list(&ListFilter {
                    repo: None,
                    archived: false,
                })
                .expect("active")
                .is_empty()
        );
        let restored = store
            .restore(RevisionEdit {
                alias,
                expected_revision: 2,
                operation_id: OperationId::generate().expect("id"),
                actor: "tester".to_owned(),
            })
            .expect("restore");
        assert!(!restored.objective.archived);
        assert_eq!(restored.objective.revision, 3);
    }

    #[test]
    fn concurrent_writers_lose_no_tasks() {
        if std::env::var_os("PHUX_MISSION_CHILD").is_some() {
            let path = std::env::var("PHUX_MISSION_PATH").expect("path");
            let title = std::env::var("PHUX_MISSION_TITLE").expect("title");
            let operation = std::env::var("PHUX_MISSION_OP").expect("op");
            let mut store = Store::open(Path::new(&path)).expect("child open");
            store
                .add(add_op(
                    "phux",
                    &title,
                    OperationId::parse(&operation).expect("op"),
                ))
                .expect("child add");
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("work-v1.sqlite");
        let mut children = Vec::new();
        for index in 0..4 {
            let operation = format!("{:032x}", index + 1);
            children.push(spawn_self(
                &[
                    ("PHUX_MISSION_CHILD", "1"),
                    ("PHUX_MISSION_TITLE", &format!("task-{index}")),
                    ("PHUX_MISSION_OP", &operation),
                ],
                &path,
            ));
        }
        for child in children {
            let output = child.wait_with_output().expect("wait");
            assert!(
                output.status.success(),
                "child failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let store = Store::open(&path).expect("open");
        let tasks = store
            .list(&ListFilter {
                repo: Some("phux".to_owned()),
                archived: false,
            })
            .expect("list");
        assert_eq!(tasks.len(), 4, "{tasks:?}");
        let titles: BTreeSet<_> = tasks.iter().map(|task| task.title.as_str()).collect();
        let numbers: BTreeSet<_> = tasks.iter().map(|task| task.alias.number).collect();
        assert_eq!(
            titles,
            ["task-0", "task-1", "task-2", "task-3"]
                .into_iter()
                .collect()
        );
        assert_eq!(numbers, (1..=4).collect());
    }

    #[test]
    fn a_second_writer_fails_clearly_without_losing_the_task() {
        if std::env::var_os("PHUX_MISSION_EXPECT_BUSY").is_some() {
            let path = std::env::var("PHUX_MISSION_PATH").expect("path");
            match Store::try_open(Path::new(&path)) {
                Err(err @ Error::Busy) => {
                    assert_eq!(
                        err.to_string(),
                        "another process holds the work store; try again"
                    );
                    return;
                }
                other => panic!("expected a busy lock, got {other:?}"),
            }
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("work-v1.sqlite");
        let mut store = Store::open(&path).expect("open");
        store.add(add("phux", "kept")).expect("add");
        let child = spawn_self(&[("PHUX_MISSION_EXPECT_BUSY", "1")], &path);
        let output = child.wait_with_output().expect("wait");
        assert!(
            output.status.success(),
            "child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let tasks = store
            .list(&ListFilter {
                repo: None,
                archived: false,
            })
            .expect("list");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "kept");
    }

    fn spawn_self(env: &[(&str, &str)], path: &Path) -> std::process::Child {
        let name = std::thread::current()
            .name()
            .expect("the test harness names the test thread")
            .to_owned();
        let mut command = Command::new(std::env::current_exe().expect("exe"));
        command
            .args(["--exact", &name, "--test-threads=1"])
            .env("PHUX_MISSION_PATH", path);
        for (key, value) in env {
            command.env(key, value);
        }
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn")
    }

    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }
}
