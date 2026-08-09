//! Binary-level contract tests for the `phux worktree` JSON surface
//! (phux-w7z2.34).
//!
//! `worktree new --json` is the first call in a fan-out script: it creates the
//! checkout an agent will work in and hands back the seed pane's
//! `terminal_id`, which is what the orchestrator then sends its first prompt
//! to. Before it existed the caller had to shell-parse the prose line or issue
//! a second `phux ls --json` and guess which pane it had just made — and the
//! guess is wrong under exactly the concurrency that makes fan-out worth
//! doing.
//!
//! The document's *shape* is pinned by unit tests next to the code that builds
//! it. What can only be checked from out here is the part a script depends on
//! at the process boundary: that `--json` really is a flag on these verbs,
//! that it refuses to combine with `--attach`, and that a failure leaves
//! stdout empty and puts ONE contract line on stderr with a code from the
//! closed vocabulary (ADR-0065 §4).
//!
//! No server is involved. Every case here fails locally, before any socket is
//! touched, so these are cheap and run in the default pool.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use tempfile::TempDir;

/// Path to the freshly-built `phux` binary, injected by cargo.
const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// Run `phux <args...>` and return `(exit_code, stdout, stderr)`.
fn run(args: &[&str]) -> (i32, String, String) {
    let out = std::process::Command::new(PHUX)
        .args(args)
        .output()
        .expect("run phux binary");
    (
        out.status.code().expect("phux exited via code, not signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        // An `--all-features` build carries the dhat profiler, whose Drop
        // prints nondeterministic heap stats to stderr. Those are a build
        // diagnostic, not CLI output.
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .filter(|line| !line.starts_with("dhat: "))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Assert the shared `--json` failure contract: the expected exit code, empty
/// stdout, and stderr that is ONE line parsing as the contract document with
/// the expected `error.code` and a non-empty remedy.
fn assert_json_error(args: &[&str], expected_code: &str) {
    let (code, stdout, stderr) = run(args);
    let spelled = args.join(" ");
    assert_eq!(code, 1, "`phux {spelled}` must exit 1; stderr={stderr}");
    assert!(
        stdout.is_empty(),
        "`phux {spelled}` must leave stdout empty on failure; got {stdout:?}"
    );
    let line = stderr.trim();
    assert!(
        !line.contains('\n'),
        "`phux {spelled}` must fail as ONE stderr line; got {stderr:?}"
    );
    let doc: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|err| {
        panic!("`phux {spelled}` stderr must parse as JSON ({err}); got {stderr:?}")
    });
    assert_eq!(doc["schema_version"], 1, "{spelled}: {doc}");
    assert_eq!(doc["error"]["code"], expected_code, "{spelled}: {doc}");
    assert_eq!(doc["exit_code"], 1, "{spelled}: {doc}");
    assert!(
        doc["remedy"].as_str().is_some_and(|r| !r.is_empty()),
        "{spelled}: a failure with no remedy is half a diagnosis; got {doc}"
    );
}

/// A directory that is not inside any git repository.
fn not_a_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("empty")).expect("create dir");
    dir
}

/// Every worktree verb that creates or destroys state now carries `--json`,
/// and each reports a local failure through the one shared contract rather
/// than prose a script would have to pattern-match.
///
/// `new` fails on the repository lookup — nothing was searched, so the code is
/// `workspace`. `open` and `remove` fail the same way for the same reason: a
/// path outside a repository is not a target miss.
#[test]
fn worktree_verbs_report_local_failures_on_the_json_contract() {
    let dir = not_a_repo();
    let outside = dir.path().join("empty");
    let outside = outside.to_str().expect("utf-8 temp path");

    assert_json_error(
        &["worktree", "new", "feat/x", "--repo", outside, "--json"],
        "workspace",
    );
    assert_json_error(
        &["worktree", "open", "feat/x", "--repo", outside, "--json"],
        "workspace",
    );
    assert_json_error(
        &["worktree", "remove", "feat/x", "--repo", outside, "--json"],
        "workspace",
    );
}

/// Inside a real repository, a target that matches nothing is a *different*
/// failure from "not a repository", and the code says so. A fan-out teardown
/// script branches on exactly this distinction: `no_such_target` means the
/// worktree is already gone (fine, keep going), `workspace` means the caller
/// is standing somewhere unexpected (stop).
#[test]
fn a_target_miss_inside_a_repository_is_not_a_workspace_failure() {
    let dir = TempDir::new().expect("tempdir");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("create repo dir");
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@example.invalid"],
        vec!["config", "user.name", "phux test"],
        vec!["commit", "-q", "--allow-empty", "-m", "seed"],
    ] {
        let status = std::process::Command::new("git")
            .current_dir(&repo)
            .args(&args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }
    let repo = repo.to_str().expect("utf-8 temp path");

    assert_json_error(
        &[
            "worktree",
            "open",
            "no-such-thing",
            "--repo",
            repo,
            "--json",
        ],
        "no_such_target",
    );
    assert_json_error(
        &[
            "worktree",
            "remove",
            "no-such-thing",
            "--repo",
            repo,
            "--json",
        ],
        "no_such_target",
    );
}

/// `--json` and `--attach` cannot combine, and the refusal is at parse time
/// (exit 2), before anything is created. An attached session owns the
/// terminal, so there would be no stdout left to put a document on — and a
/// script that asked for both wants an answer it is never going to get.
#[test]
fn json_refuses_to_combine_with_attach() {
    for args in [
        vec!["worktree", "new", "feat/x", "--json", "--attach"],
        vec!["worktree", "open", "feat/x", "--json", "--attach"],
    ] {
        let (code, stdout, stderr) = run(&args);
        assert_eq!(
            code,
            2,
            "`phux {}` must be a usage error; stderr={stderr}",
            args.join(" ")
        );
        assert!(
            stdout.is_empty(),
            "a usage refusal must not print a document; got {stdout:?}"
        );
        assert!(
            stderr.contains("--attach"),
            "the refusal must name the conflicting flag; got {stderr:?}"
        );
    }
}

/// The flag is really on the verbs, spelled `--json`, and the help text says
/// what it returns. Cheap, and it catches a `--json` that silently stopped
/// being registered on one of the three.
#[test]
fn every_worktree_verb_advertises_json_in_its_help() {
    for verb in ["new", "open", "remove", "list"] {
        let (code, stdout, _stderr) = run(&["worktree", verb, "--help"]);
        assert_eq!(code, 0, "`phux worktree {verb} --help` must succeed");
        assert!(
            stdout.contains("--json"),
            "`phux worktree {verb}` must offer --json; got:\n{stdout}"
        );
    }
}
