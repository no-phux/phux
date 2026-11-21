//! History answers: a page of commits, and one commit in full.

use serde::{Deserialize, Serialize};

/// One commit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutCommit {
    /// Full commit id.
    pub sha: String,
    /// Abbreviated commit id.
    #[serde(default)]
    pub short_sha: String,
    /// First line of the message.
    #[serde(default)]
    pub subject: String,
    /// Author name.
    #[serde(default)]
    pub author_name: String,
    /// Author email.
    #[serde(default)]
    pub author_email: String,
    /// Author time, Unix seconds.
    #[serde(default)]
    pub time: i64,
    /// Refs pointing at the commit, as `git log --decorate` names them, e.g.
    /// `HEAD -> main`, `origin/main`, `tag: v1.0`.
    #[serde(default)]
    pub refs: Vec<String>,
}

/// One page of history.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutLog {
    /// Newest first.
    #[serde(default)]
    pub commits: Vec<CheckoutCommit>,
    /// Another page follows.
    #[serde(default)]
    pub has_more: bool,
}

/// One commit in full: the reply to `show`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutCommitDetail {
    /// The commit, as a log page lists it.
    #[serde(default)]
    pub commit: CheckoutCommit,
    /// The whole message, subject included.
    #[serde(default)]
    pub message: String,
    /// Files changed against the first parent, at most
    /// [`MAX_CHECKOUT_ENTRIES`](super::MAX_CHECKOUT_ENTRIES).
    #[serde(default)]
    pub files: Vec<CheckoutCommitFile>,
    /// Unified diff against the first parent; non-UTF-8 bytes are replaced
    /// for display.
    #[serde(default)]
    pub diff: String,
    /// The diff (at [`MAX_CHECKOUT_TEXT_BYTES`](super::MAX_CHECKOUT_TEXT_BYTES),
    /// at a line boundary) or the file list was cut.
    #[serde(default)]
    pub truncated: bool,
}

/// One file a commit changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutCommitFile {
    /// Checkout-relative path.
    pub path: String,
    /// The path a rename or copy came from.
    #[serde(default)]
    pub from: Option<String>,
    /// Added lines; `None` for a binary file.
    #[serde(default)]
    pub added: Option<u32>,
    /// Removed lines; `None` for a binary file.
    #[serde(default)]
    pub deleted: Option<u32>,
    /// Git treats the content as binary.
    #[serde(default)]
    pub binary: bool,
}
