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

/// GitHub refuses a search query longer than this.
const MAX_QUERY: usize = 256;
/// Searches for owned accounts, at most; more accounts are left out.
const MAX_OWNED_SEARCHES: usize = 6;

const FRAGMENT: &str = r"
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

/// Which reason a search records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Authored,
    Requested,
    Owned,
}

/// One aliased search of the inbox document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Search {
    pub alias: String,
    pub query: String,
    pub kind: Kind,
}

/// The inbox's searches for `owners` (the first is the viewer): authored,
/// requested, then the owned accounts split so each query fits GitHub's limit.
/// The flag says accounts were left out.
pub(crate) fn searches(owners: &[String]) -> (Vec<Search>, bool) {
    const OPEN: &str = "is:pr is:open archived:false sort:updated-desc";
    let search = |alias: String, query: String, kind| Search { alias, query, kind };
    let mut searches = vec![
        search("mine".into(), format!("{OPEN} author:@me"), Kind::Authored),
        search(
            "asked".into(),
            format!("{OPEN} review-requested:@me"),
            Kind::Requested,
        ),
    ];
    let mut query = String::new();
    let mut chunks = Vec::new();
    for (ix, owner) in owners.iter().enumerate() {
        let account = if ix == 0 {
            format!("user:{owner}")
        } else {
            format!("org:{owner}")
        };
        if !query.is_empty() && OPEN.len() + query.len() + 1 + account.len() + 1 > MAX_QUERY {
            chunks.push(std::mem::take(&mut query));
        }
        if !query.is_empty() {
            query.push(' ');
        }
        query.push_str(&account);
    }
    if !query.is_empty() {
        chunks.push(query);
    }
    let left_out = chunks.len() > MAX_OWNED_SEARCHES;
    for (ix, accounts) in chunks.into_iter().take(MAX_OWNED_SEARCHES).enumerate() {
        searches.push(search(
            format!("owned{ix}"),
            format!("{OPEN} {accounts}"),
            Kind::Owned,
        ));
    }
    (searches, left_out)
}

/// One GraphQL document asking every search at once.
fn document(searches: &[Search]) -> String {
    let variables: Vec<String> = searches
        .iter()
        .map(|s| format!("${}: String!", s.alias))
        .collect();
    let fields: Vec<String> = searches
        .iter()
        .map(|s| {
            format!(
                "  {0}: search(type: ISSUE, query: ${0}, first: $first) {{ issueCount nodes {{ ...pr }} }}",
                s.alias
            )
        })
        .collect();
    format!(
        "query({}, $first: Int!) {{\n{}\n}}{FRAGMENT}",
        variables.join(", "),
        fields.join("\n")
    )
}

/// Read the inbox for `owners`, as [`owners`] returned them.
pub fn fetch(owners: &[String]) -> Result<Inbox> {
    let (searches, left_out) = searches(owners);
    let mut variables = serde_json::Map::new();
    for search in &searches {
        variables.insert(search.alias.clone(), Value::String(search.query.clone()));
    }
    variables.insert("first".into(), json!(PAGE));
    let data = gh::graphql(&document(&searches), &Value::Object(variables))?;
    let mut inbox = parse(&data, &searches)?;
    inbox.truncated |= left_out;
    Ok(inbox)
}

#[derive(Deserialize)]
struct Found {
    #[serde(rename = "issueCount", default)]
    count: usize,
    #[serde(default)]
    nodes: Vec<Value>,
}

/// Merge the searches into one list, by pull request.
pub(crate) fn parse(data: &Value, searches: &[Search]) -> Result<Inbox> {
    let mut by_id: HashMap<String, InboxPull> = HashMap::new();
    let mut truncated = false;
    for search in searches {
        let found: Found =
            serde_json::from_value(data[&search.alias].clone()).map_err(|error| {
                GhError::Failed(format!("GitHub answered an unreadable search: {error}"))
            })?;
        truncated |= found.count > found.nodes.len();
        for node in &found.nodes {
            let Some(pull) = pull(node) else { continue };
            let reasons = &mut by_id.entry(pull.id.clone()).or_insert(pull).reasons;
            match search.kind {
                Kind::Authored => reasons.authored = true,
                Kind::Requested => reasons.requested = true,
                Kind::Owned => reasons.owned = true,
            }
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
            "owned0": {"issueCount": 4, "nodes": [node("B", "2026-01-03T00:00:00Z"), node("A", "2026-01-02T00:00:00Z"), {}]},
        });
        let (searches, _) = searches(&["me".into()]);
        let inbox = parse(&data, &searches).unwrap();
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
    fn owned_accounts_share_searches_that_fit_githubs_limit() {
        let (few, left_out) = searches(&["me".into(), "acme".into()]);
        let queries: Vec<_> = few.iter().map(|s| (s.alias.as_str(), s.kind)).collect();
        assert_eq!(
            queries,
            [
                ("mine", Kind::Authored),
                ("asked", Kind::Requested),
                ("owned0", Kind::Owned)
            ]
        );
        assert!(few[2].query.ends_with("user:me org:acme") && !left_out);

        let many: Vec<String> = (0..40)
            .map(|n| format!("organization-number-{n:02}"))
            .collect();
        let (split, _) = searches(&many);
        assert!(split.len() > 3);
        assert!(
            split.iter().all(|s| s.query.len() <= MAX_QUERY),
            "{split:?}"
        );
        let named = split
            .iter()
            .filter(|s| s.kind == Kind::Owned)
            .map(|s| s.query.matches(":organization").count())
            .sum::<usize>();
        assert_eq!(named, 40);

        let (_, left_out) = searches(
            &(0..400)
                .map(|n| format!("org{n:03}-long-name-here"))
                .collect::<Vec<_>>(),
        );
        assert!(left_out);
    }

    #[test]
    fn the_document_asks_every_search() {
        let (searches, _) = searches(&["me".into()]);
        let document = document(&searches);
        assert!(document.starts_with(
            "query($mine: String!, $asked: String!, $owned0: String!, $first: Int!) {"
        ));
        assert!(document.contains("owned0: search(type: ISSUE, query: $owned0, first: $first)"));
        assert!(document.contains("fragment pr on PullRequest"));
    }
}
