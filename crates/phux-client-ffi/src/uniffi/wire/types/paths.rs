//! Directory listings and path-query records.

use phux_client_runtime::control::{
    DirectoryChild, DirectoryFailure, DirectoryListing, PathAnswer, PathFailure, PathMatch,
    PathMatchKind, PathSearchStatus,
};

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

impl From<DirectoryFailure> for WireDirectoryErrorCode {
    fn from(value: DirectoryFailure) -> Self {
        match value {
            DirectoryFailure::NotFound => Self::NotFound,
            DirectoryFailure::PermissionDenied => Self::PermissionDenied,
            DirectoryFailure::NotADirectory => Self::NotADirectory,
            DirectoryFailure::Other => Self::Other,
            DirectoryFailure::Unanswered => Self::Unanswered,
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

impl From<DirectoryChild> for WireDirectoryEntry {
    fn from(value: DirectoryChild) -> Self {
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

impl From<DirectoryListing> for WireDirectoryListing {
    fn from(value: DirectoryListing) -> Self {
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

impl From<PathMatchKind> for WirePathKind {
    fn from(value: PathMatchKind) -> Self {
        match value {
            PathMatchKind::File => Self::File,
            PathMatchKind::Directory => Self::Directory,
            PathMatchKind::Symlink => Self::Symlink,
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

impl From<PathSearchStatus> for WirePathStatus {
    fn from(value: PathSearchStatus) -> Self {
        match value {
            PathSearchStatus::Complete => Self::Complete,
            PathSearchStatus::Warming => Self::Warming,
            PathSearchStatus::Truncated => Self::Truncated,
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

impl From<PathFailure> for WirePathError {
    fn from(value: PathFailure) -> Self {
        match value {
            PathFailure::NotFound => Self::NotFound,
            PathFailure::PermissionDenied => Self::PermissionDenied,
            PathFailure::NotADirectory => Self::NotADirectory,
            PathFailure::Other => Self::Other,
            PathFailure::Unanswered => Self::Unanswered,
        }
    }
}

/// Absolute host path matched by the query. Escape for the target shell.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WirePathRow {
    pub path: String,
    pub kind: WirePathKind,
}

impl From<PathMatch> for WirePathRow {
    fn from(value: PathMatch) -> Self {
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

impl From<PathAnswer> for WirePathAnswer {
    fn from(value: PathAnswer) -> Self {
        Self {
            request_id: value.request_id,
            root: value.root,
            parent: value.parent,
            rows: value.rows.into_iter().map(Into::into).collect(),
            status: value.status.map(Into::into),
            error: value.failure.map(Into::into),
            message: value.message,
        }
    }
}
