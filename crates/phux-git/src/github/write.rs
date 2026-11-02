//! The writes a reviewer makes: a review with its line comments in one
//! submission, thread replies and resolution, viewed marks, and merges.

use serde_json::{Value, json};

use super::gh::{self, GhError, Result};
use super::types::{LineSide, MergeMethod};

/// A review's verdict.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReviewEvent {
    #[default]
    Comment,
    Approve,
    RequestChanges,
}

impl ReviewEvent {
    const fn wire(self) -> &'static str {
        match self {
            Self::Comment => "COMMENT",
            Self::Approve => "APPROVE",
            Self::RequestChanges => "REQUEST_CHANGES",
        }
    }
}

/// A new line comment, sent as a thread of the review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewThread {
    pub path: String,
    pub side: LineSide,
    pub line: u32,
    /// The first line of a multi-line comment; it must come before `line`.
    pub start: Option<(LineSide, u32)>,
    pub body: String,
}

impl NewThread {
    fn wire(&self) -> Value {
        let mut thread = json!({
            "path": self.path,
            "side": self.side.wire(),
            "line": self.line,
            "body": self.body,
        });
        if let Some((side, line)) = self.start.filter(|start| *start != (self.side, self.line)) {
            thread["startSide"] = json!(side.wire());
            thread["startLine"] = json!(line);
        }
        thread
    }
}

fn mutate(document: &str, input: &Value) -> Result<Value> {
    gh::graphql(document, &json!({ "input": input }))
}

/// Submit one review on `pull_id` at `head_oid`: its verdict, summary and
/// line comments land together, as one notification.
pub fn submit_review(
    pull_id: &str,
    head_oid: &str,
    event: ReviewEvent,
    body: &str,
    threads: &[NewThread],
) -> Result<()> {
    if event == ReviewEvent::Comment && body.trim().is_empty() && threads.is_empty() {
        return Err(GhError::Failed(
            "A comment review needs a summary or a line comment.".into(),
        ));
    }
    let threads: Vec<Value> = threads.iter().map(NewThread::wire).collect();
    let input = json!({
        "pullRequestId": pull_id,
        "commitOID": head_oid,
        "event": event.wire(),
        "body": body,
        "threads": threads,
    });
    mutate(
        "mutation($input: AddPullRequestReviewInput!) { addPullRequestReview(input: $input) { pullRequestReview { id } } }",
        &input,
    )
    .map(drop)
}

/// Reply on a thread, at once.
pub fn reply(thread_id: &str, body: &str) -> Result<()> {
    mutate(
        "mutation($input: AddPullRequestReviewThreadReplyInput!) { addPullRequestReviewThreadReply(input: $input) { comment { id } } }",
        &json!({ "pullRequestReviewThreadId": thread_id, "body": body }),
    )
    .map(drop)
}

/// Resolve a thread, or reopen it.
pub fn set_resolved(thread_id: &str, resolved: bool) -> Result<()> {
    let document = if resolved {
        "mutation($input: ResolveReviewThreadInput!) { resolveReviewThread(input: $input) { thread { id } } }"
    } else {
        "mutation($input: UnresolveReviewThreadInput!) { unresolveReviewThread(input: $input) { thread { id } } }"
    };
    mutate(document, &json!({ "threadId": thread_id })).map(drop)
}

/// Tick or clear the viewer's "Viewed" box on one file.
pub fn set_viewed(pull_id: &str, path: &str, viewed: bool) -> Result<()> {
    let document = if viewed {
        "mutation($input: MarkFileAsViewedInput!) { markFileAsViewed(input: $input) { clientMutationId } }"
    } else {
        "mutation($input: UnmarkFileAsViewedInput!) { unmarkFileAsViewed(input: $input) { clientMutationId } }"
    };
    mutate(document, &json!({ "pullRequestId": pull_id, "path": path })).map(drop)
}

/// Merge now. `head_oid` guards against a push the reviewer has not seen.
pub fn merge(pull_id: &str, method: MergeMethod, head_oid: &str) -> Result<()> {
    mutate(
        "mutation($input: MergePullRequestInput!) { mergePullRequest(input: $input) { pullRequest { state } } }",
        &json!({ "pullRequestId": pull_id, "mergeMethod": method.wire(), "expectedHeadOid": head_oid }),
    )
    .map(drop)
}

/// Merge once required checks and reviews pass, or stop waiting to.
///
/// Enabling follows future eligible revisions too: GitHub offers no expected
/// head or base input here. Callers must revalidate the reviewed range before
/// enabling; this is not an atomic SHA-pinned merge.
pub fn set_auto_merge(pull_id: &str, method: Option<MergeMethod>) -> Result<()> {
    let (document, input) = method.map_or_else(
        || {
            let input = json!({ "pullRequestId": pull_id });
            ("mutation($input: DisablePullRequestAutoMergeInput!) { disablePullRequestAutoMerge(input: $input) { clientMutationId } }", input)
        },
        |method| {
            let input = json!({ "pullRequestId": pull_id, "mergeMethod": method.wire() });
            ("mutation($input: EnablePullRequestAutoMergeInput!) { enablePullRequestAutoMerge(input: $input) { clientMutationId } }", input)
        },
    );
    mutate(document, &input).map(drop)
}

/// Take a draft out of draft.
pub fn ready_for_review(pull_id: &str) -> Result<()> {
    mutate(
        "mutation($input: MarkPullRequestReadyForReviewInput!) { markPullRequestReadyForReview(input: $input) { clientMutationId } }",
        &json!({ "pullRequestId": pull_id }),
    )
    .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_line_comment_names_no_start() {
        let thread = NewThread {
            path: "a.rs".into(),
            side: LineSide::Right,
            line: 4,
            start: Some((LineSide::Right, 4)),
            body: "x".into(),
        };
        assert_eq!(
            thread.wire(),
            json!({"path": "a.rs", "side": "RIGHT", "line": 4, "body": "x"})
        );
    }

    #[test]
    fn a_range_names_its_start_side() {
        let thread = NewThread {
            path: "a.rs".into(),
            side: LineSide::Right,
            line: 9,
            start: Some((LineSide::Left, 7)),
            body: "x".into(),
        };
        let wire = thread.wire();
        assert_eq!(
            (wire["startSide"].as_str(), wire["startLine"].as_u64()),
            (Some("LEFT"), Some(7))
        );
    }

    #[test]
    fn an_empty_comment_review_is_refused_before_the_network() {
        assert!(submit_review("PR", "h", ReviewEvent::Comment, " ", &[]).is_err());
    }
}
