//! Typed checkout methods: Git state of the checkout a Terminal is in.
//!
//! `CHECKOUT` (command tag `0x3c`, behind `features_ext.CHECKOUT`) names a
//! Terminal and one typed [`CheckoutRequest`]. The server resolves the
//! checkout from that Terminal's live working directory, never from a path
//! the client supplies, and answers one [`CheckoutAnswer`] in
//! `CommandValue::Checkout`. Paths in requests and answers are relative to
//! the checkout's top level.
//!
//! Requests and answers cross the wire as bounded JSON inside the command
//! envelope, like the native agent payload. Answers tolerate unknown fields
//! and unknown enum values, so a newer server can add detail an older client
//! skips; a new operation needs a new request variant and a client that has
//! seen the capability bit.

use serde::{Deserialize, Serialize};

mod commit;
mod pull_request;
pub use commit::{CheckoutCommit, CheckoutCommitDetail, CheckoutCommitFile, CheckoutLog};
pub use pull_request::{
    CheckoutCheck, CheckoutCheckState, CheckoutPrState, CheckoutPullRequest, CheckoutReviewDecision,
};

/// Largest encoded [`CheckoutRequest`].
pub const MAX_CHECKOUT_REQUEST_BYTES: usize = 64 * 1024;
/// Largest encoded [`CheckoutAnswer`].
pub const MAX_CHECKOUT_ANSWER_BYTES: usize = 4 * 1024 * 1024;
/// Most text one answer carries (a diff or file): longer content is cut at a
/// line boundary and flagged `truncated`.
pub const MAX_CHECKOUT_TEXT_BYTES: usize = 1024 * 1024;
/// Most commits one `log` page returns.
pub const MAX_CHECKOUT_LOG_LIMIT: u32 = 100;
/// Most paths one `stage` or `unstage` names.
pub const MAX_CHECKOUT_PATHS: usize = 1024;
/// Most status entries, branches, or tree entries one answer lists.
pub const MAX_CHECKOUT_ENTRIES: usize = 5000;
/// The answer contract this build reads.
///
/// It rides after the request in the `CHECKOUT` command body. Contract 1 (an
/// older client, which sends no byte) never sees an answer kind or refusal
/// code added by contract 2: [`CheckoutAnswer::for_contract`] folds them back.
pub const CHECKOUT_CONTRACT: u8 = 2;

/// Which comparison a file's change belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutArea {
    /// Unmerged: both sides changed it.
    Conflicted,
    /// In the index against `HEAD`.
    Staged,
    /// Tracked, changed in the working tree against the index.
    Unstaged,
    /// Not tracked yet.
    Untracked,
    /// A value this build does not know.
    #[serde(other)]
    Unknown,
}

/// One checkout operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum CheckoutRequest {
    /// Branch, upstream, and every changed file with line counts.
    Status,
    /// One file's unified diff in one area.
    Diff {
        /// Checkout-relative path.
        path: String,
        /// The comparison: staged, unstaged, or untracked (whole new file).
        area: CheckoutArea,
    },
    /// One page of history, newest first.
    Log {
        /// Commits to skip from `HEAD`.
        skip: u32,
        /// Commits to return, at most [`MAX_CHECKOUT_LOG_LIMIT`].
        limit: u32,
        /// Only commits touching this checkout-relative path.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// Local and remote-tracking branches.
    Branches,
    /// One directory's entries, tracked and untracked alike.
    Tree {
        /// Checkout-relative directory; empty for the top level.
        dir: String,
    },
    /// The start of one file as text.
    Read {
        /// Checkout-relative path.
        path: String,
        /// Most bytes to return, capped at [`MAX_CHECKOUT_TEXT_BYTES`].
        max_bytes: u32,
    },
    /// The current branch's pull request and its checks, from the host's
    /// `gh`.
    PullRequest,
    /// Add these paths to the index. Answers the refreshed status.
    Stage {
        /// Checkout-relative paths, at most [`MAX_CHECKOUT_PATHS`].
        paths: Vec<String>,
    },
    /// Return these paths in the index to `HEAD`. Answers the refreshed
    /// status.
    Unstage {
        /// Checkout-relative paths, at most [`MAX_CHECKOUT_PATHS`].
        paths: Vec<String>,
    },
    /// One commit in full: message, files with line counts, and the patch,
    /// against its first parent. Behind `features_ext.CHECKOUT_V2`.
    Show {
        /// A commit id or a prefix of one: 4 to 64 hex digits.
        sha: String,
    },
}

