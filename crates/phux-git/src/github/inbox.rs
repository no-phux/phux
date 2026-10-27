//! Open pull requests that want the viewer: ones they wrote (agents push as
//! the viewer), ones awaiting their review, and every open one in an account
//! they own or administer.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Value, json};

use super::gh::{self, GhError, Result};
use super::types::{CheckState, ReviewDecision};
use super::types::{Mergeability, rollup_state};

/// Most pull requests one search returns.
const PAGE: usize = 100;

const OWNERS: &str =
    "query { viewer { login organizations(first: 100) { nodes { login viewerCanAdminister } } } }";

const QUERY: &str = r"
query($mine: String!, $asked: String!, $owned: String!, $first: Int!) {
  mine: search(type: ISSUE, query: $mine, first: $first) { issueCount nodes { ...pr } }
  asked: search(type: ISSUE, query: $asked, first: $first) { issueCount nodes { ...pr } }
  owned: search(type: ISSUE, query: $owned, first: $first) { issueCount nodes { ...pr } }
}
fragment pr on PullRequest {
  id number title url isDraft updatedAt createdAt
  author { login }
  repository { nameWithOwner }
  headRefName baseRefName headRefOid
  additions deletions changedFiles
  mergeable reviewDecision viewerDidAuthor totalCommentsCount
  reviewThreads(first: 100) { nodes { isResolved } }
  commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
}";

/// Why a pull request is in the inbox; more than one can hold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reasons {
    /// The viewer opened it, or an agent pushing as them did.
    pub authored: bool,
    /// A review is requested of the viewer or their team.
    pub requested: bool,
    /// It is in an account the viewer owns or administers.
    pub owned: bool,
}

/// One open pull request, as the inbox lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboxPull {
    pub id: String,
    /// `owner/name`.
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub is_draft: bool,
    /// RFC 3339, as GitHub wrote it.
    pub updated_at: String,
    pub created_at: String,
    pub head_ref: String,
    pub base_ref: String,
    pub head_oid: String,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub mergeable: Mergeability,
    pub review_decision: Option<ReviewDecision>,
    /// The head commit's combined checks, when it has any.
    pub checks: Option<CheckState>,
    pub viewer_authored: bool,
    pub comments: u64,
    pub unresolved_threads: u64,
    pub reasons: Reasons,
}

/// Everything the inbox shows, newest activity first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inbox {
    pub pulls: Vec<InboxPull>,
    /// A search matched more than one page; the oldest were left out.
    pub truncated: bool,
}

/// Who the viewer is: their login, then every organization they administer.
pub fn owners() -> Result<Vec<String>> {
    let data = gh::graphql(OWNERS, &json!({}))?;
    let viewer = &data["viewer"];
    let login = viewer["login"]
        .as_str()
        .ok_or_else(|| GhError::Failed("GitHub did not say who is signed in.".into()))?;
    let orgs = viewer["organizations"]["nodes"]
        .as_array()
        .into_iter()
        .flatten();
    let admin = orgs
        .filter(|org| org["viewerCanAdminister"].as_bool() == Some(true))
        .filter_map(|org| org["login"].as_str());
    Ok(std::iter::once(login)
        .chain(admin)
        .map(str::to_owned)
        .collect())
}

/// The three searches for `owners` (the first is the viewer).
pub(crate) fn queries(owners: &[String]) -> [String; 3] {
    const OPEN: &str = "is:pr is:open archived:false sort:updated-desc";
    let accounts: Vec<String> = owners
        .iter()
        .enumerate()
        .map(|(ix, owner)| {
            if ix == 0 {
                format!("user:{owner}")
            } else {
                format!("org:{owner}")
            }
        })
        .collect();
    [
        format!("{OPEN} author:@me"),
        format!("{OPEN} review-requested:@me"),
        format!("{OPEN} {}", accounts.join(" ")),
    ]
}

/// Read the inbox for `owners`, as [`owners`] returned them.
pub fn fetch(owners: &[String]) -> Result<Inbox> {
    let [mine, asked, owned] = queries(owners);
    let owned = if owners.is_empty() {
        mine.clone()
    } else {
        owned
    };
    let variables = json!({ "mine": mine, "asked": asked, "owned": owned, "first": PAGE });
    parse(&gh::graphql(QUERY, &variables)?)
}

/// Records one search's reason on a pull request it found.
type Mark = fn(&mut Reasons);

#[derive(Deserialize)]
struct Search {
    #[serde(rename = "issueCount", default)]
    count: usize,
    #[serde(default)]
    nodes: Vec<Value>,
}

