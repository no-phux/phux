//! One bounded `gh` invocation.
//!
//! `gh` owns authentication, hosts and GitHub Enterprise; a missing or
//! signed-out `gh` is a typed refusal. A server or app started outside a login
//! shell has a narrower `PATH`, so the usual install locations are tried too.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use super::process::{self, Child, Failure};

pub(crate) const ENV: [(&str, &str); 5] = [
    ("GH_PROMPT_DISABLED", "1"),
    ("GH_NO_UPDATE_NOTIFIER", "1"),
    ("GH_SPINNER_DISABLED", "1"),
    ("NO_COLOR", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
];
const FALLBACK: [&str; 3] = ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"];

/// The longest one GitHub call may run; search can take several seconds.
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// Most bytes one answer may carry.
pub(crate) const MAX_OUTPUT: usize = 32 * 1024 * 1024;

/// Why a GitHub call produced nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GhError {
    /// `gh` is not installed.
    Missing,
    /// `gh` is installed but signed out.
    Unauthenticated(String),
    Timeout,
    /// GitHub or `gh` refused; the message says why.
    Failed(String),
}

impl std::fmt::Display for GhError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => f.write_str(
                "The GitHub CLI (gh) is not installed. Install it, then run `gh auth login`.",
            ),
            Self::Unauthenticated(_) => {
                f.write_str("The GitHub CLI is signed out. Run `gh auth login` in a terminal.")
            }
            Self::Timeout => f.write_str("GitHub did not answer in time."),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for GhError {}

pub type Result<T> = std::result::Result<T, GhError>;

/// Run `gh args…` in `dir`, writing `stdin`, and return its stdout.
pub(crate) fn run(dir: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
    run_bounded(dir, args, stdin, TIMEOUT, MAX_OUTPUT)
}

/// [`run`] under the caller's own deadline and output cap.
pub(crate) fn run_bounded(
    dir: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
    timeout: Duration,
    max_stdout: usize,
) -> Result<Vec<u8>> {
    let mut last = Err(GhError::Missing);
    for program in std::iter::once("gh").chain(FALLBACK) {
        let run = process::exec(Child {
            program,
            dir,
            args,
            env: &ENV,
            stdin: stdin.map(<[u8]>::to_vec),
            timeout,
            max_stdout,
        });
        last = match run {
            Err(Failure::NotFound) => continue,
            Err(Failure::Spawn(error)) => {
                Err(GhError::Failed(format!("Could not run gh: {error}")))
            }
            Err(Failure::Timeout) => Err(GhError::Timeout),
            Err(Failure::Wait(message)) => Err(GhError::Failed(message)),
            Ok(out) if out.truncated => Err(GhError::Failed(
                "GitHub answered more than the app reads.".into(),
            )),
            Ok(out) if out.success => Ok(out.stdout),
            Ok(out) => Err(graphql_errors(&out.stdout).unwrap_or_else(|| refusal(&out.stderr))),
        };
        break;
    }
    last
}

/// POST one GraphQL document and return its `data`.
pub(crate) fn graphql(query: &str, variables: &Value) -> Result<Value> {
    let body = serde_json::json!({ "query": query, "variables": variables });
    let body = serde_json::to_vec(&body).map_err(|error| GhError::Failed(error.to_string()))?;
    let out = run(
        &std::env::temp_dir(),
        &["api", "graphql", "--input", "-"],
        Some(&body),
    )?;
    if let Some(error) = graphql_errors(&out) {
        return Err(error);
    }
    let mut answer: Value = serde_json::from_slice(&out)
        .map_err(|error| GhError::Failed(format!("GitHub answered unreadable JSON: {error}")))?;
    Ok(answer.get_mut("data").map_or(Value::Null, Value::take))
}

/// A GraphQL answer's `errors`, joined, when it has any.
fn graphql_errors(out: &[u8]) -> Option<GhError> {
    let answer: Value = serde_json::from_slice(out).ok()?;
    let errors = answer.get("errors")?.as_array()?;
    let messages: Vec<&str> = errors
        .iter()
        .filter_map(|error| error.get("message")?.as_str())
        .collect();
    (!messages.is_empty()).then(|| GhError::Failed(messages.join("\n")))
}

pub(crate) fn refusal(stderr: &str) -> GhError {
    let lower = stderr.to_ascii_lowercase();
    let signed_out = [
        "gh auth login",
        "not logged in",
        "authentication",
        "http 401",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if signed_out {
        GhError::Unauthenticated(crate::failure(stderr, ""))
    } else {
        GhError::Failed(crate::failure(stderr, ""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphql_errors_are_joined() {
        let out = br#"{"data":null,"errors":[{"message":"one"},{"message":"two"}]}"#;
        assert_eq!(
            graphql_errors(out),
            Some(GhError::Failed("one\ntwo".into()))
        );
        assert_eq!(graphql_errors(br#"{"data":{}}"#), None);
        assert_eq!(graphql_errors(b"not json"), None);
    }

    #[test]
    fn a_signed_out_gh_is_typed() {
        assert!(matches!(
            refusal("please run: gh auth login\n"),
            GhError::Unauthenticated(_)
        ));
        assert_eq!(refusal("boom\n"), GhError::Failed("boom".into()));
    }
}
