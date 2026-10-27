//! A Reviews surface with an inbox, an opened pull request, a thread, a kept
//! comment and a failed check, for captures and tests; no GitHub involved.

use gpui::{Context, Window};
use gpui_kit::component::diff::{DiffLineRange, DiffSide};

use crate::git::PatchText;
use crate::git::github::{
    CheckState, CiCheck, Entry, Inbox, InboxPull, LineSide, MergeMethod, MergeMethods, MergeState,
    Mergeability, Note, PrState, Pull, PullFile, Reasons, ReviewDecision, Rights, Thread,
    ViewedState,
};

use super::super::changes::review::Loaded;
use super::super::desk::Desk;
use super::{Agent, Fetched, Key, Opened, Patch, PatchFrom, Pending, Place};

const PATCH: &str =
    "diff --git a/crates/phux-git/src/github/inbox.rs b/crates/phux-git/src/github/inbox.rs
index 1111111..2222222 100644
--- a/crates/phux-git/src/github/inbox.rs
+++ b/crates/phux-git/src/github/inbox.rs
@@ -1,14 +1,16 @@
 //! Open pull requests that want the viewer.
 
 use std::collections::HashMap;
 
-/// Most pull requests one search returns.
-const PAGE: usize = 50;
+/// Most pull requests one search returns; GitHub allows 100.
+const PAGE: usize = 100;
 
 pub fn fetch(owners: &[String]) -> Result<Inbox> {
-    let query = queries(owners);
-    parse(&gh::graphql(QUERY, &query)?)
+    let [mine, asked, owned] = queries(owners);
+    let owned = if owners.is_empty() { mine.clone() } else { owned };
+    let variables = json!({ \"mine\": mine, \"asked\": asked, \"owned\": owned });
+    parse(&gh::graphql(QUERY, &variables)?)
 }
 
 fn text(node: &Value, key: &str) -> String {
     node[key].as_str().unwrap_or_default().to_owned()
diff --git a/docs/desktop-reviews.md b/docs/desktop-reviews.md
new file mode 100644
--- /dev/null
+++ b/docs/desktop-reviews.md
@@ -0,0 +1,5 @@
+# Reviews
+
+Every open pull request that wants you, in one place: the diff with full
+context, threads under their lines, checks with the failing step's log, and
+one review that lands on GitHub and in the agent's pane together.
";

fn listed(
    number: u64,
    repo: &str,
    title: &str,
    author: &str,
    checks: CheckState,
    at: &str,
) -> InboxPull {
    InboxPull {
        id: format!("PR_{number}"),
        repo: repo.into(),
        number,
        title: title.into(),
        url: format!("https://github.com/{repo}/pull/{number}"),
        author: author.into(),
        is_draft: false,
        updated_at: at.into(),
        created_at: at.into(),
        head_ref: format!("feat/{number}"),
        base_ref: "main".into(),
        head_oid: "h".into(),
        additions: 42,
        deletions: 7,
        changed_files: 3,
        mergeable: Mergeability::Mergeable,
        review_decision: None,
        checks: Some(checks),
        viewer_authored: author == "phall1",
        comments: 0,
        unresolved_threads: 0,
        reasons: Reasons {
            authored: author == "phall1",
            requested: false,
            owned: true,
        },
    }
}

fn inbox() -> Inbox {
    let mut pulls = vec![
        listed(
            1812,
            "no-phux/phux",
            "feat(desktop): review pull requests in the app",
            "phall1",
            CheckState::Failure,
            "2026-10-27T00:40:00Z",
        ),
        listed(
            1803,
            "no-phux/phux",
            "chore: release main",
            "phux-release-bot",
            CheckState::Success,
            "2026-10-26T17:31:17Z",
        ),
        listed(
            896,
            "no-phux/phux-mobile",
            "docs(plans): pane management plan",
            "phall1",
            CheckState::Pending,
            "2026-10-26T13:58:27Z",
        ),
        listed(
            874,
            "no-phux/phux-mobile",
            "feat(scripts): the Duo story drives the other pane",
            "jamesponwith",
            CheckState::Failure,
            "2026-10-24T19:43:34Z",
        ),
        listed(
            24,
            "phall1/phig",
            "chore(deps): bump the rust-dependencies group",
            "dependabot",
            CheckState::Success,
            "2026-10-21T00:24:57Z",
        ),
    ];
    pulls[0].unresolved_threads = 1;
    pulls[1].is_draft = true;
    pulls[2].reasons.requested = true;
    pulls[3].mergeable = Mergeability::Conflicting;
    pulls[4].review_decision = Some(ReviewDecision::Approved);
    Inbox {
        pulls,
        truncated: false,
    }
}

fn note(id: &str, author: &str, body: &str, at: &str) -> Note {
    Note {
        id: id.into(),
        author: author.into(),
        body: body.into(),
        created_at: at.into(),
    }
}

fn pull() -> Pull {
    Pull {
        id: "PR_1812".into(),
        repo: "no-phux/phux".into(),
        number: 1812,
        title: "feat(desktop): review pull requests in the app".into(),
        url: "https://github.com/no-phux/phux/pull/1812".into(),
        body: "Adds the **Reviews** surface: an inbox of open pull requests, full-context diffs, threads, checks and merge.".into(),
        author: "phall1".into(),
        state: PrState::Open,
        is_draft: false,
        created_at: "2026-10-26T22:00:00Z".into(),
        updated_at: "2026-10-27T00:40:00Z".into(),
        head_ref: "feat/desktop-reviews".into(),
        base_ref: "main".into(),
        head_oid: "h".into(),
        base_oid: "b".into(),
        head_repo: Some("no-phux/phux".into()),
        cross_repository: false,
        mergeable: Mergeability::Mergeable,
        merge_state: MergeState::Unstable,
        review_decision: None,
        viewer_authored: false,
        auto_merge: None,
        methods: MergeMethods {
            allowed: vec![MergeMethod::Squash, MergeMethod::Rebase],
            preferred: MergeMethod::Squash,
            auto_merge: true,
            deletes_branch: true,
        },
        additions: 9,
        deletions: 3,
        files: vec![
            PullFile { path: "crates/phux-git/src/github/inbox.rs".into(), additions: 4, deletions: 3, viewed: ViewedState::Unviewed },
            PullFile { path: "docs/desktop-reviews.md".into(), additions: 5, deletions: 0, viewed: ViewedState::Viewed },
        ],
        threads: vec![Thread {
            id: "T1".into(),
            path: "crates/phux-git/src/github/inbox.rs".into(),
            resolved: false,
            outdated: false,
            side: LineSide::Right,
            line: Some(7),
            original_line: Some(7),
            start: None,
            rights: Rights { resolve: true, unresolve: false, reply: true },
            comments: vec![
                note("C1", "jamesponwith", "Does GitHub cap search at 100 per page? Worth a comment.", "2026-10-26T23:10:00Z"),
                note("C2", "phall1", "It does; the comment above says so now.", "2026-10-26T23:40:00Z"),
            ],
        }],
        conversation: vec![Entry::Review {
            note: note("R1", "jamesponwith", "Looks close. One question inline.", "2026-10-26T23:10:00Z"),
            state: "COMMENTED".into(),
        }],
        checks: vec![
            CiCheck { name: "rust lint".into(), workflow: Some("ci".into()), state: CheckState::Failure, url: Some("https://github.com/no-phux/phux/actions".into()), job: Some(1), description: None },
            CiCheck { name: "desktop".into(), workflow: Some("ci".into()), state: CheckState::Pending, url: None, job: Some(2), description: None },
            CiCheck { name: "docs".into(), workflow: Some("ci".into()), state: CheckState::Success, url: None, job: Some(3), description: None },
        ],
        rollup: Some(CheckState::Failure),
    }
}

impl Desk {
    /// Open Reviews on the fixture, with `tab` shown.
    pub(crate) fn seed_reviews_fixture(
        &mut self,
        tab: super::Tab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = self
            .reviews
            .focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        self.reviews.open = true;
        self.reviews.offline = true;
        self.reviews.inbox = Fetched {
            data: Some(inbox()),
            loading: false,
            error: None,
        };
        let key = Key {
            repo: "no-phux/phux".into(),
            number: 1812,
        };
        self.reviews.cursor = Some(key.clone());
        let mut opened = Opened::new(key, 1);
        opened.pull.data = Some(pull());
        let text = PatchText {
            text: PATCH.into(),
            reduced: false,
        };
        opened.patch.data = Some(Patch {
            from: PatchFrom::Local("/Users/you/src/phux".into()),
            loaded: Loaded::parse(text, Vec::new()),
            head: "h".into(),
        });
        opened.place = Place {
            clone: Some(("/Users/you/src/phux".into(), "origin".into())),
            worktree: Some("/Users/you/src/phux-feat-desktop-reviews".into()),
            agent: Some(Agent {
                terminal: phux_protocol::ResourceId::local(2),
                title: "opencode · reviews".into(),
            }),
        };
        opened.pending.push(Pending {
            id: 1,
            range: DiffLineRange::new(
                "crates/phux-git/src/github/inbox.rs",
                DiffSide::Modified,
                11,
                12,
            ),
            body: "Name the empty-owners fallback; it reads as a typo.".into(),
        });
        opened.tab = tab;
        if tab == super::Tab::Checks {
            opened.log = Some(super::Log {
                job: 1,
                text: Fetched {
                    data: Some("##[group]Run nix develop -c just rust-lint\nerror: unused import: `HashMap`\n --> crates/phux-git/src/github/inbox.rs:3:5\n##[error]Process completed with exit code 1.".into()),
                    loading: false,
                    error: None,
                },
            });
        }
        self.reviews.opened = Some(opened);
        self.show_review_diff(cx);
        window.focus(&focus, cx);
        cx.notify();
    }
}
