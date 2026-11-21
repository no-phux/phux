//! The pull-request answer: the current branch's PR and its checks.

use serde::{Deserialize, Serialize};

/// A pull request's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutPrState {
    /// Open.
    Open,
    /// Closed without merging.
    Closed,
    /// Merged.
    Merged,
    /// A value this build does not know.
    #[serde(other)]
    Unknown,
}

/// A pull request's review decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutReviewDecision {
    /// Approved.
    Approved,
    /// Changes requested.
    ChangesRequested,
    /// A review is required.
    ReviewRequired,
    /// A value this build does not know.
    #[serde(other)]
    Unknown,
}

/// One check's state, folded from check runs and commit statuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutCheckState {
    /// Queued or running.
    Pending,
    /// Passed.
    Success,
    /// Failed, errored, or timed out.
    Failure,
    /// Finished without a verdict.
    Neutral,
    /// Skipped.
    Skipped,
    /// Cancelled.
    Cancelled,
    /// A value this build does not know.
    #[serde(other)]
    Unknown,
}

/// One CI check on the pull request's head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutCheck {
    /// Check or status context name.
    pub name: String,
    /// The workflow that ran it, when it is an Actions check.
    #[serde(default)]
    pub workflow: Option<String>,
    /// Folded state.
    pub state: CheckoutCheckState,
    /// Details link.
    #[serde(default)]
    pub url: Option<String>,
}

/// The current branch's pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutPullRequest {
    /// Pull request number.
    pub number: u64,
    /// Title.
    #[serde(default)]
    pub title: String,
    /// State.
    pub state: CheckoutPrState,
    /// Web link.
    #[serde(default)]
    pub url: String,
    /// The head branch.
    #[serde(default)]
    pub head_ref: String,
    /// The branch it merges into; empty from a server that predates it.
    #[serde(default)]
    pub base_ref: String,
    /// A draft.
    #[serde(default)]
    pub is_draft: bool,
    /// Review decision, when the repository requires reviews.
    #[serde(default)]
    pub review_decision: Option<CheckoutReviewDecision>,
    /// Checks on the head commit.
    #[serde(default)]
    pub checks: Vec<CheckoutCheck>,
}
