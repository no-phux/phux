//! One pull request in full: its files and viewed marks, review threads, the
//! conversation, the head commit's checks, and how the repository merges.

use serde_json::{Value, json};

use super::gh::{self, GhError, Result};
use super::inbox::review_decision;
use super::types::{CheckState, PrState, ReviewDecision};
use super::types::{
    LineSide, MergeMethod, MergeMethods, MergeState, Mergeability, ViewedState, check_run_state,
    rollup_state,
};

/// GitHub lists at most 3000 files of a pull request.
const MAX_FILES: usize = 3000;

const QUERY: &str = r"
query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed autoMergeAllowed deleteBranchOnMerge viewerDefaultMergeMethod
    pullRequest(number: $number) {
      id number title url body isDraft state createdAt updatedAt
      author { login }
      headRefName baseRefName headRefOid baseRefOid isCrossRepository
      headRepository { nameWithOwner }
      mergeable mergeStateStatus reviewDecision viewerDidAuthor viewerCanUpdate
      autoMergeRequest { mergeMethod }
      additions deletions changedFiles
      files(first: 100) { pageInfo { hasNextPage endCursor } nodes { ...file } }
      reviewThreads(first: 100) { nodes {
        id isResolved isOutdated path line originalLine startLine originalStartLine diffSide startDiffSide
        viewerCanResolve viewerCanUnresolve viewerCanReply
        comments(first: 100) { nodes { id author { login } body createdAt } }
      } }
      comments(last: 100) { nodes { id author { login } body createdAt } }
      reviews(last: 100) { nodes { id author { login } state body submittedAt } }
      commits(last: 1) { nodes { commit { oid statusCheckRollup { state contexts(first: 100) { nodes {
        __typename
        ... on CheckRun { databaseId name status conclusion detailsUrl checkSuite { workflowRun { databaseId workflow { name } } } }
        ... on StatusContext { context state targetUrl description }
      } } } } } }
    }
  }
}
fragment file on PullRequestChangedFile { path additions deletions viewerViewedState }";

const FILES: &str = r"
query($owner: String!, $name: String!, $number: Int!, $after: String!) {
  repository(owner: $owner, name: $name) { pullRequest(number: $number) {
    files(first: 100, after: $after) { pageInfo { hasNextPage endCursor } nodes { path additions deletions viewerViewedState } }
  } }
}";

/// One changed file and the viewer's mark on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    pub viewed: ViewedState,
}

/// One comment, in a thread or the conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub id: String,
    pub author: String,
    pub body: String,
    pub created_at: String,
}

/// A review thread on some lines of one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread {
    pub id: String,
    pub path: String,
    pub resolved: bool,
    /// The lines changed since; `line` is then `None` and `original_line` is
    /// where it was.
    pub outdated: bool,
    pub side: LineSide,
    pub line: Option<u32>,
    pub original_line: Option<u32>,
    /// The first line of a multi-line thread.
    pub start: Option<(LineSide, u32)>,
    pub rights: Rights,
    pub comments: Vec<Note>,
}

/// What the viewer may do to a thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rights {
    pub resolve: bool,
    pub unresolve: bool,
    pub reply: bool,
}

/// One item of the conversation tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Comment(Note),
    /// A submitted review: its verdict (`APPROVED`, `CHANGES_REQUESTED`,
    /// `COMMENTED`, `DISMISSED`) and summary.
    Review {
        note: Note,
        state: String,
    },
}

impl Entry {
    #[must_use]
    pub const fn note(&self) -> &Note {
        match self {
            Self::Comment(note) | Self::Review { note, .. } => note,
        }
    }
}

/// One check or commit status on the head commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CiCheck {
    pub name: String,
    pub workflow: Option<String>,
    pub state: CheckState,
    pub url: Option<String>,
    /// An Actions job's id, whose log can be read.
    pub job: Option<u64>,
    pub description: Option<String>,
}

