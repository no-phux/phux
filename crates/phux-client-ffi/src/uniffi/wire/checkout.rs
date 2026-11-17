//! The Swift/Kotlin checkout vocabulary: one request enum, one answer record,
//! and the typed records each answer carries. A server refusal and a runtime
//! failure share one error vocabulary.

use phux_protocol::checkout::{
    CheckoutAnswer, CheckoutArea, CheckoutBranch, CheckoutBranches, CheckoutCheck,
    CheckoutCheckState, CheckoutCommit, CheckoutDiff, CheckoutEntry, CheckoutEntryKind,
    CheckoutFile, CheckoutLog, CheckoutPrState, CheckoutPullRequest, CheckoutRequest,
    CheckoutReviewDecision, CheckoutStatus, CheckoutTree, CheckoutTreeEntry,
};

use phux_client_runtime::control::{CheckoutFailure, CheckoutOutcome, CheckoutResponse};
use phux_protocol::checkout::CheckoutRefusalCode;

use crate::projection::id;

/// Which comparison a change is in.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireCheckoutArea {
    Conflicted,
    Staged,
    Unstaged,
    Untracked,
    /// A value this build does not know; never send it.
    Unknown,
}

mirror_enum_from!(CheckoutArea <=> WireCheckoutArea {
    Conflicted,
    Staged,
    Unstaged,
    Untracked,
    Unknown,
});

/// One checkout operation on the checkout a Terminal is in. Paths are
/// relative to the checkout's top level.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum WireCheckoutRequest {
    /// Branch, upstream, and every change with line counts.
    Status,
    /// One file's unified diff in one area.
    Diff {
        path: String,
        area: WireCheckoutArea,
    },
    /// One page of history, newest first; `limit` is clamped to 1..=100.
    Log {
        skip: u32,
        limit: u32,
        path: Option<String>,
    },
    /// Local, then remote-tracking branches.
    Branches,
    /// One directory; empty `dir` is the top level.
    Tree { dir: String },
    /// The start of a file as text; zero `max_bytes` means 1 MiB.
    Read { path: String, max_bytes: u32 },
    /// The branch's pull request and checks, from the host's `gh`.
    PullRequest,
    /// Stage paths; answers the refreshed status.
    Stage { paths: Vec<String> },
    /// Unstage paths; answers the refreshed status.
    Unstage { paths: Vec<String> },
}

mirror_enum_from!(WireCheckoutRequest => CheckoutRequest {
    Status,
    Diff { path, area: into },
    Log { skip, limit, path },
    Branches,
    Tree { dir },
    Read { path, max_bytes },
    PullRequest,
    Stage { paths },
    Unstage { paths },
});

/// Why a checkout request produced no answer.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireCheckoutError {
    CwdUnknown,
    NotARepository,
    InvalidPath,
    NotFound,
    InvalidRequest,
    Timeout,
    Busy,
    GitFailed,
    ReadFailed,
    GhUnavailable,
    GhUnauthenticated,
    NoPullRequest,
    GhFailed,
    PermissionDenied,
    TerminalNotFound,
    Unsupported,
    Unanswered,
    Other,
}

mirror_enum_from!(CheckoutRefusalCode => WireCheckoutError {
    CwdUnknown,
    NotARepository,
    InvalidPath,
    NotFound,
    InvalidRequest,
    Timeout,
    Busy,
    GitFailed,
    ReadFailed,
    GhUnavailable,
    GhUnauthenticated,
    NoPullRequest,
    GhFailed,
    Unknown => Other,
});

impl From<CheckoutFailure> for WireCheckoutError {
    fn from(failure: CheckoutFailure) -> Self {
        match failure {
            CheckoutFailure::PermissionDenied => Self::PermissionDenied,
            CheckoutFailure::TerminalNotFound => Self::TerminalNotFound,
            CheckoutFailure::Unsupported => Self::Unsupported,
            CheckoutFailure::Busy => Self::Busy,
            CheckoutFailure::Unanswered => Self::Unanswered,
            CheckoutFailure::Other => Self::Other,
        }
    }
}

