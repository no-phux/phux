//! The current branch's pull request through the host's GitHub CLI.
//!
//! `gh` owns authentication, the remote-to-repository mapping, and GitHub
//! Enterprise hosts, so the server needs no token of its own. A host
//! without `gh`, or with `gh` signed out, is a typed refusal rather than a
//! failure.

use std::path::Path;

use serde::Deserialize;

use super::{CheckoutError, GH_TIMEOUT, Result};
use crate::github::gh::{self, GhError};
pub use crate::github::types::{CheckState, PrState, ReviewDecision};
use crate::github::types::{check_run_state, rollup_state};

const MAX_GH_OUTPUT: usize = 4 * 1024 * 1024;
const FIELDS: &str = "number,title,state,url,headRefName,isDraft,reviewDecision,statusCheckRollup";

/// One check on the head commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub workflow: Option<String>,
    pub state: CheckState,
    pub url: Option<String>,
}

/// The current branch's pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub state: PrState,
    pub url: String,
    pub head_ref: String,
    pub is_draft: bool,
    pub review_decision: Option<ReviewDecision>,
    pub checks: Vec<Check>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPr {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    head_ref_name: String,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    review_decision: Option<String>,
    #[serde(default)]
    status_check_rollup: Option<Vec<GhCheck>>,
}

/// A `CheckRun` or a `StatusContext`; each fills its own fields.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhCheck {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    workflow_name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    details_url: Option<String>,
    #[serde(default)]
    target_url: Option<String>,
}

pub(super) fn view(root: &Path) -> Result<PullRequest> {
    let args = ["pr", "view", "--json", FIELDS];
    match gh::run_bounded(root, &args, None, GH_TIMEOUT, MAX_GH_OUTPUT) {
        Ok(out) => parse(&out),
        Err(GhError::Missing) => Err(CheckoutError::GhUnavailable),
        Err(GhError::Timeout) => Err(CheckoutError::Timeout),
        Err(GhError::Unauthenticated(message)) => Err(CheckoutError::GhUnauthenticated(message)),
        Err(GhError::Failed(message)) => Err(refusal(&message)),
    }
}

/// `gh`'s own refusal; "no pull request" is its own answer.
fn refusal(message: &str) -> CheckoutError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("no pull requests found") || lower.contains("no open pull requests") {
        CheckoutError::NoPullRequest
    } else {
        CheckoutError::Gh(message.to_owned())
    }
}

pub(super) fn parse(json: &[u8]) -> Result<PullRequest> {
    let pr: GhPr = serde_json::from_slice(json)
        .map_err(|error| CheckoutError::Gh(format!("gh answered unreadable JSON: {error}")))?;
    Ok(PullRequest {
        number: pr.number,
        title: pr.title,
        state: match pr.state.as_str() {
            "OPEN" => PrState::Open,
            "CLOSED" => PrState::Closed,
            "MERGED" => PrState::Merged,
            _ => PrState::Unknown,
        },
        url: pr.url,
        head_ref: pr.head_ref_name,
        is_draft: pr.is_draft,
        review_decision: pr
            .review_decision
            .filter(|decision| !decision.is_empty())
            .map(|decision| match decision.as_str() {
                "APPROVED" => ReviewDecision::Approved,
                "CHANGES_REQUESTED" => ReviewDecision::ChangesRequested,
                "REVIEW_REQUIRED" => ReviewDecision::ReviewRequired,
                _ => ReviewDecision::Unknown,
            }),
        checks: pr
            .status_check_rollup
            .unwrap_or_default()
            .into_iter()
            .map(check)
            .collect(),
    })
}

fn check(raw: GhCheck) -> Check {
    let state = match (&raw.state, &raw.status) {
        // A commit status carries `state` alone.
        (Some(state), None) => rollup_state(state),
        _ => check_run_state(raw.status.as_deref(), raw.conclusion.as_deref()),
    };
    Check {
        name: raw.name.or(raw.context).unwrap_or_default(),
        workflow: raw.workflow_name.filter(|name| !name.is_empty()),
        state,
        url: raw
            .details_url
            .or(raw.target_url)
            .filter(|url| !url.is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "number": 1678, "title": "docs: compare", "state": "OPEN",
      "url": "https://github.com/no-phux/phux/pull/1678",
      "headRefName": "docs/compare", "isDraft": false, "reviewDecision": "",
      "statusCheckRollup": [
        {"__typename":"CheckRun","name":"checks","workflowName":"ci","status":"COMPLETED","conclusion":"SUCCESS","detailsUrl":"https://x/1"},
        {"__typename":"CheckRun","name":"build","workflowName":"ci","status":"IN_PROGRESS","conclusion":"","detailsUrl":""},
        {"__typename":"CheckRun","name":"lint","status":"COMPLETED","conclusion":"TIMED_OUT"},
        {"__typename":"StatusContext","context":"mini","state":"PENDING","targetUrl":"https://x/2"},
        {"__typename":"StatusContext","context":"deploy","state":"ERROR"}
      ]
    }"#;

    #[test]
    fn gh_json_folds_check_runs_and_statuses() {
        let pr = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(pr.number, 1678);
        assert_eq!(pr.state, PrState::Open);
        assert_eq!(pr.head_ref, "docs/compare");
        assert_eq!(pr.review_decision, None);
        let states: Vec<_> = pr
            .checks
            .iter()
            .map(|c| (c.name.as_str(), c.state))
            .collect();
        assert_eq!(
            states,
            [
                ("checks", CheckState::Success),
                ("build", CheckState::Pending),
                ("lint", CheckState::Failure),
                ("mini", CheckState::Pending),
                ("deploy", CheckState::Failure),
            ]
        );
        assert_eq!(pr.checks[0].workflow.as_deref(), Some("ci"));
        assert_eq!(pr.checks[1].url, None);
        assert_eq!(pr.checks[3].url.as_deref(), Some("https://x/2"));
    }

    #[test]
    fn gh_failures_are_typed() {
        assert_eq!(
            refusal("no pull requests found for branch \"x\""),
            CheckoutError::NoPullRequest
        );
        assert!(matches!(
            gh::refusal("To get started with GitHub CLI, please run:  gh auth login\n"),
            GhError::Unauthenticated(_)
        ));
        assert!(matches!(refusal("boom"), CheckoutError::Gh(ref m) if m == "boom"));
    }
}
