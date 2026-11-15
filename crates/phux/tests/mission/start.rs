//! `phux mission start`: a worktree session, a brief, and an agent pane.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::common::ServerGuard;
use crate::support::{assert_valid, init_repo, mission, write_plugin};
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
    write_plugin(root.path(), &script, &log_dir, "sleep 60\n");

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
        &[],
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
        &[],
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
        &[],
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
        &[],
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

/// `…/mission` from `…/mission/worktrees/<repo>/<slug>`.
fn mission_dir(worktree: &Path) -> &Path {
    worktree
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("worktree path")
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