/// Merge the three searches into one list, by pull request.
pub(crate) fn parse(data: &Value) -> Result<Inbox> {
    let mut by_id: HashMap<String, InboxPull> = HashMap::new();
    let mut truncated = false;
    let buckets: [(&str, Mark); 3] = [
        ("mine", |reasons| reasons.authored = true),
        ("asked", |reasons| reasons.requested = true),
        ("owned", |reasons| reasons.owned = true),
    ];
    for (bucket, mark) in buckets {
        let search: Search = serde_json::from_value(data[bucket].clone()).map_err(|error| {
            GhError::Failed(format!("GitHub answered an unreadable search: {error}"))
        })?;
        truncated |= search.count > search.nodes.len();
        for node in &search.nodes {
            let Some(pull) = pull(node) else { continue };
            let entry = by_id.entry(pull.id.clone()).or_insert(pull);
            mark(&mut entry.reasons);
        }
    }
    let mut pulls: Vec<InboxPull> = by_id.into_values().collect();
    pulls.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
    Ok(Inbox { pulls, truncated })
}

fn text(node: &Value, key: &str) -> String {
    node[key].as_str().unwrap_or_default().to_owned()
}

fn count(node: &Value, key: &str) -> u64 {
    node[key].as_u64().unwrap_or_default()
}

pub(crate) fn review_decision(raw: Option<&str>) -> Option<ReviewDecision> {
    match raw? {
        "" => None,
        "APPROVED" => Some(ReviewDecision::Approved),
        "CHANGES_REQUESTED" => Some(ReviewDecision::ChangesRequested),
        "REVIEW_REQUIRED" => Some(ReviewDecision::ReviewRequired),
        _ => Some(ReviewDecision::Unknown),
    }
}

/// One search node; anything but a pull request (an issue) is skipped.
fn pull(node: &Value) -> Option<InboxPull> {
    let id = node["id"].as_str()?.to_owned();
    let number = node["number"].as_u64()?;
    let unresolved = node["reviewThreads"]["nodes"]
        .as_array()
        .map_or(0, |threads| {
            threads.iter().filter(|t| t["isResolved"] == false).count()
        });
    let rollup = &node["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"];
    Some(InboxPull {
        id,
        repo: node["repository"]["nameWithOwner"].as_str()?.to_owned(),
        number,
        title: text(node, "title"),
        url: text(node, "url"),
        author: node["author"]["login"]
            .as_str()
            .unwrap_or("ghost")
            .to_owned(),
        is_draft: node["isDraft"].as_bool().unwrap_or_default(),
        updated_at: text(node, "updatedAt"),
        created_at: text(node, "createdAt"),
        head_ref: text(node, "headRefName"),
        base_ref: text(node, "baseRefName"),
        head_oid: text(node, "headRefOid"),
        additions: count(node, "additions"),
        deletions: count(node, "deletions"),
        changed_files: count(node, "changedFiles"),
        mergeable: Mergeability::parse(node["mergeable"].as_str()),
        review_decision: review_decision(node["reviewDecision"].as_str()),
        checks: rollup.as_str().map(rollup_state),
        viewer_authored: node["viewerDidAuthor"].as_bool().unwrap_or_default(),
        comments: count(node, "totalCommentsCount"),
        unresolved_threads: unresolved as u64,
        reasons: Reasons::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, updated: &str) -> Value {
        json!({
            "id": id, "number": 7, "title": "t", "url": "u", "isDraft": false,
            "updatedAt": updated, "createdAt": "2026-01-01T00:00:00Z",
            "author": {"login": "bot"}, "repository": {"nameWithOwner": "o/r"},
            "headRefName": "feat", "baseRefName": "main", "headRefOid": "abc",
            "additions": 3, "deletions": 1, "changedFiles": 2,
            "mergeable": "CONFLICTING", "reviewDecision": null, "viewerDidAuthor": true,
            "totalCommentsCount": 4,
            "reviewThreads": {"nodes": [{"isResolved": true}, {"isResolved": false}]},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "FAILURE"}}}]}
        })
    }

    #[test]
    fn searches_merge_by_pull_request_and_keep_every_reason() {
        let data = json!({
            "mine": {"issueCount": 1, "nodes": [node("A", "2026-01-02T00:00:00Z")]},
            "asked": {"issueCount": 0, "nodes": []},
            "owned": {"issueCount": 4, "nodes": [node("B", "2026-01-03T00:00:00Z"), node("A", "2026-01-02T00:00:00Z"), {}]},
        });
        let inbox = parse(&data).unwrap();
        let ids: Vec<_> = inbox.pulls.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["B", "A"]);
        assert!(inbox.truncated);
        let a = &inbox.pulls[1];
        assert_eq!(
            a.reasons,
            Reasons {
                authored: true,
                requested: false,
                owned: true
            }
        );
        assert_eq!(a.unresolved_threads, 1);
        assert_eq!(a.checks, Some(CheckState::Failure));
        assert_eq!(a.mergeable, Mergeability::Conflicting);
        assert_eq!(a.review_decision, None);
    }

    #[test]
    fn owned_accounts_are_one_search() {
        let [mine, asked, owned] = queries(&["me".into(), "acme".into()]);
        assert!(mine.ends_with("author:@me"));
        assert!(asked.ends_with("review-requested:@me"));
        assert!(owned.ends_with("user:me org:acme"));
    }
}