/// One pull request in full.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pull {
    pub id: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub body: String,
    pub author: String,
    pub state: PrState,
    pub is_draft: bool,
    pub created_at: String,
    pub updated_at: String,
    pub head_ref: String,
    pub base_ref: String,
    pub head_oid: String,
    pub base_oid: String,
    /// `owner/name` of the head when it is a fork.
    pub head_repo: Option<String>,
    pub mergeable: Mergeability,
    pub merge_state: MergeState,
    pub review_decision: Option<ReviewDecision>,
    pub viewer_authored: bool,
    pub viewer_can_update: bool,
    pub auto_merge: Option<MergeMethod>,
    pub methods: MergeMethods,
    pub additions: u64,
    pub deletions: u64,
    pub files: Vec<PullFile>,
    pub threads: Vec<Thread>,
    /// Comments and submitted reviews, oldest first.
    pub conversation: Vec<Entry>,
    pub checks: Vec<CiCheck>,
    pub rollup: Option<CheckState>,
}

impl Pull {
    /// Whether the head lives in another repository.
    #[must_use]
    pub fn is_fork(&self) -> bool {
        self.head_repo
            .as_deref()
            .is_some_and(|head| head != self.repo)
    }
}

/// Read `repo#number`, every file page included.
pub fn fetch(repo: &str, number: u64) -> Result<Pull> {
    let (owner, name) = super::split_repo(repo)
        .ok_or_else(|| GhError::Failed(format!("{repo} is not an owner/name repository.")))?;
    let variables = json!({ "owner": owner, "name": name, "number": number });
    let data = gh::graphql(QUERY, &variables)?;
    let mut pull = parse(repo, &data)?;
    let mut page = data["repository"]["pullRequest"]["files"]["pageInfo"].clone();
    while page["hasNextPage"] == true && pull.files.len() < MAX_FILES {
        let mut variables = variables.clone();
        variables["after"] = page["endCursor"].clone();
        let data = gh::graphql(FILES, &variables)?;
        let files = &data["repository"]["pullRequest"]["files"];
        pull.files.extend(
            files["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(file),
        );
        page = files["pageInfo"].clone();
    }
    Ok(pull)
}

fn text(node: &Value, key: &str) -> String {
    node[key].as_str().unwrap_or_default().to_owned()
}

fn login(node: &Value) -> String {
    node["author"]["login"]
        .as_str()
        .unwrap_or("ghost")
        .to_owned()
}

fn line(node: &Value, key: &str) -> Option<u32> {
    node[key].as_u64().and_then(|line| u32::try_from(line).ok())
}

fn nodes<'a>(node: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    node[key]["nodes"].as_array().into_iter().flatten()
}

pub(crate) fn parse(repo: &str, data: &Value) -> Result<Pull> {
    let repository = &data["repository"];
    let node = &repository["pullRequest"];
    if !node.is_object() {
        return Err(GhError::Failed(format!("{repo} has no such pull request.")));
    }
    let commit = &node["commits"]["nodes"][0]["commit"];
    let rollup = &commit["statusCheckRollup"];
    Ok(Pull {
        id: text(node, "id"),
        repo: repo.to_owned(),
        number: node["number"].as_u64().unwrap_or_default(),
        title: text(node, "title"),
        url: text(node, "url"),
        body: text(node, "body"),
        author: login(node),
        state: match node["state"].as_str() {
            Some("OPEN") => PrState::Open,
            Some("CLOSED") => PrState::Closed,
            Some("MERGED") => PrState::Merged,
            _ => PrState::Unknown,
        },
        is_draft: node["isDraft"] == true,
        created_at: text(node, "createdAt"),
        updated_at: text(node, "updatedAt"),
        head_ref: text(node, "headRefName"),
        base_ref: text(node, "baseRefName"),
        head_oid: text(node, "headRefOid"),
        base_oid: text(node, "baseRefOid"),
        head_repo: node["headRepository"]["nameWithOwner"]
            .as_str()
            .map(str::to_owned),
        mergeable: Mergeability::parse(node["mergeable"].as_str()),
        merge_state: MergeState::parse(node["mergeStateStatus"].as_str()),
        review_decision: review_decision(node["reviewDecision"].as_str()),
        viewer_authored: node["viewerDidAuthor"] == true,
        viewer_can_update: node["viewerCanUpdate"] == true,
        auto_merge: MergeMethod::parse(node["autoMergeRequest"]["mergeMethod"].as_str()),
        methods: methods(repository),
        additions: node["additions"].as_u64().unwrap_or_default(),
        deletions: node["deletions"].as_u64().unwrap_or_default(),
        files: nodes(node, "files").filter_map(file).collect(),
        threads: nodes(node, "reviewThreads").filter_map(thread).collect(),
        conversation: conversation(node),
        checks: nodes(rollup, "contexts").map(check).collect(),
        rollup: rollup["state"].as_str().map(rollup_state),
    })
}

