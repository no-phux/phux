use super::*;

fn entry(path: &str, area: CheckoutArea, code: &str) -> CheckoutEntry {
    CheckoutEntry {
        path: path.to_owned(),
        from: None,
        area,
        code: code.to_owned(),
        added: Some(2),
        deleted: Some(1),
        binary: false,
    }
}

fn answered(answer: CheckoutAnswer) -> CheckoutOutcome {
    CheckoutOutcome::Answered(Box::new(answer))
}

#[test]
fn a_status_lists_each_file_in_its_area_and_counts_them() {
    let status = CheckoutStatus {
        branch: Some("main".to_owned()),
        ahead: 2,
        entries: vec![
            entry("staged.txt", CheckoutArea::Staged, "A"),
            entry("tracked.txt", CheckoutArea::Unstaged, "M"),
            CheckoutEntry {
                added: None,
                deleted: None,
                ..entry("new.txt", CheckoutArea::Untracked, "?")
            },
            CheckoutEntry {
                binary: true,
                from: Some("old.png".to_owned()),
                ..entry("logo.png", CheckoutArea::Staged, "R")
            },
        ],
        ..CheckoutStatus::default()
    };
    let listing = listing(&status);
    assert_eq!(
        listing.summary,
        "main · 2 ahead · 2 staged · 1 unstaged · 1 untracked"
    );
    let rows: Vec<_> = listing
        .rows
        .iter()
        .map(|r| (r.path.as_str(), r.code.as_str(), r.state, r.counts.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            ("staged.txt", "A", "staged", "+2 −1"),
            ("tracked.txt", "M", "unstaged", "+2 −1"),
            ("new.txt", "?", "untracked", ""),
            ("logo.png", "R", "staged", "binary"),
        ]
    );
    assert_eq!(listing.rows[3].from.as_deref(), Some("old.png"));
}

#[test]
fn a_clean_detached_or_cut_status_says_so() {
    let clean = CheckoutStatus {
        head: Some("0123456789abcdef".to_owned()),
        behind: 1,
        truncated: true,
        ..CheckoutStatus::default()
    };
    assert_eq!(
        listing(&clean).summary,
        "detached at 0123456 · 1 behind · no changes · more files than the server lists"
    );
    let unborn = listing(&CheckoutStatus::default());
    assert_eq!(unborn.summary, "no commits yet · no changes");
    assert!(unborn.rows.is_empty());
}

const DIFF: &str = "diff --git a/a.txt b/a.txt\n\
index 1..2 100644\n\
--- a/a.txt\n\
+++ b/a.txt\n\
@@ -1,4 +1,4 @@\n \
same\n\
-old\n\
--- a rule that was content\n\
+new\n\
+++ and one that is content\n\
\n\
\\ No newline at end of file\n";

#[test]
fn diff_lines_are_classified_by_where_they_sit() {
    use Line::{Added, Context, Hunk, Meta, Removed};
    let kinds: Vec<Line> = classify(DIFF).into_iter().map(|(kind, _)| kind).collect();
    assert_eq!(
        kinds,
        [
            Meta, Meta, Meta, Meta, Hunk, Context, Removed, Removed, Added, Added, Context, Meta
        ]
    );
}

fn diff_answer(text: &str) -> CheckoutDiff {
    CheckoutDiff {
        path: "a.txt".to_owned(),
        area: CheckoutArea::Unstaged,
        text: text.to_owned(),
        truncated: false,
        binary: false,
    }
}

