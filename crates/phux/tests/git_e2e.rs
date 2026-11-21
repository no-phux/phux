//! `phux git` against a disposable server: each verb reads the checkout the
//! pane is in. The `#[ignore]`d case runs in `just e2e`.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod common;
#[path = "common/runner.rs"]
mod runner;

use std::path::Path;

use common::ServerGuard;
use serde_json::Value;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
#[ignore = "spawns a real private server; runs in the e2e lane"]
fn git_verbs_read_the_panes_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    for (key, value) in [
        ("user.email", "test@example.com"),
        ("user.name", "Test"),
        ("commit.gpgsign", "false"),
    ] {
        git(&repo, &["config", key, value]);
    }
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
    let sha = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();

    let server = ServerGuard::builder("git-verbs").start();
    let repo_arg = repo.to_str().unwrap();
    server.success(&["new", "-s", "repo", "-c", repo_arg, "--json"]);
    let answer = |args: &[&str]| -> Value {
        let mut argv = vec!["git"];
        argv.extend_from_slice(args);
        argv.push("--json");
        let doc: Value = serde_json::from_str(&server.success(&argv)).unwrap();
        assert_eq!(doc["schema_version"], 1, "{doc}");
        doc["answer"].clone()
    };

    let status = answer(&["status", "repo"]);
    assert_eq!(status["kind"], "status", "{status}");
    assert_eq!(status["branch"], "main");
    assert_eq!(status["entries"][0]["path"], "a.txt");
    assert_eq!(status["entries"][0]["area"], "unstaged");

    let diff = answer(&["diff", "repo", "a.txt"]);
    assert!(diff["text"].as_str().unwrap().contains("+two"), "{diff}");

    let log = answer(&["log", "repo", "--limit", "1"]);
    assert_eq!(log["commits"][0]["sha"], sha.as_str(), "{log}");

    let branches = answer(&["branches", "repo"]);
    assert_eq!(branches["current"], "main", "{branches}");

    let commit = answer(&["show", "repo", &sha[..8]]);
    assert_eq!(commit["kind"], "commit", "{commit}");
    assert!(commit["diff"].as_str().unwrap().contains("+one"));

    // A typed refusal keeps its code on stdout and exits 1.
    let (code, out, _) = server.run(&["git", "show", "repo", "0000000", "--json"]);
    assert_eq!(code, 1, "{out}");
    let doc: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(doc["answer"]["kind"], "refused", "{doc}");

    // Human output of a diff is the patch itself.
    let patch = server.success(&["git", "diff", "repo", "a.txt"]);
    assert!(patch.contains("+two"), "{patch}");
}
