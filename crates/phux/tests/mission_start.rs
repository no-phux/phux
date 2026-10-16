//! `phux mission start`: a worktree session, a brief, and an agent pane.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in an integration test"
)]

mod common;
#[path = "common/runner.rs"]
mod runner;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::ServerGuard;
use serde_json::Value;

#[test]
#[allow(clippy::too_many_lines, reason = "one start-then-queue scenario")]
#[expect(
    clippy::cognitive_complexity,
    reason = "one start-then-queue scenario; every assert scores as a branch"
)]
fn start_creates_a_worktree_session_and_queues_past_the_cap() {
    let root = tempfile::tempdir().expect("temp");
    let repo = root.path().join("repo");
    let origin = root.path().join("origin.git");
    let log_dir = root.path().join("logs");
    let target = root.path().join("cargo-target");
    let script = root.path().join("record.sh");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&log_dir).unwrap();
    init_repo(&repo, &origin);
    std::fs::create_dir_all(repo.join(".phux")).unwrap();
    std::fs::write(
        repo.join(".phux/mission.toml"),
        format!(
            "[mission]\nmax_agents = 1\nshared_target_dir = \"{}\"\nagent = \"fake\"\nrules = \"Stay in the worktree.\\n\"\n",
            target.display()
        ),
    )
    .unwrap();
    write_plugin(root.path(), &script, &log_dir);

    let guard = ServerGuard::builder("mission-start")
        .env("PHUX_MISSION_TEST_DIR", &log_dir)
        .start();
    let config_dir = guard.xdg.config.join("phux");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            "[[plugins]]\nmanifest = \"{}\"\nenabled = true\n",
            root.path().join("plugin/phux-plugin.toml").display()
        ),
    )
    .unwrap();

    let (code, stdout, stderr) = mission(
        &guard,
        &repo,
        &[
            "mission",
            "add",
            "--start",
            "--agent",
            "fake",
            "--repo",
            "demo",
            "Billing page",
            "--json",
        ],
    );
    assert_eq!(code, 0, "start failed\nstdout={stdout}\nstderr={stderr}");
    let doc: Value = serde_json::from_str(&stdout).expect("start json");
    assert_valid(&doc);
    assert_eq!(doc["verb"], "start");
    assert_eq!(doc["disposition"], "started");
    assert_eq!(doc["task"]["alias"], "demo/1");
    assert_eq!(doc["task"]["repo"], "demo");
    assert_eq!(doc["task"]["state"], "working");
    assert_eq!(doc["run"]["state"], "running");
    assert_eq!(doc["run"]["agent_kind"], "fake");
    assert_eq!(doc["binding"]["branch"], "mission/1-billing-page");
    assert_eq!(doc["binding"]["tab_title"], "#1 Billing page");
    assert_eq!(doc["binding"]["session"], "1-billing-page");
    let worktree = PathBuf::from(doc["binding"]["worktree_path"].as_str().unwrap());
    assert!(
        worktree.join(".git").exists() || worktree.exists(),
        "{worktree:?}"
    );
    assert!(
        repo_has_branch(&repo, "mission/1-billing-page"),
        "branch missing"
    );
    let brief = std::fs::read_to_string(mission_dir(&worktree).join("tasks/demo/1/brief.md"))
        .expect("brief");
    assert!(brief.contains("Billing page"), "{brief}");
    assert!(brief.contains("Stay in the worktree."), "{brief}");
    let launch = wait_for_launch(&log_dir);
    assert!(
        launch.contains(&format!("CARGO_TARGET_DIR={}", target.display())),
        "{launch}"
    );
    assert!(
        launch.contains("PHUX_SOCKET=") && !launch.contains(&guard.socket_text()),
        "agent socket should be the credential, not {}\n{launch}",
        guard.socket_text()
    );
    assert!(
        launch.contains("brief.md"),
        "brief was not passed as an argument\n{launch}"
    );

    let listed = guard.cmd(&["ls", "--json"]).output().expect("ls");
    let list_text = String::from_utf8_lossy(&listed.stdout);
    assert!(
        listed.status.success(),
        "ls failed {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    assert!(list_text.contains("1-billing-page"), "{list_text}");
    let list: Value = serde_json::from_str(&list_text).expect("ls json");
    let project_set = list["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|session| session["name"] == "1-billing-page" && !session["project"].is_null());
    assert!(project_set, "session project attr missing: {list_text}");

    let (code, stdout, stderr) = mission(
        &guard,
        &repo,
        &[
            "mission",
            "add",
            "--start",
            "--repo",
            "demo",
            "Next page",
            "--json",
        ],
    );
    assert_eq!(code, 0, "queue failed\nstdout={stdout}\nstderr={stderr}");
    let queued: Value = serde_json::from_str(&stdout).expect("queue json");
    assert_valid(&queued);
    assert_eq!(queued["disposition"], "queued");
    assert_eq!(queued["queue_position"], 1);
    assert_eq!(queued["task"]["state"], "queued");
    assert!(queued["binding"].is_null());
    assert_eq!(queued["run"]["state"], "queued");

    let (code, stdout, stderr) = mission(
        &guard,
        &repo,
        &["mission", "add", "--start", "--repo", "demo", "Third page"],
    );
    assert_eq!(
        code, 0,
        "human queue failed\nstdout={stdout}\nstderr={stderr}"
    );
    assert_eq!(stdout, "queued demo/3 (rev 2), position 2\n");

    let logs = std::fs::read_dir(&log_dir).unwrap().count();
    assert_eq!(logs, 1, "a queued task must not launch another agent");
}

#[test]
fn a_failed_start_on_a_non_tty_leaves_the_worktree() {
    let root = tempfile::tempdir().expect("temp");
    let repo = root.path().join("repo");
    let origin = root.path().join("origin.git");
    std::fs::create_dir_all(&repo).unwrap();
    init_repo(&repo, &origin);
    let guard = ServerGuard::builder("mission-start-fail").start();
    let (code, stdout, stderr) = mission(
        &guard,
        &repo,
        &[
            "mission",
            "add",
            "--start",
            "--agent",
            "missing-agent",
            "--repo",
            "demo",
            "Billing page",
            "--json",
        ],
    );
    assert_eq!(code, 2, "stdout={stdout}\nstderr={stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    let err: Value = serde_json::from_str(&stderr).expect("error json");
    assert_valid(&err);
    assert_eq!(err["error"]["code"], "mission_confirmation_required");
    assert_eq!(err["exit_code"], 2);
    let message = err["error"]["message"].as_str().unwrap_or("");
    let kept = message
        .split_once("; worktree kept at ")
        .is_some_and(|(_, path)| Path::new(path).is_dir());
    assert!(kept, "non-tty start must leave the worktree: {stderr}");
    assert!(stderr.contains("worktree kept at "), "{stderr}");
    assert!(!stderr.contains("left the worktree"), "{stderr}");
    assert!(!stderr.contains("--yes"), "{stderr}");
}

fn init_repo(repo: &Path, origin: &Path) {
    git(repo, &["init", "-b", "main"]);
    git(repo, &["config", "user.email", "t@example.com"]);
    git(repo, &["config", "user.name", "test"]);
    git(repo, &["commit", "--allow-empty", "-m", "init"]);
    git(repo, &["init", "--bare", origin.to_str().unwrap()]);
    git(repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(repo, &["push", "-u", "origin", "main"]);
    git(repo, &["remote", "set-head", "origin", "main"]);
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repo_has_branch(repo: &Path, branch: &str) -> bool {
    Command::new("git")
        .current_dir(repo)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .status()
        .expect("show-ref")
        .success()
}

fn write_plugin(root: &Path, script: &Path, log_dir: &Path) {
    let plugin = root.join("plugin");
    std::fs::create_dir_all(plugin.join("integrations")).unwrap();
    std::fs::write(
        plugin.join("phux-plugin.toml"),
        "id = \"com.phux.test.mission-fake\"\nname = \"mission fake\"\nversion = \"0.0.1\"\nmin_phux_version = \"0.0.1\"\ndescription = \"test\"\nplatforms = [\"linux\"]\n",
    )
    .unwrap();
    std::fs::write(
        plugin.join("integrations/fake.toml"),
        format!(
            "schema_version = 1\nid = \"fake\"\ndisplay_name = \"Fake\"\nkind = \"terminal-agent\"\n\n[launch]\ncommand = [\"/bin/sh\", \"{}\"]\nworking_directory = \"workspace\"\n",
            script.display()
        ),
    )
    .unwrap();
    let log = log_dir.display();
    std::fs::write(
        script,
        format!(
            "#!/bin/sh\nmkdir -p \"{log}\"\n{{\n  echo \"CARGO_TARGET_DIR=$CARGO_TARGET_DIR\"\n  echo \"PHUX_SOCKET=$PHUX_SOCKET\"\n  echo \"ARGS:$*\"\n}} > \"{log}/launch-$$.txt\"\nsleep 60\n"
        ),
    )
    .unwrap();
}

/// `…/mission` from `…/mission/worktrees/<repo>/<slug>`.
fn mission_dir(worktree: &Path) -> &Path {
    worktree
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("worktree path")
}

fn mission(guard: &ServerGuard, repo: &Path, args: &[&str]) -> (i32, String, String) {
    let mut cmd = guard.cmd(args);
    cmd.current_dir(repo);
    let output = cmd.output().expect("phux");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn wait_for_launch(log_dir: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(text) = std::fs::read_dir(log_dir).ok().and_then(|entries| {
            entries
                .filter_map(Result::ok)
                .find_map(|entry| std::fs::read_to_string(entry.path()).ok())
        }) && !text.is_empty()
        {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "launch log did not appear in {}",
            log_dir.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn assert_valid(value: &Value) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../phux-mission/schema/phux.mission.v1.schema.json");
    let schema: Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).expect("schema");
    let validator = jsonschema::validator_for(&schema).expect("schema");
    let errors: Vec<_> = validator
        .iter_errors(value)
        .map(|err| err.to_string())
        .collect();
    assert!(errors.is_empty(), "{value} schema errors: {errors:?}");
}