/// One correlated checkout answer.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutAnswer {
    pub request_id: u32,
    pub terminal_id: String,
    pub result: WireCheckoutResult,
}

impl From<CheckoutResponse> for WireCheckoutAnswer {
    fn from(value: CheckoutResponse) -> Self {
        Self {
            request_id: value.request_id,
            terminal_id: id::encode(&value.terminal_id),
            result: value.outcome.into(),
        }
    }
}

/// A typed answer, or why there is none. `Stage` and `Unstage` answer
/// `Status`.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum WireCheckoutResult {
    Status {
        status: WireCheckoutStatus,
    },
    Diff {
        diff: WireCheckoutDiff,
    },
    Log {
        log: WireCheckoutLog,
    },
    Branches {
        branches: WireCheckoutBranches,
    },
    Tree {
        tree: WireCheckoutTree,
    },
    File {
        file: WireCheckoutFile,
    },
    PullRequest {
        pull_request: WireCheckoutPullRequest,
    },
    /// The message is display-only.
    Failed {
        error: WireCheckoutError,
        message: String,
    },
}

impl From<CheckoutOutcome> for WireCheckoutResult {
    fn from(value: CheckoutOutcome) -> Self {
        match value {
            CheckoutOutcome::Failed { failure, message } => Self::Failed {
                error: failure.into(),
                message,
            },
            CheckoutOutcome::Answered(answer) => match *answer {
                CheckoutAnswer::Status(status) => Self::Status {
                    status: status.into(),
                },
                CheckoutAnswer::Diff(diff) => Self::Diff { diff: diff.into() },
                CheckoutAnswer::Log(log) => Self::Log { log: log.into() },
                CheckoutAnswer::Branches(branches) => Self::Branches {
                    branches: branches.into(),
                },
                CheckoutAnswer::Tree(tree) => Self::Tree { tree: tree.into() },
                CheckoutAnswer::File(file) => Self::File { file: file.into() },
                CheckoutAnswer::PullRequest(pr) => Self::PullRequest {
                    pull_request: pr.into(),
                },
                CheckoutAnswer::Refused(refusal) => Self::Failed {
                    error: refusal.code.into(),
                    message: refusal.message,
                },
            },
        }
    }
}

/// The checked-out branch and its changes.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutStatus {
    pub root: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub entries: Vec<WireCheckoutEntry>,
    pub truncated: bool,
}

mirror_from!(CheckoutStatus => WireCheckoutStatus {
    root,
    branch,
    head,
    upstream,
    ahead,
    behind,
    entries: vec,
    truncated,
});

/// One changed path in one area.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutEntry {
    pub path: String,
    pub from: Option<String>,
    pub area: WireCheckoutArea,
    pub code: String,
    pub added: Option<u32>,
    pub deleted: Option<u32>,
    pub binary: bool,
}

mirror_from!(CheckoutEntry => WireCheckoutEntry {
    path,
    from,
    area: into,
    code,
    added,
    deleted,
    binary,
});

/// One file's unified diff.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutDiff {
    pub path: String,
    pub area: WireCheckoutArea,
    pub text: String,
    pub truncated: bool,
    pub binary: bool,
}

mirror_from!(CheckoutDiff => WireCheckoutDiff {
    path,
    area: into,
    text,
    truncated,
    binary,
});

/// One page of history.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutLog {
    pub commits: Vec<WireCheckoutCommit>,
    pub has_more: bool,
}

mirror_from!(CheckoutLog => WireCheckoutLog { commits: vec, has_more });

/// One commit; `time` is the author time in Unix seconds.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutCommit {
    pub sha: String,
    pub short_sha: String,
    pub subject: String,
    pub author_name: String,
    pub author_email: String,
    pub time: i64,
    pub refs: Vec<String>,
}