impl CheckoutRequest {
    /// Whether the request writes the index.
    #[must_use]
    pub const fn mutates(&self) -> bool {
        matches!(self, Self::Stage { .. } | Self::Unstage { .. })
    }
}

/// The answer to one [`CheckoutRequest`]. `Stage` and `Unstage` answer
/// [`Self::Status`], or [`Self::Applied`] when only the status read failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckoutAnswer {
    /// Reply to `status`, `stage`, and `unstage`.
    Status(CheckoutStatus),
    /// Reply to `diff`.
    Diff(CheckoutDiff),
    /// Reply to `log`.
    Log(CheckoutLog),
    /// Reply to `branches`.
    Branches(CheckoutBranches),
    /// Reply to `tree`.
    Tree(CheckoutTree),
    /// Reply to `read`.
    File(CheckoutFile),
    /// Reply to `pull_request`.
    PullRequest(CheckoutPullRequest),
    /// Reply to `show` (contract 2).
    Commit(CheckoutCommitDetail),
    /// Reply to `stage` or `unstage` (contract 2) whose write landed but
    /// whose refreshed status could not be read; the code and message say
    /// why the read failed. Read the status again.
    Applied(CheckoutRefusal),
    /// The operation did not run or did not finish; nothing was changed
    /// unless the code says otherwise.
    Refused(CheckoutRefusal),
}

impl CheckoutAnswer {
    /// This answer as a client reading `contract` can: below 2, an
    /// [`Self::Applied`] reads as the refusal of its status read (what
    /// contract 1 always answered) and `git_unavailable` as `git_failed`.
    #[must_use]
    pub fn for_contract(self, contract: u8) -> Self {
        if contract >= CHECKOUT_CONTRACT {
            return self;
        }
        match self {
            Self::Applied(refusal) => Self::Refused(refusal).for_contract(contract),
            Self::Refused(CheckoutRefusal {
                code: CheckoutRefusalCode::GitUnavailable,
                message,
            }) => Self::Refused(CheckoutRefusal {
                code: CheckoutRefusalCode::GitFailed,
                message,
            }),
            answer => answer,
        }
    }
}

/// Why a checkout operation produced no result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutRefusalCode {
    /// The server could not observe the Terminal's working directory.
    CwdUnknown,
    /// The Terminal's working directory is not inside a Git working tree.
    NotARepository,
    /// A path was absolute, climbed out of the checkout, named `.git`, or
    /// crossed a symbolic link.
    InvalidPath,
    /// The named path does not exist or is not the expected kind.
    NotFound,
    /// The request was well-formed but out of bounds.
    InvalidRequest,
    /// Git or `gh` did not finish in time. A write may or may not have
    /// landed; read the status again.
    Timeout,
    /// Too many checkout operations are already running; retry.
    Busy,
    /// Git refused; the message is Git's own.
    GitFailed,
    /// The host has no working `git` (contract 2; contract 1 reads
    /// `git_failed`).
    GitUnavailable,
    /// The host could not read a file or directory (for example, permission
    /// denied).
    ReadFailed,
    /// The host has no `gh` executable.
    GhUnavailable,
    /// `gh` is not signed in on the host.
    GhUnauthenticated,
    /// The current branch has no pull request.
    NoPullRequest,
    /// `gh` failed for another reason; the message is its own.
    GhFailed,
    /// A value this build does not know.
    #[serde(other)]
    Unknown,
}

/// A typed refusal with a display-only diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutRefusal {
    /// What went wrong.
    pub code: CheckoutRefusalCode,
    /// Display-only detail; never parse it.
    #[serde(default)]
    pub message: String,
}