fn methods(repository: &Value) -> MergeMethods {
    let allowed: Vec<MergeMethod> = [
        ("mergeCommitAllowed", MergeMethod::Merge),
        ("squashMergeAllowed", MergeMethod::Squash),
        ("rebaseMergeAllowed", MergeMethod::Rebase),
    ]
    .into_iter()
    .filter(|(key, _)| repository[*key] == true)
    .map(|(_, method)| method)
    .collect();
    let preferred = MergeMethod::parse(repository["viewerDefaultMergeMethod"].as_str())
        .filter(|method| allowed.contains(method))
        .or_else(|| allowed.first().copied())
        .unwrap_or(MergeMethod::Merge);
    MergeMethods {
        allowed,
        preferred,
        auto_merge: repository["autoMergeAllowed"] == true,
        deletes_branch: repository["deleteBranchOnMerge"] == true,
    }
}

fn file(node: &Value) -> Option<PullFile> {
    Some(PullFile {
        path: node["path"].as_str()?.to_owned(),
        additions: node["additions"].as_u64().unwrap_or_default(),
        deletions: node["deletions"].as_u64().unwrap_or_default(),
        viewed: ViewedState::parse(node["viewerViewedState"].as_str()),
    })
}

fn note(node: &Value, at: &str) -> Note {
    Note {
        id: text(node, "id"),
        author: login(node),
        body: text(node, "body"),
        created_at: text(node, at),
    }
}

fn thread(node: &Value) -> Option<Thread> {
    let side = LineSide::parse(node["diffSide"].as_str());
    let start_side = node["startDiffSide"]
        .as_str()
        .map_or(side, |raw| LineSide::parse(Some(raw)));
    let start = line(node, "startLine").or_else(|| line(node, "originalStartLine"));
    Some(Thread {
        id: node["id"].as_str()?.to_owned(),
        path: node["path"].as_str()?.to_owned(),
        resolved: node["isResolved"] == true,
        outdated: node["isOutdated"] == true,
        side,
        line: line(node, "line"),
        original_line: line(node, "originalLine"),
        start: start.map(|start| (start_side, start)),
        rights: Rights {
            resolve: node["viewerCanResolve"] == true,
            unresolve: node["viewerCanUnresolve"] == true,
            reply: node["viewerCanReply"] == true,
        },
        comments: nodes(node, "comments")
            .map(|c| note(c, "createdAt"))
            .collect(),
    })
}

/// Comments and non-empty or decisive reviews, oldest first.
fn conversation(node: &Value) -> Vec<Entry> {
    let comments = nodes(node, "comments").map(|c| Entry::Comment(note(c, "createdAt")));
    let reviews = nodes(node, "reviews")
        .filter(|review| {
            let body = review["body"].as_str().unwrap_or_default();
            !body.trim().is_empty() || review["state"] != "COMMENTED"
        })
        .filter(|review| review["state"] != "PENDING")
        .map(|review| Entry::Review {
            note: note(review, "submittedAt"),
            state: text(review, "state"),
        });
    let mut entries: Vec<Entry> = comments.chain(reviews).collect();
    entries.sort_by(|a, b| a.note().created_at.cmp(&b.note().created_at));
    entries
}

fn check(node: &Value) -> CiCheck {
    let optional = |value: &Value| value.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    if node["__typename"] == "StatusContext" {
        return CiCheck {
            name: text(node, "context"),
            workflow: None,
            state: rollup_state(node["state"].as_str().unwrap_or_default()),
            url: optional(&node["targetUrl"]),
            job: None,
            description: optional(&node["description"]),
        };
    }
    let run = &node["checkSuite"]["workflowRun"];
    CiCheck {
        name: text(node, "name"),
        workflow: optional(&run["workflow"]["name"]),
        state: check_run_state(node["status"].as_str(), node["conclusion"].as_str()),
        url: optional(&node["detailsUrl"]),
        // Only an Actions run's check is a job whose log GitHub serves.
        job: run
            .is_object()
            .then(|| node["databaseId"].as_u64())
            .flatten(),
        description: None,
    }
}

#[cfg(test)]
mod tests;
