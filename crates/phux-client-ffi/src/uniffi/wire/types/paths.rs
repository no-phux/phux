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

mirror_enum_from!(DirectoryFailure => WireDirectoryErrorCode {
    NotFound,
    PermissionDenied,
    NotADirectory,
    Other,
    Unanswered,
});

/// One child directory in a listing.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireDirectoryEntry {
    pub name: String,
    /// True when the entry is a symbolic link that resolves to a directory.
    pub is_symlink: bool,
}

mirror_from!(DirectoryChild => WireDirectoryEntry { name, is_symlink });

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

mirror_from!(DirectoryListing => WireDirectoryListing {
    request_id,
    path,
    parent,
    entries: vec,
    truncated,
    error: opt,
    message,
});

/// Host path kind. A symlink is not implicitly a directory.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WirePathKind {
    File,
    Directory,
    Symlink,
}

mirror_enum_from!(PathMatchKind => WirePathKind {
    File,
    Directory,
    Symlink,
});

/// Completeness of one answer; warming and truncated may be retried.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WirePathStatus {
    Complete,
    Warming,
    Truncated,
}

mirror_enum_from!(PathSearchStatus => WirePathStatus {
    Complete,
    Warming,
    Truncated,
});

/// Typed refusal; unanswered means the carrying connection ended.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WirePathError {
    NotFound,
    PermissionDenied,
    NotADirectory,
    Other,
    Unanswered,
}

mirror_enum_from!(PathFailure => WirePathError {
    NotFound,
    PermissionDenied,
    NotADirectory,
    Other,
    Unanswered,
});

/// Absolute host path matched by the query. Escape for the target shell.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WirePathRow {
    pub path: String,
    pub kind: WirePathKind,
}

mirror_from!(PathMatch => WirePathRow { path, kind: into });

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

mirror_from!(PathAnswer => WirePathAnswer {
    request_id,
    root,
    parent,
    rows: vec,
    status: opt,
    failure as error: opt,
    message,
});
