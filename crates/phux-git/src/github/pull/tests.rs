use super::*;

fn sample() -> Value {
    let mut pull = sample_pull();
    pull["reviewThreads"] = threads();
    pull["commits"] = commits();
    json!({ "repository": {
        "mergeCommitAllowed": false, "squashMergeAllowed": true, "rebaseMergeAllowed": true,
        "autoMergeAllowed": true, "deleteBranchOnMerge": true, "viewerDefaultMergeMethod": "MERGE",
        "pullRequest": pull
    }})
}

#[test]
fn a_pull_request_reads_whole() {
    let pull = parse("o/r", &sample()).unwrap();
    assert_eq!((pull.number, pull.author.as_str()), (12, "ghost"));
    assert!(pull.is_fork());
    assert_eq!(pull.merge_state, MergeState::Unstable);
    assert_eq!(pull.review_decision, Some(ReviewDecision::Approved));
    assert_eq!(pull.auto_merge, Some(MergeMethod::Squash));
    assert_eq!(
        pull.methods.allowed,
        [MergeMethod::Squash, MergeMethod::Rebase]
    );
    // The viewer's default is not allowed here, so the first allowed one leads.
    assert_eq!(pull.methods.preferred, MergeMethod::Squash);
    assert_eq!(
        pull.files.iter().map(|f| f.viewed).collect::<Vec<_>>(),
        [ViewedState::Dismissed, ViewedState::Viewed]
    );
    assert_eq!(pull.rollup, Some(CheckState::Failure));
}

#[test]
fn threads_keep_their_sides_and_outdated_lines() {
    let pull = parse("o/r", &sample()).unwrap();
    let [current, outdated] = &pull.threads[..] else {
        panic!("two threads")
    };
    assert_eq!(current.side, LineSide::Right);
    assert_eq!(current.line, Some(9));
    assert_eq!(current.start, Some((LineSide::Left, 7)));
    assert_eq!(
        current.rights,
        Rights {
            resolve: true,
            unresolve: false,
            reply: true
        }
    );
    assert_eq!(current.comments[0].author, "me");
    assert!(outdated.outdated && outdated.resolved);
    assert_eq!((outdated.line, outdated.original_line), (None, Some(3)));
    assert_eq!(outdated.start, None);
}

#[test]
fn the_conversation_drops_empty_comment_reviews_and_drafts() {
    let pull = parse("o/r", &sample()).unwrap();
    let ids: Vec<_> = pull
        .conversation
        .iter()
        .map(|e| e.note().id.as_str())
        .collect();
    assert_eq!(ids, ["R1", "I1"]);
    assert!(matches!(&pull.conversation[0], Entry::Review { state, .. } if state == "APPROVED"));
}

#[test]
fn only_actions_checks_have_a_job_log() {
    let pull = parse("o/r", &sample()).unwrap();
    let checks: Vec<_> = pull
        .checks
        .iter()
        .map(|c| (c.name.as_str(), c.state, c.job))
        .collect();
    assert_eq!(
        checks,
        [
            ("lint", CheckState::Failure, Some(77)),
            ("app", CheckState::Pending, None),
            ("deploy", CheckState::Success, None),
        ]
    );
    assert_eq!(pull.checks[0].workflow.as_deref(), Some("ci"));
    assert_eq!(pull.checks[1].url, None);
    assert_eq!(pull.checks[2].description.as_deref(), Some("ok"));
}

#[test]
fn a_deleted_fork_is_still_a_fork() {
    let mut data = sample();
    let pull_node = &mut data["repository"]["pullRequest"];
    pull_node["headRepository"] = Value::Null;
    pull_node["isCrossRepository"] = json!(false);
    assert!(parse("o/r", &data).unwrap().is_fork());
    let pull_node = &mut data["repository"]["pullRequest"];
    pull_node["headRepository"] = json!({"nameWithOwner": "o/r"});
    assert!(!parse("o/r", &data).unwrap().is_fork());
}

