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

impl From<CheckoutArea> for WireCheckoutArea {
    fn from(value: CheckoutArea) -> Self {
        match value {
            CheckoutArea::Conflicted => Self::Conflicted,
            CheckoutArea::Staged => Self::Staged,
            CheckoutArea::Unstaged => Self::Unstaged,
            CheckoutArea::Untracked => Self::Untracked,
            CheckoutArea::Unknown => Self::Unknown,
        }
    }
}

impl From<WireCheckoutArea> for CheckoutArea {
    fn from(value: WireCheckoutArea) -> Self {
        match value {
            WireCheckoutArea::Conflicted => Self::Conflicted,
            WireCheckoutArea::Staged => Self::Staged,
            WireCheckoutArea::Unstaged => Self::Unstaged,
            WireCheckoutArea::Untracked => Self::Untracked,
            WireCheckoutArea::Unknown => Self::Unknown,
        }
    }
}

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

impl From<WireCheckoutRequest> for CheckoutRequest {
    fn from(value: WireCheckoutRequest) -> Self {
        match value {
            WireCheckoutRequest::Status => Self::Status,
            WireCheckoutRequest::Diff { path, area } => Self::Diff {
                path,
                area: area.into(),
            },
            WireCheckoutRequest::Log { skip, limit, path } => Self::Log { skip, limit, path },
            WireCheckoutRequest::Branches => Self::Branches,
            WireCheckoutRequest::Tree { dir } => Self::Tree { dir },
            WireCheckoutRequest::Read { path, max_bytes } => Self::Read { path, max_bytes },
            WireCheckoutRequest::PullRequest => Self::PullRequest,
            WireCheckoutRequest::Stage { paths } => Self::Stage { paths },
            WireCheckoutRequest::Unstage { paths } => Self::Unstage { paths },
        }
    }
}

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

impl From<CheckoutRefusalCode> for WireCheckoutError {
    fn from(code: CheckoutRefusalCode) -> Self {
        use CheckoutRefusalCode as C;
        match code {
            C::CwdUnknown => Self::CwdUnknown,
            C::NotARepository => Self::NotARepository,
            C::InvalidPath => Self::InvalidPath,
            C::NotFound => Self::NotFound,
            C::InvalidRequest => Self::InvalidRequest,
            C::Timeout => Self::Timeout,
            C::Busy => Self::Busy,
            C::GitFailed => Self::GitFailed,
            C::ReadFailed => Self::ReadFailed,
            C::GhUnavailable => Self::GhUnavailable,
            C::GhUnauthenticated => Self::GhUnauthenticated,
            C::NoPullRequest => Self::NoPullRequest,
            C::GhFailed => Self::GhFailed,
            C::Unknown => Self::Other,
        }
    }
}

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

impl From<CheckoutStatus> for WireCheckoutStatus {
    fn from(value: CheckoutStatus) -> Self {
        Self {
            root: value.root,
            branch: value.branch,
            head: value.head,
            upstream: value.upstream,
            ahead: value.ahead,
            behind: value.behind,
            entries: value.entries.into_iter().map(Into::into).collect(),
            truncated: value.truncated,
        }
    }
}

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

impl From<CheckoutEntry> for WireCheckoutEntry {
    fn from(value: CheckoutEntry) -> Self {
        Self {
            path: value.path,
            from: value.from,
            area: value.area.into(),
            code: value.code,
            added: value.added,
            deleted: value.deleted,
            binary: value.binary,
        }
    }
}

/// One file's unified diff.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutDiff {
    pub path: String,
    pub area: WireCheckoutArea,
    pub text: String,
    pub truncated: bool,
    pub binary: bool,
}

impl From<CheckoutDiff> for WireCheckoutDiff {
    fn from(value: CheckoutDiff) -> Self {
        Self {
            path: value.path,
            area: value.area.into(),
            text: value.text,
            truncated: value.truncated,
            binary: value.binary,
        }
    }
}

/// One page of history.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutLog {
    pub commits: Vec<WireCheckoutCommit>,
    pub has_more: bool,
}

impl From<CheckoutLog> for WireCheckoutLog {
    fn from(value: CheckoutLog) -> Self {
        Self {
            commits: value.commits.into_iter().map(Into::into).collect(),
            has_more: value.has_more,
        }
    }
}

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

impl From<CheckoutCommit> for WireCheckoutCommit {
    fn from(value: CheckoutCommit) -> Self {
        Self {
            sha: value.sha,
            short_sha: value.short_sha,
            subject: value.subject,
            author_name: value.author_name,
            author_email: value.author_email,
            time: value.time,
            refs: value.refs,
        }
    }
}

/// Every branch, locals first.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutBranches {
    pub current: Option<String>,
    pub branches: Vec<WireCheckoutBranch>,
    pub truncated: bool,
}

