//! Directory listings and path-query records.

#![allow(
    clippy::wildcard_imports,
    reason = "wire records intentionally share the parent UniFFI enum namespace"
)]

use super::*;

/// Why a `LIST_DIRECTORY` produced no listing. The first four mirror the
/// wire's `DirectoryErrorCode` one for one (an unallocated wire value already
/// reads as `Other`); `Unanswered` is bridge-local: the connection carrying
/// the request ended before the server replied. Listing is read-only, so a
/// retry is always safe.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireDirectoryErrorCode {
    NotFound,
    PermissionDenied,
    NotADirectory,
    Other,
    Unanswered,
}

impl From<outcome::DirectoryError> for WireDirectoryErrorCode {
    fn from(value: outcome::DirectoryError) -> Self {
        match value {
            outcome::DirectoryError::NotFound => Self::NotFound,
            outcome::DirectoryError::PermissionDenied => Self::PermissionDenied,
            outcome::DirectoryError::NotADirectory => Self::NotADirectory,
            outcome::DirectoryError::Other => Self::Other,
            outcome::DirectoryError::Unanswered => Self::Unanswered,
        }
    }
}

/// One child directory in a listing.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireDirectoryEntry {
    pub name: String,
    /// True when the entry is a symbolic link that resolves to a directory.
    pub is_symlink: bool,
}

impl From<outcome::DirectoryEntry> for WireDirectoryEntry {
    fn from(value: outcome::DirectoryEntry) -> Self {
        Self {
            name: value.name,
            is_symlink: value.is_symlink,
        }
    }
}

/// The correlated answer to one `list_directory` call, drained losslessly
/// through `take_directory_listings`. `error` is `None` on success; then
/// `path` is the resolved absolute path, `parent` its lexical parent (`None`
/// at the root), and `entries` the child directories sorted by name, capped
/// server-side with `truncated` set. On a refusal `path` is the path the
/// server attempted, the listing fields are empty, and `message` is
/// diagnostic text that must not be parsed.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireDirectoryListing {
    pub request_id: u32,
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<WireDirectoryEntry>,
    pub truncated: bool,
    pub error: Option<WireDirectoryErrorCode>,
    pub message: String,
}

impl From<outcome::Directory> for WireDirectoryListing {
    fn from(value: outcome::Directory) -> Self {
        Self {
            request_id: value.request_id,
            path: value.path,
            parent: value.parent,
            entries: value.entries.into_iter().map(Into::into).collect(),
            truncated: value.truncated,
            error: value.error.map(Into::into),
            message: value.message,
        }
    }
}

/// Host path kind. A symlink is not implicitly a directory.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WirePathKind {
    File,
    Directory,
    Symlink,
}

impl From<outcome::PathKind> for WirePathKind {
    fn from(value: outcome::PathKind) -> Self {
        match value {
            outcome::PathKind::File => Self::File,
            outcome::PathKind::Directory => Self::Directory,
            outcome::PathKind::Symlink => Self::Symlink,
        }
    }
}

/// Completeness of one answer; warming and truncated may be retried.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WirePathStatus {
    Complete,
    Warming,
    Truncated,
}

impl From<outcome::PathStatus> for WirePathStatus {
    fn from(value: outcome::PathStatus) -> Self {
        match value {
            outcome::PathStatus::Complete => Self::Complete,
            outcome::PathStatus::Warming => Self::Warming,
            outcome::PathStatus::Truncated => Self::Truncated,
        }
    }
}

/// Typed refusal; unanswered means the carrying connection ended.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WirePathError {
    NotFound,
    PermissionDenied,
    NotADirectory,
    Other,
    Unanswered,
}

impl From<outcome::PathError> for WirePathError {
    fn from(value: outcome::PathError) -> Self {
        match value {
            outcome::PathError::NotFound => Self::NotFound,
            outcome::PathError::PermissionDenied => Self::PermissionDenied,
            outcome::PathError::NotADirectory => Self::NotADirectory,
            outcome::PathError::Other => Self::Other,
            outcome::PathError::Unanswered => Self::Unanswered,
        }
    }
}

/// Absolute host path matched by the query. Escape for the target shell.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WirePathRow {
    pub path: String,
    pub kind: WirePathKind,
}

impl From<outcome::PathRow> for WirePathRow {
    fn from(value: outcome::PathRow) -> Self {
        Self {
            path: value.path,
            kind: value.kind.into(),
        }
    }
}

/// One correlated bounded answer, or a typed refusal/cancellation.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WirePathAnswer {
    pub request_id: u32,
    pub root: String,
    pub parent: Option<String>,
    pub rows: Vec<WirePathRow>,
    pub status: Option<WirePathStatus>,
    pub error: Option<WirePathError>,
    pub message: String,
}

impl From<outcome::PathResult> for WirePathAnswer {
    fn from(value: outcome::PathResult) -> Self {
        Self {
            request_id: value.request_id,
            root: value.root,
            parent: value.parent,
            rows: value.rows.into_iter().map(Into::into).collect(),
            status: value.status.map(Into::into),
            error: value.error.map(Into::into),
            message: value.message,
        }
    }
}