/// The checked-out branch and its changes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutStatus {
    /// Absolute top level of the working tree on the serving host.
    #[serde(default)]
    pub root: String,
    /// The branch name; `None` when `HEAD` is detached.
    #[serde(default)]
    pub branch: Option<String>,
    /// Full `HEAD` commit id; `None` before the first commit.
    #[serde(default)]
    pub head: Option<String>,
    /// The upstream branch, e.g. `origin/main`.
    #[serde(default)]
    pub upstream: Option<String>,
    /// Commits on the branch and not on its upstream.
    #[serde(default)]
    pub ahead: u32,
    /// Commits on the upstream and not on the branch.
    #[serde(default)]
    pub behind: u32,
    /// Changed files, sorted by area then path. A file staged and edited
    /// again appears once per area.
    #[serde(default)]
    pub entries: Vec<CheckoutEntry>,
    /// More entries existed than [`MAX_CHECKOUT_ENTRIES`].
    #[serde(default)]
    pub truncated: bool,
    /// Every changed entry there is; `None` when the host could not count
    /// them all, or predates the field.
    #[serde(default)]
    pub total: Option<u32>,
}

/// One changed path in one area.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutEntry {
    /// Checkout-relative path.
    pub path: String,
    /// The path a rename or copy came from.
    #[serde(default)]
    pub from: Option<String>,
    /// Which comparison the change is in.
    pub area: CheckoutArea,
    /// Porcelain status letter: `M`, `A`, `D`, `R`, `C`, `T`, `U` or `?`.
    pub code: String,
    /// Added lines; `None` for binary or uncounted files.
    #[serde(default)]
    pub added: Option<u32>,
    /// Removed lines; `None` for binary or uncounted files.
    #[serde(default)]
    pub deleted: Option<u32>,
    /// Git treats the content as binary.
    #[serde(default)]
    pub binary: bool,
}

/// One file's unified diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutDiff {
    /// Checkout-relative path.
    pub path: String,
    /// The comparison shown.
    pub area: CheckoutArea,
    /// Unified diff text; non-UTF-8 bytes are replaced for display. Empty
    /// when the file has no change in that area.
    #[serde(default)]
    pub text: String,
    /// The diff was longer than [`MAX_CHECKOUT_TEXT_BYTES`] and was cut at a
    /// line boundary.
    #[serde(default)]
    pub truncated: bool,
    /// Git reports the content as binary; `text` holds only headers.
    #[serde(default)]
    pub binary: bool,
}

/// One branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutBranch {
    /// Short name: `main`, or `origin/main` for a remote-tracking branch.
    pub name: String,
    /// A remote-tracking branch.
    #[serde(default)]
    pub remote: bool,
    /// Checked out in this working tree.
    #[serde(default)]
    pub current: bool,
    /// The configured upstream of a local branch.
    #[serde(default)]
    pub upstream: Option<String>,
    /// Commits ahead of the upstream, when one is configured and present.
    #[serde(default)]
    pub ahead: Option<u32>,
    /// Commits behind the upstream, when one is configured and present.
    #[serde(default)]
    pub behind: Option<u32>,
    /// Abbreviated tip commit id.
    #[serde(default)]
    pub sha: String,
    /// Tip committer time, Unix seconds.
    #[serde(default)]
    pub time: i64,
    /// Tip commit subject.
    #[serde(default)]
    pub subject: String,
}

/// Local branches, then remote-tracking ones, each newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutBranches {
    /// The current branch; `None` when `HEAD` is detached.
    #[serde(default)]
    pub current: Option<String>,
    /// Every branch, at most [`MAX_CHECKOUT_ENTRIES`].
    #[serde(default)]
    pub branches: Vec<CheckoutBranch>,
    /// More branches existed.
    #[serde(default)]
    pub truncated: bool,
    /// Every branch there is; `None` when the host could not count them all,
    /// or predates the field.
    #[serde(default)]
    pub total: Option<u32>,
}

