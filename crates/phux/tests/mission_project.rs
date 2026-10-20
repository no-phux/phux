//! Keeper projection of agent state into task state.

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

struct StopKeeper(PathBuf);

impl Drop for StopKeeper {
    fn drop(&mut self) {
        kill_keeper(&self.0);
    }
}

#[test]
fn agent_reports_and_a_non_zero_exit_project_into_task_state() {
    let root = tempfile::tempdir().expect("temp");
    let repo = root.path().join("repo");
    let harness = Harness::new(root.path(), "sleep 60\n", None, false);
    let _stop = StopKeeper(harness.guard.xdg.state.clone());
    let started = mission_json(
        &harness.guard,
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
    assert_eq!(started["task"]["state"], "working");
    let pane = started["binding"]["terminal_id"].as_str().unwrap();
    report(&harness.guard, pane, "blocked");
    let blocked = wait_state(&harness.guard, &repo, "demo/1", "needs_you");
    assert_valid(&blocked);
    report(&harness.guard, pane, "working");
    wait_state(&harness.guard, &repo, "demo/1", "working");
    report(&harness.guard, pane, "done");
    let done = wait_state(&harness.guard, &repo, "demo/1", "done_unchecked");
    assert_valid(&done);
    let events = event_rows(&harness.guard.xdg.state);
    assert_eq!(
        events
            .iter()
            .filter(|(_, kind)| kind == "needs_you")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|(_, kind)| kind == "agent_working")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|(_, kind)| kind == "done_unchecked")
            .count(),
        1
    );
    assert!(events.iter().any(|(actor, _)| actor == "keeper"));

    let failing = tempfile::tempdir().expect("temp");
    let fail_repo = failing.path().join("repo");
    let fail = Harness::new(failing.path(), "sleep 0.4\nexit 1\n", None, true);
    let _stop_fail = StopKeeper(fail.guard.xdg.state.clone());
    mission_json(
        &fail.guard,
        &fail_repo,
        &[
            "mission", "add", "--start", "--agent", "fake", "--repo", "demo", "Exit", "--json",
        ],
        &[],
    );
    wait_state(&fail.guard, &fail_repo, "demo/1", "failed");
    let failed = event_rows(&fail.guard.xdg.state);
    assert!(
        failed.iter().any(|(_, kind)| kind == "failed"),
        "non-zero exit should record failed, got {failed:?}"
    );
}

#[test]
fn a_freed_slot_launches_the_oldest_queued_task_once() {
    let root = tempfile::tempdir().expect("temp");
    let repo = root.path().join("repo");
    let harness = Harness::new(root.path(), "sleep 60\n", None, false);
    let _stop = StopKeeper(harness.guard.xdg.state.clone());
    let started = mission_json(
        &harness.guard,
        &repo,
        &[
            "mission", "add", "--start", "--agent", "fake", "--repo", "demo", "First", "--json",
        ],
        &[],
    );
    let pane = started["binding"]["terminal_id"].as_str().unwrap();
    mission_json(
        &harness.guard,
        &repo,
        &[
            "mission", "add", "--start", "--repo", "demo", "Second", "--json",
        ],
        &[],
    );
    assert_eq!(launch_count(&harness.log_dir), 1);
    report(&harness.guard, pane, "done");
    wait_state(&harness.guard, &repo, "demo/1", "done_unchecked");
    wait_state(&harness.guard, &repo, "demo/2", "working");
    wait_for_launches(&harness.log_dir, 2);
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(
        launch_count(&harness.log_dir),
        2,
        "the queued task launched once"
    );
}