mirror_from!(CheckoutCommit => WireCheckoutCommit {
    sha,
    short_sha,
    subject,
    author_name,
    author_email,
    time,
    refs,
});

/// Every branch, locals first.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutBranches {
    pub current: Option<String>,
    pub branches: Vec<WireCheckoutBranch>,
    pub truncated: bool,
}

mirror_from!(CheckoutBranches => WireCheckoutBranches {
    current,
    branches: vec,
    truncated,
});

/// One branch; `time` is the tip's committer time in Unix seconds.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutBranch {
    pub name: String,
    pub remote: bool,
    pub current: bool,
    pub upstream: Option<String>,
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    pub sha: String,
    pub time: i64,
    pub subject: String,
}

mirror_from!(CheckoutBranch => WireCheckoutBranch {
    name,
    remote,
    current,
    upstream,
    ahead,
    behind,
    sha,
    time,
    subject,
});

/// What a directory entry is.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireCheckoutEntryKind {
    File,
    Directory,
    Symlink,
    Unknown,
}

mirror_enum_from!(CheckoutEntryKind => WireCheckoutEntryKind {
    File,
    Directory,
    Symlink,
    Unknown,
});

/// One directory listing, directories first.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutTree {
    pub dir: String,
    pub entries: Vec<WireCheckoutTreeEntry>,
    pub truncated: bool,
}

mirror_from!(CheckoutTree => WireCheckoutTree { dir, entries: vec, truncated });

/// One directory entry.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutTreeEntry {
    pub name: String,
    pub kind: WireCheckoutEntryKind,
    pub size: Option<u64>,
    pub ignored: bool,
}

mirror_from!(CheckoutTreeEntry => WireCheckoutTreeEntry {
    name,
    kind: into,
    size,
    ignored,
});

/// The start of one file.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutFile {
    pub path: String,
    pub size: u64,
    pub binary: bool,
    pub truncated: bool,
    pub text: String,
}

mirror_from!(CheckoutFile => WireCheckoutFile {
    path,
    size,
    binary,
    truncated,
    text,
});

/// A pull request's state.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireCheckoutPrState {
    Open,
    Closed,
    Merged,
    Unknown,
}

mirror_enum_from!(CheckoutPrState => WireCheckoutPrState {
    Open,
    Closed,
    Merged,
    Unknown,
});

/// A pull request's review decision.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireCheckoutReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
    Unknown,
}

mirror_enum_from!(CheckoutReviewDecision => WireCheckoutReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
    Unknown,
});

/// One check's folded state.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireCheckoutCheckState {
    Pending,
    Success,
    Failure,
    Neutral,
    Skipped,
    Cancelled,
    Unknown,
}

mirror_enum_from!(CheckoutCheckState => WireCheckoutCheckState {
    Pending,
    Success,
    Failure,
    Neutral,
    Skipped,
    Cancelled,
    Unknown,
});

/// One CI check on the pull request's head.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutCheck {
    pub name: String,
    pub workflow: Option<String>,
    pub state: WireCheckoutCheckState,
    pub url: Option<String>,
}

mirror_from!(CheckoutCheck => WireCheckoutCheck {
    name,
    workflow,
    state: into,
    url,
});

/// The current branch's pull request.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutPullRequest {
    pub number: u64,
    pub title: String,
    pub state: WireCheckoutPrState,
    pub url: String,
    pub head_ref: String,
    pub is_draft: bool,
    pub review_decision: Option<WireCheckoutReviewDecision>,
    pub checks: Vec<WireCheckoutCheck>,
}

mirror_from!(CheckoutPullRequest => WireCheckoutPullRequest {
    number,
    title,
    state: into,
    url,
    head_ref,
    is_draft,
    review_decision: opt,
    checks: vec,
});

#[cfg(test)]
mod tests;