/// What a tree entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutEntryKind {
    /// A regular file.
    File,
    /// A directory (including a submodule's working tree).
    Directory,
    /// A symbolic link; never followed.
    Symlink,
    /// A value this build does not know.
    #[serde(other)]
    Unknown,
}

/// One directory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutTreeEntry {
    /// Basename.
    pub name: String,
    /// What it is.
    pub kind: CheckoutEntryKind,
    /// Size in bytes, for a file.
    #[serde(default)]
    pub size: Option<u64>,
    /// Git ignores it.
    #[serde(default)]
    pub ignored: bool,
}

/// One directory listing: directories first, then by name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutTree {
    /// The listed checkout-relative directory; empty for the top level.
    #[serde(default)]
    pub dir: String,
    /// Entries, without `.git`.
    #[serde(default)]
    pub entries: Vec<CheckoutTreeEntry>,
    /// More entries existed than [`MAX_CHECKOUT_ENTRIES`].
    #[serde(default)]
    pub truncated: bool,
    /// Every entry there is; `None` when the host could not count them all,
    /// or predates the field.
    #[serde(default)]
    pub total: Option<u32>,
}

/// The start of one file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutFile {
    /// Checkout-relative path.
    #[serde(default)]
    pub path: String,
    /// The file's size in bytes.
    #[serde(default)]
    pub size: u64,
    /// The content is not UTF-8 text; `text` is empty.
    #[serde(default)]
    pub binary: bool,
    /// Fewer bytes than the file holds were returned.
    #[serde(default)]
    pub truncated: bool,
    /// UTF-8 content.
    #[serde(default)]
    pub text: String,
}

/// Encode a request. Serializing these plain records cannot fail.
pub(crate) fn encode_request(request: &CheckoutRequest) -> Vec<u8> {
    serde_json::to_vec(request).unwrap_or_default()
}

/// Encode an answer. Serializing these plain records cannot fail.
pub(crate) fn encode_answer(answer: &CheckoutAnswer) -> Vec<u8> {
    serde_json::to_vec(answer).unwrap_or_default()
}

/// The encoded size of `answer`, for a server bounding its reply.
#[must_use]
pub fn encoded_answer_len(answer: &CheckoutAnswer) -> usize {
    encode_answer(answer).len()
}

/// The encoded size of `request`, for a client refusing an oversized one
/// before sending it.
#[must_use]
pub fn encoded_request_len(request: &CheckoutRequest) -> usize {
    encode_request(request).len()
}

pub(crate) fn decode_request(bytes: &[u8]) -> Result<CheckoutRequest, CheckoutDecodeError> {
    if bytes.len() > MAX_CHECKOUT_REQUEST_BYTES {
        return Err(CheckoutDecodeError::TooLarge);
    }
    // `deny_unknown_fields` still refuses a newer op or field, but as
    // `Unsupported`: a newer client is not a malformed one.
    serde_json::from_slice(bytes).map_err(|error| {
        if crate::wire::error::names_unknown_json(&error.to_string()) {
            CheckoutDecodeError::Unsupported
        } else {
            CheckoutDecodeError::Invalid
        }
    })
}

pub(crate) fn decode_answer(bytes: &[u8]) -> Result<CheckoutAnswer, CheckoutDecodeError> {
    if bytes.len() > MAX_CHECKOUT_ANSWER_BYTES {
        return Err(CheckoutDecodeError::TooLarge);
    }
    // A newer server's answer kind is `Unsupported` (the reply settles as
    // unreadable), not malformed; unknown fields are already skipped.
    serde_json::from_slice(bytes).map_err(|error| {
        if crate::wire::error::names_unknown_json(&error.to_string()) {
            CheckoutDecodeError::Unsupported
        } else {
            CheckoutDecodeError::Invalid
        }
    })
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum CheckoutDecodeError {
    TooLarge,
    Invalid,
    /// Well-formed JSON naming an op or field this build does not know.
    Unsupported,
}

#[cfg(test)]
mod tests;