#[test]
fn restart_reconciles_without_a_duplicate_launch_or_event() {
    let root = tempfile::tempdir().expect("temp");
    let repo = root.path().join("repo");
    let harness = Harness::new(root.path(), "sleep 60\n", None, false);
    let state = harness.guard.xdg.state.clone();
    let config = harness.guard.xdg.config.clone();
    let _stop = StopKeeper(state.clone());
    let started = mission_json(
        &harness.guard,
        &repo,
        &[
            "mission", "add", "--start", "--agent", "fake", "--repo", "demo", "Billing", "--json",
        ],
        &[],
    );
    let pane = started["binding"]["terminal_id"].as_str().unwrap();
    report(&harness.guard, pane, "blocked");
    wait_state(&harness.guard, &repo, "demo/1", "needs_you");
    let before = event_rows(&state);
    let launches = launch_count(&harness.log_dir);
    kill_keeper(&state);
    let mut guard = harness.guard;
    guard.sigkill();
    guard.wait_for_exit(Duration::from_secs(5));
    drop(guard);
    let guard = ServerGuard::builder("mission-project-restart")
        .env("XDG_STATE_HOME", &state)
        .env("XDG_CONFIG_HOME", &config)
        .start();
    let _ = mission(&guard, &repo, &["mission", "show", "demo/1", "--json"], &[]);
    wait_state(&guard, &repo, "demo/1", "failed");
    let after = event_rows(&state);
    assert_eq!(
        after
            .iter()
            .filter(|(_, kind)| kind == "terminal_lost")
            .count(),
        1,
        "{after:?}"
    );
    assert!(
        after.starts_with(&before),
        "restart rewrote history\nbefore={before:?}\nafter={after:?}"
    );
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(event_rows(&state), after);
    assert_eq!(launch_count(&harness.log_dir), launches);
}

#[test]
fn a_journal_gap_rereads_level_state() {
    let root = tempfile::tempdir().expect("temp");
    let repo = root.path().join("repo");
    let harness = Harness::new(root.path(), "sleep 60\n", Some(2), false);
    let _stop = StopKeeper(harness.guard.xdg.state.clone());
    for _ in 0..4 {
        let _ = harness.guard.run(&["spawn", "--", "/bin/true"]);
    }
    let started = mission_json(
        &harness.guard,
        &repo,
        &[
            "mission", "add", "--start", "--agent", "fake", "--repo", "demo", "Gap", "--json",
        ],
        &[
            ("PHUX_MISSION_PROJECTOR_DELAY_MS", "1500"),
            ("PHUX_MISSION_SUBSCRIBE_AFTER", "0"),
        ],
    );
    let pane = started["binding"]["terminal_id"].as_str().unwrap();
    report(&harness.guard, pane, "blocked");
    wait_state(&harness.guard, &repo, "demo/1", "needs_you");
    let log = work_dir(&harness.guard.xdg.state).join("keeper.log");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        if text.contains("keeper reconciled level state after journal_gap") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "keeper log never recorded a level reconcile after journal_gap\n{text}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let events = event_rows(&harness.guard.xdg.state);
    assert_eq!(
        events
            .iter()
            .filter(|(_, kind)| kind == "needs_you")
            .count(),
        1
    );
}

#[test]
fn a_done_that_lands_during_the_exit_grace_projects_done_unchecked() {
    let root = tempfile::tempdir().expect("temp");
    let repo = root.path().join("repo");
    // Stay up long enough to be identified, then exit 0. The projector logs
    // the grace before it will record `failed`. Publishing `done` in that
    // window is the late record the re-read has to see.
    let harness = Harness::new(root.path(), "sleep 3\nexit 0\n", None, true);
    let _stop = StopKeeper(harness.guard.xdg.state.clone());
    let started = mission_json(
        &harness.guard,
        &repo,
        &[
            "mission", "add", "--start", "--agent", "fake", "--repo", "demo", "Late", "--json",
        ],
        &[],
    );
    let pane = started["binding"]["terminal_id"].as_str().unwrap();
    let log = work_dir(&harness.guard.xdg.state).join("keeper.log");
    wait_for_log(&log, "keeper holding an exit for a late done record");
    publish_done(&harness.guard, pane);
    let done = wait_state(&harness.guard, &repo, "demo/1", "done_unchecked");
    assert_valid(&done);
    let events = event_rows(&harness.guard.xdg.state);
    assert!(
        events.iter().any(|(_, kind)| kind == "done_unchecked"),
        "late done should record done_unchecked, got {events:?}"
    );
    assert!(
        events.iter().all(|(_, kind)| kind != "failed"),
        "late done was recorded as failed: {events:?}"
    );
}

