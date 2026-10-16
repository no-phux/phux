//! Work store failures. A failed mutation leaves the file as it was.

/// Why a work store operation failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The filesystem refused a create, lock, or permission change.
    #[error("work store io: {0}")]
    Io(#[from] std::io::Error),
    /// SQLite refused a statement.
    #[error("work store: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A stored JSON snapshot did not encode or decode.
    #[error("work store json: {0}")]
    Json(#[from] serde_json::Error),
    /// `PRAGMA user_version` is not the version this binary reads.
    #[error("work store schema version {found} is newer than this binary reads ({supported})")]
    UnsupportedSchema {
        /// The version stored in the file.
        found: i64,
        /// The newest version this binary reads.
        supported: i64,
    },
    /// Another process holds the store lock and no keeper answered.
    #[error("another process holds the work store; try again")]
    Busy,
    /// The keeper could not be started or broke the request exchange.
    #[error("work keeper: {0}")]
    Keeper(String),
    /// The keeper's store failed; the text is the keeper's own error.
    #[error("{0}")]
    Store(String),
    /// No objective has this alias.
    #[error("no task {alias}")]
    NotFound {
        /// The alias that was asked for.
        alias: String,
    },
    /// The caller named a revision the objective no longer has.
    #[error("{alias} is at rev {actual}; rerun with --expected-revision {actual}")]
    RevisionConflict {
        /// Display alias, such as `phux/3`.
        alias: String,
        /// The revision the caller expected.
        expected: u64,
        /// The revision stored now.
        actual: u64,
    },
    /// This operation id was already admitted for a different body.
    #[error("operation {operation_id} was already used for a different write")]
    OperationConflict {
        /// Hex operation id.
        operation_id: String,
    },
    /// The alias is not `repo/n`.
    #[error("alias must look like `phux/3`: {0}")]
    InvalidAlias(String),
    /// The operation id is not 32 hex digits, or it is all zeros.
    #[error("operation id must be 32 hex digits and not all zeros: {0}")]
    InvalidOperationId(String),
    /// The repo id is empty or contains a character an alias cannot carry.
    #[error("repo id must be 1-256 bytes of letters, digits, `.`, `_`, `-`, `/`, or `:`: {0}")]
    InvalidRepo(String),
    /// A text field is empty, too long, or contains a NUL.
    #[error("{field} {reason}")]
    InvalidText {
        /// Field name.
        field: String,
        /// What is wrong with the value.
        reason: String,
    },
    /// `edit` named neither a title nor a description.
    #[error("edit needs a title or a description")]
    EmptyEdit,
    /// The task is archived, so it cannot be started.
    #[error("{alias} is archived; restore it to start it")]
    NotStartable {
        /// Display alias.
        alias: String,
    },
    /// The task already has a live run.
    #[error("{alias} is already running")]
    AlreadyStarted {
        /// Display alias.
        alias: String,
    },
    /// Another live task already binds this worktree or branch.
    #[error("worktree {path} is bound by live task {alias}")]
    WorktreeBound {
        /// The path or branch that is taken.
        path: String,
        /// The task that holds it.
        alias: String,
    },
    /// A running start found the agent cap full.
    #[error("no agent slot free ({running} of {limit} running)")]
    AtCapacity {
        /// Agents with a running, unfinished run.
        running: u64,
        /// The cap the start named.
        limit: u64,
    },
    /// The operating system could not mint an id.
    #[error("could not mint an id: {0}")]
    Random(String),
    /// A row on disk breaks a store invariant.
    #[error("work store corrupt: {0}")]
    Corrupt(String),
}

impl Error {
    pub(crate) fn invalid_text(field: &str, reason: &str) -> Self {
        Self::InvalidText {
            field: field.to_owned(),
            reason: reason.to_owned(),
        }
    }
}