#[test]
fn a_diff_groups_lines_into_runs_and_says_what_was_cut() {
    let whole = diff(&diff_answer(DIFF), MAX_LINES);
    assert_eq!(whole.note, None);
    assert_eq!(whole.runs.len(), 7);
    assert_eq!(
        whole.runs[3],
        (
            Line::Removed,
            "-old\n--- a rule that was content".to_owned()
        )
    );

    // Cut here: what arrived still shows, and the note counts the rest.
    let cut = diff(&diff_answer(DIFF), 5);
    assert_eq!(cut.runs.len(), 2);
    assert_eq!(
        cut.note.as_deref(),
        Some("Showing the first 5 of 12 lines.")
    );

    // Cut by the server.
    let truncated = CheckoutDiff {
        truncated: true,
        ..diff_answer(DIFF)
    };
    let truncated = diff(&truncated, MAX_LINES);
    assert_eq!(truncated.runs.len(), 7);
    assert!(
        truncated
            .note
            .is_some_and(|note| note.contains("too large"))
    );

    let binary = CheckoutDiff {
        binary: true,
        ..diff_answer("Binary files differ\n")
    };
    assert!(
        diff(&binary, MAX_LINES)
            .note
            .is_some_and(|n| n.contains("Binary"))
    );
    assert_eq!(
        diff(&diff_answer(""), MAX_LINES).note.as_deref(),
        Some("No text changes in this file.")
    );
}

#[test]
fn every_refusal_reads_as_words() {
    use CheckoutRefusalCode as Code;
    let say = |code, message: &str| {
        refusal(&CheckoutRefusal {
            code,
            message: message.to_owned(),
        })
    };
    for (code, words) in [
        (Code::CwdUnknown, "which folder"),
        (Code::NotARepository, "Not a Git checkout"),
        (Code::InvalidPath, "refused that path"),
        (Code::NotFound, "gone"),
        (Code::InvalidRequest, "out of bounds"),
        (Code::Timeout, "in time"),
        (Code::Busy, "busy"),
        (Code::GitFailed, "Git failed."),
        (Code::ReadFailed, "permission denied"),
        (Code::GhUnavailable, "doesn't know"),
        (Code::GhUnauthenticated, "doesn't know"),
        (Code::NoPullRequest, "doesn't know"),
        (Code::GhFailed, "doesn't know"),
        (Code::Unknown, "doesn't know"),
    ] {
        assert!(say(code, "").contains(words), "{code:?}");
    }
    // Where our words say it all, the server's are not repeated after them.
    assert!(!say(Code::NotARepository, "not inside a Git working tree").contains("inside"));
    // Git's own words follow ours.
    assert_eq!(
        say(Code::GitFailed, " fatal: bad object\n"),
        "Git failed. fatal: bad object"
    );
}

#[test]
fn every_failure_reads_as_words() {
    use CheckoutFailure as F;
    for (kind, words) in [
        (F::PermissionDenied, "Permission denied"),
        (F::TerminalNotFound, "terminal is gone"),
        (F::Unsupported, "doesn't serve changes"),
        (F::Busy, "busy"),
        (F::Unanswered, "connection ended"),
        (F::Other, "The server refused. why"),
    ] {
        assert!(failure(kind, "why").contains(words), "{kind:?}");
    }
    assert!(unsent(true).contains("doesn't support changes"));
    assert!(unsent(false).contains("Not connected"));
}

#[test]
fn an_outcome_is_its_answer_or_the_reason_there_is_none() {
    let status = answered(CheckoutAnswer::Status(CheckoutStatus::default()));
    assert!(status_of(&status).is_ok());
    assert!(diff_of(&status).is_err(), "a status is not a diff");
    let diff = answered(CheckoutAnswer::Diff(diff_answer(DIFF)));
    assert!(diff_of(&diff).is_ok());
    assert!(status_of(&diff).is_err());

    let refused = answered(CheckoutAnswer::Refused(CheckoutRefusal {
        code: CheckoutRefusalCode::NotARepository,
        message: String::new(),
    }));
    let words = status_of(&refused).expect_err("refused");
    assert!(words.starts_with("Not a Git checkout"));
    let failed = CheckoutOutcome::Failed {
        failure: CheckoutFailure::PermissionDenied,
        message: String::new(),
    };
    let words = diff_of(&failed).expect_err("failed");
    assert!(words.starts_with("Permission denied"));
}
