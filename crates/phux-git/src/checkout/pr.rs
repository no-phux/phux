//! The current branch's pull request through the host's GitHub CLI.
//!
//! `gh` owns authentication, the remote-to-repository mapping, and GitHub
//! Enterprise hosts, so the server needs no token of its own. A host
//! without `gh`, or with `gh` signed out, is a typed refusal rather than a
//! failure.

use std::path::Path;

use serde::Deserialize;

use super::{CheckoutError, GH_TIMEOUT, Result};
use crate::bounded::{Run, RunError, Stop};

const MAX_GH_OUTPUT: usize = 4 * 1024 * 1024;
const FIELDS: &str = "number,title,state,url,headRefName,isDraft,reviewDecision,statusCheckRollup";
const GH_ENV: [(&str, &str); 5] = [
    ("GH_PROMPT_DISABLED", "1"),
    ("GH_NO_UPDATE_NOTIFIER", "1"),
    ("GH_SPINNER_DISABLED", "1"),
    ("NO_COLOR", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
];
/// A server started outside a login shell has a narrower `PATH`.
const GH_FALLBACK: [&str; 3] = ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"];

/// A pull request's state; GitHub values are read leniently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrState {
    Open,
    Closed,
    Merged,
    Unknown,
}

/// A pull request's review decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
    Unknown,
}

/// One check's state, folded from check runs and commit statuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckState {
    Pending,
    Success,
    Failure,
    Neutral,
    Skipped,
    Cancelled,
    Unknown,
}

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
    for program in std::iter::once("gh").chain(GH_FALLBACK) {
        let run = Run {
            program,
            dir: root,
            args: &args,
            env: &GH_ENV,
            timeout: GH_TIMEOUT,
            max_stdout: MAX_GH_OUTPUT,
            stdin: None,
            stop: Stop::Kill,
        }
        .exec();
        return match run {
            Err(RunError::Spawn(error)) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(RunError::Spawn(error)) => {
                Err(CheckoutError::Gh(format!("Could not run gh: {error}")))
            }
            Err(RunError::Timeout) => Err(CheckoutError::Timeout),
            Err(RunError::Wait(message)) => Err(refusal(&message)),
            Ok(out) if out.truncated => Err(CheckoutError::Gh(
                "GitHub answered more than the app reads.".into(),
            )),
            Ok(out) if out.status.success() => parse(&out.stdout),
            Ok(out) => Err(graphql_errors(&out.stdout)
                .map_or_else(|| gh_refusal(&out.stderr), |message| refusal(&message))),
        };
    }
    Err(CheckoutError::GhUnavailable)
}

/// A GraphQL answer's `errors`, joined, when it has any.
fn graphql_errors(out: &[u8]) -> Option<String> {
    let answer: serde_json::Value = serde_json::from_slice(out).ok()?;
    let messages: Vec<&str> = answer
        .get("errors")?
        .as_array()?
        .iter()
        .filter_map(|error| error.get("message")?.as_str())
        .collect();
    (!messages.is_empty()).then(|| messages.join("\n"))
}

/// A failed `gh`'s stderr: signed out, or [`refusal`].
fn gh_refusal(stderr: &str) -> CheckoutError {
    let lower = stderr.to_ascii_lowercase();
    let message = crate::status::failure(stderr, "");
    let signed_out = [
        "gh auth login",
        "not logged in",
        "authentication",
        "http 401",
    ];
    if signed_out.iter().any(|needle| lower.contains(needle)) {
        CheckoutError::GhUnauthenticated(message)
    } else {
        refusal(&message)
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

/// A commit's combined status (`StatusState`).
fn rollup_state(raw: &str) -> CheckState {
    match raw {
        "SUCCESS" => CheckState::Success,
        "PENDING" | "EXPECTED" => CheckState::Pending,
        "FAILURE" | "ERROR" => CheckState::Failure,
        _ => CheckState::Unknown,
    }
}

/// One check run's state from its `status` and `conclusion`.
fn check_run_state(status: Option<&str>, conclusion: Option<&str>) -> CheckState {
    if status.is_some_and(|status| status != "COMPLETED") {
        return CheckState::Pending;
    }
    match conclusion.unwrap_or_default() {
        "SUCCESS" => CheckState::Success,
        "FAILURE" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED" => CheckState::Failure,
        "NEUTRAL" => CheckState::Neutral,
        "SKIPPED" => CheckState::Skipped,
        "CANCELLED" => CheckState::Cancelled,
        _ => CheckState::Unknown,
    }
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
            gh_refusal("To get started with GitHub CLI, please run:  gh auth login\n"),
            CheckoutError::GhUnauthenticated(_)
        ));
        assert!(matches!(gh_refusal("boom\n"), CheckoutError::Gh(ref m) if m == "boom"));
        let out = br#"{"data":null,"errors":[{"message":"one"},{"message":"two"}]}"#;
        assert_eq!(graphql_errors(out).as_deref(), Some("one\ntwo"));
        assert_eq!(graphql_errors(b"not json"), None);
    }
}
