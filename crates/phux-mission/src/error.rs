//! Store failures. A failed mutation leaves the file as it was.

/// Why a mission store operation failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The filesystem refused a create, lock, or permission change.
    #[error("mission store io: {0}")]
    Io(#[from] std::io::Error),
    /// SQLite refused a statement.
    #[error("mission store: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A stored JSON snapshot did not encode or decode.
    #[error("mission store json: {0}")]
    Json(#[from] serde_json::Error),
    /// `PRAGMA user_version` is not the version this binary reads.
    #[error("mission store schema version {found} is newer than this binary reads ({supported})")]
    UnsupportedSchema {
        /// The version stored in the file.
        found: i64,
        /// The newest version this binary reads.
        supported: i64,
    },
    /// Another writer holds the store lock.
    #[error("another phux mission command is writing; try again")]
    Busy,
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
        field: &'static str,
        /// What is wrong with the value.
        reason: &'static str,
    },
    /// `edit` named neither a title nor a description.
    #[error("edit needs a title or a description")]
    EmptyEdit,
    /// The operating system could not mint an id.
    #[error("could not mint an id: {0}")]
    Random(String),
    /// A row on disk breaks a store invariant.
    #[error("mission store corrupt: {0}")]
    Corrupt(String),
}

impl Error {
    pub(crate) const fn invalid_text(field: &'static str, reason: &'static str) -> Self {
        Self::InvalidText { field, reason }
    }
}