#[test]
fn an_exit_with_no_done_record_projects_failed() {
    let root = tempfile::tempdir().expect("temp");
    let repo = root.path().join("repo");
    let harness = Harness::new(root.path(), "sleep 0.6\nexit 0\n", None, true);
    let _stop = StopKeeper(harness.guard.xdg.state.clone());
    mission_json(
        &harness.guard,
        &repo,
        &[
            "mission", "add", "--start", "--agent", "fake", "--repo", "demo", "Bare", "--json",
        ],
        &[],
    );
    wait_state(&harness.guard, &repo, "demo/1", "failed");
    let events = event_rows(&harness.guard.xdg.state);
    assert!(
        events.iter().any(|(_, kind)| kind == "failed"),
        "exit with no done should record failed, got {events:?}"
    );
    assert!(
        events.iter().all(|(_, kind)| kind != "done_unchecked"),
        "exit with no done recorded done_unchecked: {events:?}"
    );
}

struct Harness {
    guard: ServerGuard,
    log_dir: PathBuf,
}

impl Harness {
    fn new(root: &Path, script_body: &str, journal_entries: Option<u32>, retain: bool) -> Self {
        let repo = root.join("repo");
        let origin = root.join("origin.git");
        let log_dir = root.join("logs");
        let target = root.join("cargo-target");
        // The detector identifies a foreground process by argv basename.
        // `sh` is a runtime wrapper, so the script's own name has to be one
        // the shipped rules already know (`claude`) before `report-state`
        // can publish a `phux.agent/v1` record.
        let script = root.join("claude");
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
        let plugin = write_plugin(root, &script, &log_dir, script_body);
        let guard = ServerGuard::builder("mission-project").start_with(|cmd| {
            write_server_config(cmd, &plugin, journal_entries, retain);
        });
        Self { guard, log_dir }
    }
}

fn write_server_config(cmd: &Command, plugin: &Path, journal_entries: Option<u32>, retain: bool) {
    let config_home = cmd
        .get_envs()
        .find_map(|(key, value)| (key == "XDG_CONFIG_HOME").then_some(value).flatten())
        .expect("XDG_CONFIG_HOME");
    let dir = Path::new(config_home).join("phux");
    std::fs::create_dir_all(&dir).unwrap();
    let defaults = match (journal_entries, retain) {
        (Some(entries), true) => {
            format!("[defaults]\nevent-journal-entries = {entries}\nretain-on-exit = true\n")
        }
        (Some(entries), false) => {
            format!("[defaults]\nevent-journal-entries = {entries}\n")
        }
        (None, true) => "[defaults]\nretain-on-exit = true\n".to_owned(),
        (None, false) => String::new(),
    };
    let text = format!(
        "{defaults}[[plugins]]\nmanifest = \"{}\"\nenabled = true\n",
        plugin.display()
    );
    std::fs::write(dir.join("config.toml"), text).unwrap();
}

fn write_plugin(root: &Path, script: &Path, log_dir: &Path, body: &str) -> PathBuf {
    let plugin = root.join("plugin");
    std::fs::create_dir_all(plugin.join("integrations")).unwrap();
    let manifest = plugin.join("phux-plugin.toml");
    std::fs::write(
        &manifest,
        "id = \"com.phux.test.mission-project\"\nname = \"mission project\"\nversion = \"0.0.1\"\nmin_phux_version = \"0.0.1\"\ndescription = \"test\"\nplatforms = [\"linux\"]\n",
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
            "#!/bin/sh\nmkdir -p \"{log}\"\n{{\n  echo \"ARGS:$*\"\n}} > \"{log}/launch-$$.txt\"\n{body}"
        ),
    )
    .unwrap();
    manifest
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

