use super::session::default_worktree_path;
use super::*;

/// The ADR-0054 session name for a checkout: its sanitized basename, with
/// dash runs collapsed (three separate illegal characters still make one
/// dash), selector sigils stripped from the edges (a leading `@` would
/// parse as a terminal id, `#` as a tag, a bare `.` as the focused pane),
/// never the `:` locus separator, and a fallback when nothing survives.
#[test]
fn derived_name_is_the_sanitized_basename() {
    for (path, expected) in [
        ("/w/phux-feat-auth", "phux-feat-auth"),
        ("/w/my repo", "my-repo"),
        ("/w/v1.2_beta", "v1.2_beta"),
        ("/w/a---b", "a-b"),
        ("/w/a ~ b", "a-b"),
        ("/w/@42", "42"),
        ("/w/#tag", "tag"),
        ("/w/.hidden", "hidden"),
        ("/w/work:1.0", "work-1.0"),
        ("/w/...", FALLBACK_NAME),
        ("/", FALLBACK_NAME),
    ] {
        let name = session_name_for(Path::new(path));
        assert_eq!(name, expected, "{path}");
        assert!(
            !name.contains(':'),
            "{path}: '{name}' would parse as a locus"
        );
    }
}

#[test]
fn clean_worktree_summarizes_to_nothing() {
    assert_eq!(summarize_porcelain(""), None);
    assert_eq!(summarize_porcelain("\n  \n"), None);
}

#[test]
fn porcelain_separates_modified_from_untracked() {
    // Staged, unstaged, and renamed entries are all "work to lose";
    // only `??` is untracked.
    assert_eq!(
        summarize_porcelain("?? a.txt\n?? b.txt\n"),
        Some("2 untracked file(s)".to_owned())
    );
    assert_eq!(
        summarize_porcelain(" M a.txt\nA  b.txt\nR  c.txt -> d.txt\n"),
        Some("3 modified file(s)".to_owned())
    );
    assert_eq!(
        summarize_porcelain(" M a.txt\n?? b.txt\n"),
        Some("1 modified and 1 untracked file(s)".to_owned())
    );
}

/// `worktree new --json` returns the seed pane's `terminal_id`. The full key
/// set is pinned: it is a frozen surface (ADR-0071).
#[test]
fn the_new_document_carries_the_seed_terminal_id() {
    let doc = binding_json(
        Some("feat/auth"),
        Path::new("/src/phux-feat-auth"),
        "phux-feat-auth",
        42,
    );
    assert_eq!(doc["schema_version"], serde_json::json!(1));
    assert_eq!(doc["branch"], serde_json::json!("feat/auth"));
    assert_eq!(doc["path"], serde_json::json!("/src/phux-feat-auth"));
    assert_eq!(doc["session"], serde_json::json!("phux-feat-auth"));
    assert_eq!(
        doc["terminal_id"],
        serde_json::json!(42),
        "the terminal_id IS the feature: without it a caller must guess \
         which pane it just made",
    );

    // Compare the sorted key set regardless of serde_json's map backend:
    // the contract is which fields exist, not their rendering order.
    let mut keys: Vec<&str> = doc
        .as_object()
        .expect("a JSON object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["branch", "path", "schema_version", "session", "terminal_id"],
        "the key set is frozen surface (ADR-0071): dropping or renaming \
         one breaks every consumer",
    );
}

/// The document's session is the ADR-0054 derivation, or the `-s`
/// override when given.
#[test]
fn the_document_reports_the_session_that_was_actually_bound() {
    let path = Path::new("/src/phux-feat-auth");
    let derived = binding_json(Some("feat/auth"), path, &session_name_for(path), 7);
    assert_eq!(derived["session"], serde_json::json!("phux-feat-auth"));

    let overridden = binding_json(Some("feat/auth"), path, "agent-3", 7);
    assert_eq!(overridden["session"], serde_json::json!("agent-3"));
}

/// A detached checkout has no branch, and the document says `null` rather
/// than inventing a name — `worktree open` can be pointed at one.
#[test]
fn a_detached_checkout_reports_a_null_branch() {
    let doc = binding_json(None, Path::new("/src/detached"), "detached", 1);
    assert_eq!(doc["branch"], serde_json::Value::Null);
}

/// Teardown has the same parsing problem creation does. `killed_session`
/// is the one fact the caller cannot recover afterwards: by the time the
/// document is written the session is gone either way.
#[test]
fn the_remove_document_says_whether_a_session_was_killed() {
    let killed = removal_json(
        Some("feat/auth"),
        Path::new("/src/phux-feat-auth"),
        "phux-feat-auth",
        true,
    );
    assert_eq!(killed["schema_version"], serde_json::json!(1));
    assert_eq!(killed["killed_session"], serde_json::json!(true));
    assert_eq!(killed["removed"], serde_json::json!(true));
    assert_eq!(killed["session"], serde_json::json!("phux-feat-auth"));

    let unbound = removal_json(None, Path::new("/src/x"), "x", false);
    assert_eq!(unbound["killed_session"], serde_json::json!(false));
}

/// `--json` and `--attach` cannot combine: an attached session owns the
/// terminal, so there would be no stdout left to put the document on.
/// Asserted through the parser, because that is where the refusal lives.
#[test]
fn json_and_attach_are_mutually_exclusive_on_new_and_open() {
    for args in [
        vec!["phux", "worktree", "new", "b", "--json", "--attach"],
        vec!["phux", "worktree", "open", "b", "--json", "--attach"],
    ] {
        let parsed = crate::parse_cli(&args);
        assert!(
            parsed.is_err(),
            "`{}` must be refused at parse time",
            args.join(" ")
        );
    }
}

/// The `--json` failure channel is the shared contract line, so a script
/// that branches on `error.code` gets a code and not prose.
#[test]
fn a_json_failure_carries_a_stable_code_and_leaves_stdout_alone() {
    let doc = crate::commands::json_err::error_document(
        &crate::commands::json_err::CliError::new(
            crate::commands::json_err::codes::WORKSPACE,
            "not a git repository",
            "run this inside a git repository, or point --repo at one",
        ),
        1,
    );
    assert_eq!(doc["error"]["code"], serde_json::json!("workspace"));
    assert_eq!(doc["exit_code"], serde_json::json!(1));
}

/// The default checkout is a sibling of the repository, with the branch's
/// slashes flattened so it stays one path component.
#[test]
fn default_path_is_a_sibling_of_the_repo() {
    for (branch, expected) in [
        ("feat/auth", "/src/phux-feat-auth"),
        ("a/b/c", "/src/phux-a-b-c"),
    ] {
        let dest = default_worktree_path(Path::new("/src/phux"), branch);
        assert_eq!(dest, PathBuf::from(expected), "{branch}");
        assert_eq!(dest.components().count(), 3, "{branch}");
    }
}
