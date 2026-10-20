//! The checkout surface's records and its error.

use std::fmt;

use crate::Area;

/// Why a checkout operation produced nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckoutError {
    /// The directory or path does not exist or is not the expected kind.
    NotFound(String),
    /// The directory is not inside a Git working tree.
    NotARepository,
    /// A path is absolute, climbs out, names `.git`, or crosses a symlink.
    InvalidPath(String),
    /// The request is out of bounds.
    InvalidRequest(String),
    /// A child did not finish in time and was killed.
    Timeout,
    /// Git refused; its own message.
    Git(String),
    /// A filesystem read failed.
    Io(String),
    /// No `gh` on this host.
    GhUnavailable,
    /// `gh` is not signed in.
    GhUnauthenticated(String),
    /// The branch has no pull request.
    NoPullRequest,
    /// `gh` failed otherwise; its own message.
    Gh(String),
}

impl fmt::Display for CheckoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(message)
            | Self::InvalidPath(message)
            | Self::InvalidRequest(message)
            | Self::Git(message)
            | Self::Io(message)
            | Self::GhUnauthenticated(message)
            | Self::Gh(message) => f.write_str(message),
            Self::NotARepository => f.write_str("not inside a Git working tree"),
            Self::Timeout => f.write_str("git did not finish in time"),
            Self::GhUnavailable => f.write_str("the GitHub CLI (gh) is not installed on this host"),
            Self::NoPullRequest => f.write_str("this branch has no pull request"),
        }
    }
}

impl std::error::Error for CheckoutError {}

pub(super) type Result<T> = std::result::Result<T, CheckoutError>;

/// A checkout's branch and changes with line counts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// The branch; `None` when `HEAD` is detached.
    pub branch: Option<String>,
    /// Full `HEAD` commit id; `None` before the first commit.
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// Sorted by area, then path.
    pub changes: Vec<Change>,
    /// More changes existed than [`MAX_ENTRIES`](super::MAX_ENTRIES) or fit the output cap.
    pub truncated: bool,
}

/// One changed path in one area, with its line counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub area: Area,
    pub path: String,
    pub from: Option<String>,
    /// Porcelain status letter.
    pub code: char,
    /// `None` for binary or uncounted files.
    pub added: Option<u32>,
    pub deleted: Option<u32>,
    pub binary: bool,
}

/// One file's unified diff.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDiff {
    pub text: String,
    /// Cut at a line boundary to [`MAX_TEXT_BYTES`](super::MAX_TEXT_BYTES).
    pub truncated: bool,
    pub binary: bool,
}

/// One commit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub short_sha: String,
    pub subject: String,
    pub author_name: String,
    pub author_email: String,
    /// Author time, Unix seconds.
    pub time: i64,
    /// Decorations, e.g. `HEAD -> main`, `origin/main`, `tag: v1`.
    pub refs: Vec<String>,
}

/// One page of history.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogPage {
    pub commits: Vec<Commit>,
    pub has_more: bool,
}

/// One branch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchRef {
    /// `main`, or `origin/main` for a remote-tracking branch.
    pub name: String,
    pub remote: bool,
    pub current: bool,
    pub upstream: Option<String>,
    /// Present when the upstream is configured and exists.
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    pub sha: String,
    /// Committer time, Unix seconds.
    pub time: i64,
    pub subject: String,
}

/// Local branches, then remote-tracking ones, each newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchList {
    pub current: Option<String>,
    pub branches: Vec<BranchRef>,
    pub truncated: bool,
}

/// What a directory entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
}

/// One directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListingEntry {
    pub name: String,
    pub kind: EntryKind,
    /// Size in bytes, for a file.
    pub size: Option<u64>,
    pub ignored: bool,
}

/// One directory, directories first, then by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<ListingEntry>,
    pub truncated: bool,
}

/// The start of one file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileText {
    pub size: u64,
    /// Not UTF-8 text; `text` is empty.
    pub binary: bool,
    pub truncated: bool,
    pub text: String,
}