fn mission(
    guard: &ServerGuard,
    repo: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> (i32, String, String) {
    let mut cmd = guard.cmd(args);
    cmd.current_dir(repo);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let output = cmd.output().expect("phux");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn mission_json(guard: &ServerGuard, repo: &Path, args: &[&str], env: &[(&str, &str)]) -> Value {
    let (code, stdout, stderr) = mission(guard, repo, args, env);
    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
    let doc: Value = serde_json::from_str(&stdout).expect("json");
    assert_valid(&doc);
    doc
}

fn wait_for_log(path: &Path, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.contains(needle) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "keeper log never contained {needle}\n{text}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn publish_done(guard: &ServerGuard, pane: &str) {
    let target = format!("@{pane}");
    let deadline = Instant::now() + Duration::from_millis(2_000);
    loop {
        let (code, stdout, stderr) =
            guard.run(&["agent", "set", &target, "--name", "fake", "--state", "done"]);
        let last = format!("code={code} stdout={stdout} stderr={stderr}");
        // `agent set` confirms the write by printing the stored record. Exit
        // status stays 0 when the server refuses the read-back, so the body
        // has to name `done`.
        if code == 0 && stdout.contains("\"state\":\"done\"") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "could not publish done during the exit grace: {last}"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn report(guard: &ServerGuard, pane: &str, state: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (code, stdout, stderr) =
            guard.run(&["agent", "report-state", &format!("@{pane}"), state]);
        assert_eq!(
            code, 0,
            "report-state {state} stdout={stdout} stderr={stderr}"
        );
        if agent_record_state(guard, pane).as_deref() == Some(state) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "agent record for @{pane} never became {state}; last {}",
            agent_record_state(guard, pane).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn agent_record_state(guard: &ServerGuard, pane: &str) -> Option<String> {
    let (code, stdout, _) = guard.run(&["agent", "show", &format!("@{pane}"), "--json"]);
    if code != 0 {
        return None;
    }
    let doc: Value = serde_json::from_str(&stdout).ok()?;
    let want = format!("@{pane}");
    doc["agents"].as_array()?.iter().find_map(|agent| {
        (agent["terminal"] == want).then(|| agent["state"].as_str().unwrap_or_default().to_owned())
    })
}

fn wait_state(guard: &ServerGuard, repo: &Path, alias: &str, want: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (code, stdout, stderr) =
            mission(guard, repo, &["mission", "show", alias, "--json"], &[]);
        if code == 0
            && let Ok(doc) = serde_json::from_str::<Value>(&stdout)
            && doc["task"]["state"] == want
        {
            return doc;
        }
        let last = format!("code={code} stdout={stdout} stderr={stderr}");
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {alias} {want}: {last}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn launch_count(log_dir: &Path) -> usize {
    std::fs::read_dir(log_dir).map_or(0, |entries| entries.filter_map(Result::ok).count())
}

fn wait_for_launches(log_dir: &Path, want: usize) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while launch_count(log_dir) < want {
        assert!(
            Instant::now() < deadline,
            "wanted {want} launches, saw {}",
            launch_count(log_dir)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn work_dir(state_home: &Path) -> PathBuf {
    // Cargo test binaries are dev builds, so the store lives under `phux-dev`.
    state_home.join("phux-dev/work")
}

fn event_rows(state_home: &Path) -> Vec<(String, String)> {
    let path = work_dir(state_home).join("work-v1.sqlite");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match read_events(&path) {
            Ok(rows) => return rows,
            Err(err) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
                let _ = err;
            }
            Err(err) => panic!("could not read {}: {err}", path.display()),
        }
    }
}

fn read_events(path: &Path) -> Result<Vec<(String, String)>, rusqlite::Error> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = conn.prepare("SELECT actor, kind FROM event ORDER BY seq")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

fn kill_keeper(state_home: &Path) {
    let path = work_dir(state_home).join("keeper.pid");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(pid) = text.trim().parse::<u32>() else {
        return;
    };
    if pid == 0 {
        return;
    }
    let _ = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status();
    let deadline = Instant::now() + Duration::from_secs(2);
    while path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn assert_valid(value: &Value) {
    let path = runner::manifest_dir().join("../phux-mission/schema/phux.mission.v1.schema.json");
    let schema: Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).expect("schema");
    let validator = jsonschema::validator_for(&schema).expect("schema");
    let errors: Vec<_> = validator
        .iter_errors(value)
        .map(|err| err.to_string())
        .collect();
    assert!(errors.is_empty(), "{value} schema errors: {errors:?}");
}