impl From<CheckoutBranches> for WireCheckoutBranches {
    fn from(value: CheckoutBranches) -> Self {
        Self {
            current: value.current,
            branches: value.branches.into_iter().map(Into::into).collect(),
            truncated: value.truncated,
        }
    }
}

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

impl From<CheckoutBranch> for WireCheckoutBranch {
    fn from(value: CheckoutBranch) -> Self {
        Self {
            name: value.name,
            remote: value.remote,
            current: value.current,
            upstream: value.upstream,
            ahead: value.ahead,
            behind: value.behind,
            sha: value.sha,
            time: value.time,
            subject: value.subject,
        }
    }
}

/// What a directory entry is.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireCheckoutEntryKind {
    File,
    Directory,
    Symlink,
    Unknown,
}

impl From<CheckoutEntryKind> for WireCheckoutEntryKind {
    fn from(value: CheckoutEntryKind) -> Self {
        match value {
            CheckoutEntryKind::File => Self::File,
            CheckoutEntryKind::Directory => Self::Directory,
            CheckoutEntryKind::Symlink => Self::Symlink,
            CheckoutEntryKind::Unknown => Self::Unknown,
        }
    }
}

/// One directory listing, directories first.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutTree {
    pub dir: String,
    pub entries: Vec<WireCheckoutTreeEntry>,
    pub truncated: bool,
}

impl From<CheckoutTree> for WireCheckoutTree {
    fn from(value: CheckoutTree) -> Self {
        Self {
            dir: value.dir,
            entries: value.entries.into_iter().map(Into::into).collect(),
            truncated: value.truncated,
        }
    }
}

/// One directory entry.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutTreeEntry {
    pub name: String,
    pub kind: WireCheckoutEntryKind,
    pub size: Option<u64>,
    pub ignored: bool,
}

impl From<CheckoutTreeEntry> for WireCheckoutTreeEntry {
    fn from(value: CheckoutTreeEntry) -> Self {
        Self {
            name: value.name,
            kind: value.kind.into(),
            size: value.size,
            ignored: value.ignored,
        }
    }
}

/// The start of one file.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutFile {
    pub path: String,
    pub size: u64,
    pub binary: bool,
    pub truncated: bool,
    pub text: String,
}

impl From<CheckoutFile> for WireCheckoutFile {
    fn from(value: CheckoutFile) -> Self {
        Self {
            path: value.path,
            size: value.size,
            binary: value.binary,
            truncated: value.truncated,
            text: value.text,
        }
    }
}

/// A pull request's state.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireCheckoutPrState {
    Open,
    Closed,
    Merged,
    Unknown,
}

impl From<CheckoutPrState> for WireCheckoutPrState {
    fn from(value: CheckoutPrState) -> Self {
        match value {
            CheckoutPrState::Open => Self::Open,
            CheckoutPrState::Closed => Self::Closed,
            CheckoutPrState::Merged => Self::Merged,
            CheckoutPrState::Unknown => Self::Unknown,
        }
    }
}

/// A pull request's review decision.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireCheckoutReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
    Unknown,
}

impl From<CheckoutReviewDecision> for WireCheckoutReviewDecision {
    fn from(value: CheckoutReviewDecision) -> Self {
        match value {
            CheckoutReviewDecision::Approved => Self::Approved,
            CheckoutReviewDecision::ChangesRequested => Self::ChangesRequested,
            CheckoutReviewDecision::ReviewRequired => Self::ReviewRequired,
            CheckoutReviewDecision::Unknown => Self::Unknown,
        }
    }
}

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

impl From<CheckoutCheckState> for WireCheckoutCheckState {
    fn from(value: CheckoutCheckState) -> Self {
        match value {
            CheckoutCheckState::Pending => Self::Pending,
            CheckoutCheckState::Success => Self::Success,
            CheckoutCheckState::Failure => Self::Failure,
            CheckoutCheckState::Neutral => Self::Neutral,
            CheckoutCheckState::Skipped => Self::Skipped,
            CheckoutCheckState::Cancelled => Self::Cancelled,
            CheckoutCheckState::Unknown => Self::Unknown,
        }
    }
}

/// One CI check on the pull request's head.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCheckoutCheck {
    pub name: String,
    pub workflow: Option<String>,
    pub state: WireCheckoutCheckState,
    pub url: Option<String>,
}

impl From<CheckoutCheck> for WireCheckoutCheck {
    fn from(value: CheckoutCheck) -> Self {
        Self {
            name: value.name,
            workflow: value.workflow,
            state: value.state.into(),
            url: value.url,
        }
    }
}

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

impl From<CheckoutPullRequest> for WireCheckoutPullRequest {
    fn from(value: CheckoutPullRequest) -> Self {
        Self {
            number: value.number,
            title: value.title,
            state: value.state.into(),
            url: value.url,
            head_ref: value.head_ref,
            is_draft: value.is_draft,
            review_decision: value.review_decision.map(Into::into),
            checks: value.checks.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg(test)]
mod tests;
