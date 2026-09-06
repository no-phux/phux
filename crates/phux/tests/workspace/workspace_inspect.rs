#![allow(clippy::expect_used, reason = "tests")]

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(PHUX)
        .args(args)
        .output()
        .expect("run phux binary");
    (
        out.status.code().expect("phux exited via code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture_repo() -> (
    TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).expect("create repo dir");
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "agent@example.test"]);
    git(&repo, &["config", "user.name", "phux agent"]);
    std::fs::write(repo.join("README.md"), "workspace\n").expect("write readme");
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-m", "init"]);

    let feature = tmp.path().join("repo-feature");
    let detached = tmp.path().join("repo-detached");
    let feature_arg = feature.to_string_lossy();
    let detached_arg = detached.to_string_lossy();
    git(&repo, &["worktree", "add", "-b", "feature", &feature_arg]);
    git(
        &repo,
        &["worktree", "add", "--detach", &detached_arg, "HEAD"],
    );
    (tmp, repo, feature, detached)
}

fn path_text(path: &Path) -> String {
    path.canonicalize()
        .expect("canonical path")
        .to_string_lossy()
        .into_owned()
}

#[test]
fn workspace_inspect_reports_existing_worktrees_as_json() {
    let (_tmp, repo, feature, detached) = fixture_repo();
    let repo_text = path_text(&repo);
    let feature_text = path_text(&feature);
    let detached_text = path_text(&detached);
    let repo_arg = repo.to_string_lossy();
    let (code, stdout, stderr) = run(&["workspace", "inspect", "--json", &repo_arg]);
    assert_eq!(code, 0, "workspace inspect should succeed; stderr={stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["repo"]["path"], repo_text);
    assert_eq!(value["repo"]["branch"], "main");
    assert_eq!(value["repo"]["detached"], false);

    let worktrees = value["worktrees"].as_array().expect("worktrees array");
    assert!(
        worktrees
            .iter()
            .any(|item| item["path"] == repo_text && item["current"] == true)
    );
    assert!(
        worktrees
            .iter()
            .any(|item| item["path"] == feature_text && item["branch"] == "feature")
    );
    assert!(
        worktrees
            .iter()
            .any(|item| item["path"] == detached_text && item["detached"] == true)
    );
}

#[test]
fn workspace_inspect_marks_detached_worktree_current() {
    let (_tmp, _repo, _feature, detached) = fixture_repo();
    let detached_text = path_text(&detached);
    let detached_arg = detached.to_string_lossy();
    let (code, stdout, stderr) = run(&["workspace", "inspect", "--json", &detached_arg]);
    assert_eq!(
        code, 0,
        "detached workspace inspect should succeed; stderr={stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(value["repo"]["path"], detached_text);
    assert_eq!(value["repo"]["branch"], serde_json::Value::Null);
    assert_eq!(value["repo"]["detached"], true);
    assert!(
        value["worktrees"]
            .as_array()
            .expect("worktrees array")
            .iter()
            .any(|item| item["path"] == detached_text
                && item["current"] == true
                && item["detached"] == true)
    );
}

#[test]
fn workspace_inspect_rejects_missing_repo_without_stdout() {
    let tmp = TempDir::new().expect("tempdir");
    let missing_arg = tmp.path().to_string_lossy();
    let (code, stdout, stderr) = run(&["workspace", "inspect", "--json", &missing_arg]);
    assert_ne!(code, 0, "missing repo should fail");
    assert!(stdout.is_empty());
    assert!(stderr.contains("not a git repository"));
}