#[test]
fn a_missing_pull_request_is_refused() {
    let error = parse("o/r", &json!({"repository": {"pullRequest": null}})).unwrap_err();
    assert_eq!(
        error,
        GhError::Failed("o/r has no such pull request.".into())
    );
}

fn sample_pull() -> Value {
    let mut pull = json!({
        "id": "PR_1", "number": 12, "title": "feat: x", "url": "https://github.com/o/r/pull/12",
        "body": "why", "isDraft": false, "state": "OPEN",
        "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
        "author": null,
        "headRefName": "feat/x", "baseRefName": "main", "headRefOid": "h", "baseRefOid": "b",
        "isCrossRepository": true, "headRepository": {"nameWithOwner": "fork/r"},
        "mergeable": "MERGEABLE", "mergeStateStatus": "UNSTABLE", "reviewDecision": "APPROVED",
        "viewerDidAuthor": true, "viewerCanUpdate": true,
        "autoMergeRequest": {"mergeMethod": "SQUASH"},
        "additions": 5, "deletions": 2
    });
    pull["files"] = json!({"pageInfo": {"hasNextPage": false}, "nodes": [
        {"path": "a.rs", "additions": 4, "deletions": 1, "viewerViewedState": "DISMISSED"},
        {"path": "b.rs", "additions": 1, "deletions": 1, "viewerViewedState": "VIEWED"}
    ]});
    pull["comments"] = json!({"nodes": [
        {"id": "I1", "author": {"login": "bot"}, "body": "ci", "createdAt": "2026-01-01T03:00:00Z"}
    ]});
    pull["reviews"] = json!({"nodes": [
        {"id": "R1", "author": {"login": "you"}, "state": "APPROVED", "body": "", "submittedAt": "2026-01-01T02:00:00Z"},
        {"id": "R2", "author": {"login": "you"}, "state": "COMMENTED", "body": " ", "submittedAt": "2026-01-01T04:00:00Z"},
        {"id": "R3", "author": {"login": "me"}, "state": "PENDING", "body": "draft", "submittedAt": null}
    ]});
    pull
}

fn threads() -> Value {
    let current = json!({
        "id": "T1", "isResolved": false, "isOutdated": false, "path": "a.rs",
        "line": 9, "originalLine": 8, "startLine": 7, "originalStartLine": 6,
        "diffSide": "RIGHT", "startDiffSide": "LEFT",
        "viewerCanResolve": true, "viewerCanUnresolve": false, "viewerCanReply": true,
        "comments": {"nodes": [{"id": "C1", "author": {"login": "me"}, "body": "hm", "createdAt": "2026-01-01T01:00:00Z"}]}
    });
    let outdated = json!({
        "id": "T2", "isResolved": true, "isOutdated": true, "path": "b.rs",
        "line": null, "originalLine": 3, "startLine": null, "originalStartLine": null,
        "diffSide": "LEFT", "startDiffSide": null,
        "comments": {"nodes": []}
    });
    json!({"nodes": [current, outdated]})
}

fn commits() -> Value {
    let contexts = json!([
        {"__typename": "CheckRun", "databaseId": 77, "name": "lint", "status": "COMPLETED", "conclusion": "FAILURE",
         "detailsUrl": "https://x/77", "checkSuite": {"workflowRun": {"databaseId": 5, "workflow": {"name": "ci"}}}},
        {"__typename": "CheckRun", "databaseId": 78, "name": "app", "status": "IN_PROGRESS", "conclusion": null,
         "detailsUrl": "", "checkSuite": {"workflowRun": null}},
        {"__typename": "StatusContext", "context": "deploy", "state": "SUCCESS", "targetUrl": "https://d", "description": "ok"}
    ]);
    let rollup = json!({"state": "FAILURE", "contexts": {"nodes": contexts}});
    json!({"nodes": [{"commit": {"oid": "h", "statusCheckRollup": rollup}}]})
}
