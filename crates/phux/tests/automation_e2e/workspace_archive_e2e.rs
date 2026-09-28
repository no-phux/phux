#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn start(session: &str) -> common::ServerGuard {
    common::ServerGuard::builder("wa").session(session).start()
}

fn run(args: &[&str]) -> (i32, String, String) {
    output(Command::new(PHUX).args(args))
}

fn run_with_xdg(args: &[&str], xdg: &std::path::Path) -> (i32, String, String) {
    output(Command::new(PHUX).env("XDG_CONFIG_HOME", xdg).args(args))
}

fn output(cmd: &mut Command) -> (i32, String, String) {
    let out = cmd.stdin(Stdio::null()).output().expect("run phux command");
    (
        out.status.code().expect("phux exited with code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Ambient budget for a fake agent's `/bin/sh` to run at all (a loaded box
/// has taken ~8s); `phux wait` returns as soon as the line appears.
const AGENT_PAINT_BUDGET_SECS: &str = "60";

/// Subject budget for a specific line once the agent is painting: it writes
/// every line in one burst, so a timeout here means the argv is wrong.
const AGENT_LINE_BUDGET_SECS: &str = "5";

/// Wait for `target` to paint at all (ambient budget), then for `marker`
/// (subject budget), so load cannot masquerade as a contract failure.
fn wait_painted_then(socket: &str, target: &str, marker: &str) {
    let (code, _, stderr) = run(&[
        "wait",
        "--until",
        "FAKE_AGENT_ARGS=",
        "--timeout",
        AGENT_PAINT_BUDGET_SECS,
        "--socket",
        socket,
        target,
    ]);
    assert_eq!(
        code, 0,
        "{target} never painted within {AGENT_PAINT_BUDGET_SECS}s (environment, not contract): {stderr}"
    );
    let (code, _, stderr) = run(&[
        "wait",
        "--until",
        marker,
        "--timeout",
        AGENT_LINE_BUDGET_SECS,
        "--socket",
        socket,
        target,
    ]);
    assert_eq!(code, 0, "{target} never printed {marker:?}: {stderr}");
}

/// Poll until `selector` is gone from `socket`'s registry (ambient budget:
/// each poll forks a `phux`).
fn wait_for_terminal_absent(socket: &str, selector: &str) {
    let budget = Duration::from_secs(60);
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        let (code, _, _) = run(&["snapshot", "--json", "--socket", socket, selector]);
        if code != 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{selector} was still present {budget:?} after kill");
}

/// `workspace save` archives a session's real L3 split layout (not the flat
/// fallback), and save -> restore -> save keeps the same split.
#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
#[allow(
    clippy::too_many_lines,
    reason = "one linear save-restore-resave round trip keeps its own assertions together"
)]
fn workspace_save_captures_and_restore_replays_a_real_split_tree() {
    let source = start("source");
    let dest = start("seed");
    let archive_dir = tempfile::tempdir().expect("archive tempdir");
    let source_archive = archive_dir.path().join("source.json");
    let dest_archive = archive_dir.path().join("dest.json");
    let cwd = archive_dir.path().to_string_lossy().into_owned();
    let source_socket = source.socket_text();
    let dest_socket = dest.socket_text();

    let (code, stdout, stderr) = run(&[
        "new",
        "--socket",
        &source_socket,
        "--json",
        "-s",
        "split-bench",
        "--cwd",
        &cwd,
    ]);
    assert_eq!(code, 0, "create split-bench session failed: {stderr}");
    let created: serde_json::Value = serde_json::from_str(&stdout).expect("new --json");
    let seed_pane = created["terminal_id"]
        .as_u64()
        .expect("new --json names the seed pane");
    let seed_selector = format!("@{seed_pane}");

    // A placed pane writes a real two-leaf split into the layout envelope.
    let (code, _stdout, stderr) = run(&[
        "spawn",
        "--socket",
        &source_socket,
        "--target",
        &seed_selector,
        "--split",
        "vertical",
        "--",
        "sleep",
        "30",
    ]);
    assert_eq!(code, 0, "placed spawn failed: {stderr}");

    let (code, stdout, stderr) = run(&[
        "workspace",
        "save",
        "--socket",
        &source_socket,
        "--output",
        &source_archive.to_string_lossy(),
    ]);
    assert_eq!(code, 0, "workspace save failed: {stderr}");
    assert!(stdout.is_empty());

    let saved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&source_archive).expect("read saved archive"),
    )
    .expect("saved archive JSON");
    let split_window = split_bench_window(&saved);
    assert_eq!(
        split_window["panes"].as_array().expect("panes array").len(),
        2,
        "the saved window must carry both panes, not just the seed: {split_window}"
    );
    assert_eq!(
        split_window["layout"]["kind"], "split",
        "the saved layout must be the real two-leaf split, not the bare-pane fallback: {split_window}"
    );

    let (code, stdout, stderr) = run(&[
        "workspace",
        "restore",
        &source_archive.to_string_lossy(),
        "--socket",
        &dest_socket,
    ]);
    assert_eq!(code, 0, "workspace restore failed: {stderr}");
    let summary: serde_json::Value = serde_json::from_str(&stdout).expect("restore summary JSON");
    assert_eq!(summary["schema_version"], 2);
    assert!(
        summary["failed"].as_array().is_none_or(Vec::is_empty),
        "restore must not report a failure: {summary}"
    );
    assert!(
        summary["restored"]
            .as_array()
            .expect("restored array")
            .iter()
            .any(|name| name == "split-bench")
    );

    let (code, stdout, stderr) = run(&[
        "workspace",
        "save",
        "--socket",
        &dest_socket,
        "--output",
        &dest_archive.to_string_lossy(),
    ]);
    assert_eq!(code, 0, "workspace save (post-restore) failed: {stderr}");
    assert!(stdout.is_empty());
    let resaved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&dest_archive).expect("read resaved archive"),
    )
    .expect("resaved archive JSON");
    let resaved_window = split_bench_window(&resaved);
    assert_eq!(
        resaved_window["panes"]
            .as_array()
            .expect("panes array")
            .len(),
        2,
        "the round-tripped window must still carry both panes: {resaved_window}"
    );
    assert_eq!(
        resaved_window["layout"]["kind"], "split",
        "the round-tripped layout must still be a real split, not flattened: {resaved_window}"
    );
}

/// The one window of the archive's `split-bench` session.
fn split_bench_window(archive: &serde_json::Value) -> serde_json::Value {
    let sessions = archive["sessions"].as_array().expect("sessions array");
    let session = sessions
        .iter()
        .find(|session| session["name"] == "split-bench")
        .unwrap_or_else(|| panic!("archive names no split-bench session: {archive}"));
    session["windows"]
        .as_array()
        .and_then(|windows| windows.first())
        .unwrap_or_else(|| panic!("split-bench session has no window: {session}"))
        .clone()
}

/// Named session's windows array from an archive, or panics naming the
/// archive it looked in.
fn session_windows<'a>(archive: &'a serde_json::Value, name: &str) -> &'a Vec<serde_json::Value> {
    archive["sessions"]
        .as_array()
        .expect("sessions array")
        .iter()
        .find(|session| session["name"] == name)
        .unwrap_or_else(|| panic!("archive names no {name:?} session: {archive}"))
        .get("windows")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("{name:?} session has no windows array: {archive}"))
}

/// Total pane count across every window of `windows`.
fn total_panes(windows: &[serde_json::Value]) -> usize {
    windows
        .iter()
        .map(|window| window["panes"].as_array().map_or(0, std::vec::Vec::len))
        .sum()
}

/// A stored layout can miss live panes (a headless spawn never touches it)
/// and keep leaves of closed panes (nothing prunes them). `workspace save`
/// must drop the dead leaf and put the headless pane in an `"unplaced"`
/// window with a warning, so save -> restore -> save keeps exactly the two
/// live panes.
#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
#[allow(
    clippy::too_many_lines,
    reason = "one linear repro-save-restore-resave round trip keeps its own assertions together"
)]
fn workspace_save_reconciles_a_dead_layout_leaf_and_a_headless_spawn() {
    // One session only, so the headless spawn can join nothing else; its
    // seed pane is always `@1`.
    let source = start("reconcile-repro");
    let dest = start("seed");
    let archive_dir = tempfile::tempdir().expect("archive tempdir");
    let source_archive = archive_dir.path().join("reconcile-source.json");
    let dest_archive = archive_dir.path().join("reconcile-dest.json");
    let source_socket = source.socket_text();
    let dest_socket = dest.socket_text();
    let seed_selector = "@1";

    // A placed pane, then killed: a dead layout leaf.
    let (code, stdout, stderr) = run(&[
        "spawn",
        "--socket",
        &source_socket,
        "--target",
        seed_selector,
        "--split",
        "vertical",
        "--json",
        "--",
        "sleep",
        "30",
    ]);
    assert_eq!(code, 0, "placed spawn failed: {stderr}");
    let placed: serde_json::Value = serde_json::from_str(&stdout).expect("spawn --json");
    let placed_id = placed["terminal_id"]
        .as_u64()
        .expect("spawn --json names the placed pane");
    let placed_selector = format!("@{placed_id}");
    let (code, _, stderr) = run(&[
        "kill",
        "--yes",
        "--socket",
        &source_socket,
        &placed_selector,
    ]);
    assert_eq!(code, 0, "kill the placed pane before save: {stderr}");
    wait_for_terminal_absent(&source_socket, &placed_selector);

    // A headless spawn joins the session but never touches its layout.
    let (code, _stdout, stderr) = run(&["spawn", "--socket", &source_socket, "--", "sleep", "30"]);
    assert_eq!(code, 0, "headless spawn failed: {stderr}");

    let (code, stdout, stderr) = run(&[
        "workspace",
        "save",
        "--socket",
        &source_socket,
        "--output",
        &source_archive.to_string_lossy(),
    ]);
    assert_eq!(code, 0, "workspace save failed: {stderr}");
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("reconcile-repro") && stderr.contains("absent from its stored layout"),
        "save must warn about the unplaced headless pane naming the session: {stderr}"
    );

    let saved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&source_archive).expect("read saved archive"),
    )
    .expect("saved archive JSON");
    let windows = session_windows(&saved, "reconcile-repro");
    assert_eq!(
        total_panes(windows),
        2,
        "archive must hold exactly the seed pane and the headless spawn — the dead leaf \
         dropped, the headless pane not lost: {windows:?}"
    );
    let unplaced = windows
        .iter()
        .find(|window| window["name"] == "unplaced")
        .unwrap_or_else(|| {
            panic!("the headless pane must land in a synthesized unplaced window: {windows:?}")
        });
    assert_eq!(
        unplaced["panes"].as_array().expect("panes array").len(),
        1,
        "unplaced window must hold exactly the one pane the layout never named: {unplaced}"
    );
    let seed_window = windows
        .iter()
        .find(|window| window["name"] != "unplaced")
        .expect("the seed's own window is still present");
    assert_eq!(
        seed_window["panes"].as_array().expect("panes array").len(),
        1,
        "the dead leaf must be pruned from the seed's own window, collapsing its split: \
         {seed_window}"
    );

    let (code, stdout, stderr) = run(&[
        "workspace",
        "restore",
        &source_archive.to_string_lossy(),
        "--socket",
        &dest_socket,
    ]);
    assert_eq!(code, 0, "workspace restore failed: {stderr}");
    let summary: serde_json::Value = serde_json::from_str(&stdout).expect("restore summary JSON");
    assert!(
        summary["failed"].as_array().is_none_or(Vec::is_empty),
        "restore must not report a failure: {summary}"
    );

    let (code, stdout, stderr) = run(&[
        "workspace",
        "save",
        "--socket",
        &dest_socket,
        "--output",
        &dest_archive.to_string_lossy(),
    ]);
    assert_eq!(code, 0, "workspace save (post-restore) failed: {stderr}");
    assert!(stdout.is_empty());
    let resaved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&dest_archive).expect("read resaved archive"),
    )
    .expect("resaved archive JSON");
    let resaved_windows = session_windows(&resaved, "reconcile-repro");
    assert_eq!(
        total_panes(resaved_windows),
        2,
        "the restored session must carry exactly two live panes, matching the source: \
         {resaved_windows:?}"
    );
}

#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
fn workspace_restore_starts_archived_command_process() {
    let dest = start("seed");
    let archive_dir = tempfile::tempdir().expect("archive tempdir");
    let archive_path = archive_dir.path().join("workspace-command.json");
    let cwd = archive_dir.path().to_string_lossy().into_owned();
    let marker = format!(
        "PHUX_RESTORED_PROCESS_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    );
    let command = vec![
        "sh".to_owned(),
        "-lc".to_owned(),
        format!("printf '%s\\nPWD=%s\\n' {marker} \"$PWD\"; sleep 30"),
    ];
    let archive = serde_json::json!({
        "schema_version": 1,
        "sessions": [
            {
                "name": "restored-proc",
                "active": true,
                "cwd": cwd,
                "command": command,
                "windows": [
                    {
                        "name": "main",
                        "active": true,
                        "layout": { "kind": "pane", "pane": 0 },
                        "panes": [
                            {
                                "active": true,
                                "cwd": cwd,
                                "command": command,
                                "cols": 80,
                                "rows": 24
                            }
                        ]
                    }
                ]
            }
        ]
    });
    std::fs::write(
        &archive_path,
        serde_json::to_string_pretty(&archive).expect("render archive"),
    )
    .expect("write archive");
    let archive_arg = archive_path.to_string_lossy().into_owned();
    let socket_arg = dest.socket_text();

    let (code, stdout, stderr) = run(&[
        "workspace",
        "restore",
        &archive_arg,
        "--socket",
        &socket_arg,
    ]);
    assert_eq!(code, 0, "workspace restore failed: {stderr}");
    let summary: serde_json::Value = serde_json::from_str(&stdout).expect("restore summary JSON");
    assert!(
        summary["restored"]
            .as_array()
            .expect("restored array")
            .iter()
            .any(|name| name == "restored-proc")
    );

    // Ambient budget: the marker is unique, so only load can expire it.
    let (code, _stdout, stderr) = run(&[
        "wait",
        "--until",
        &marker,
        "--timeout",
        AGENT_PAINT_BUDGET_SECS,
        "--socket",
        &socket_arg,
        "restored-proc",
    ]);
    assert_eq!(code, 0, "restored command marker did not appear: {stderr}");

    let (code, stdout, stderr) = run(&[
        "snapshot",
        "--json",
        "--socket",
        &socket_arg,
        "restored-proc",
    ]);
    assert_eq!(code, 0, "snapshot after restore failed: {stderr}");
    assert!(
        stdout.contains(&marker),
        "snapshot should show restored command output"
    );
    assert!(
        stdout.contains(&cwd),
        "snapshot should show restored command cwd"
    );
}

fn write_fake_agent_plugin(root: &std::path::Path) -> PathBuf {
    let plugin = root.join("plugin");
    let integrations = plugin.join("integrations");
    let scripts = plugin.join("scripts");
    let xdg = root.join("xdg");
    std::fs::create_dir_all(&integrations).expect("create integrations");
    std::fs::create_dir_all(&scripts).expect("create scripts");
    std::fs::create_dir_all(xdg.join("phux")).expect("create config");
    std::fs::write(
        plugin.join("phux-plugin.toml"),
        r#"id = "com.phux.test.restore"
name = "Restore test agent"
version = "1.0.0"
min_phux_version = "0.0.2"
platforms = ["linux", "macos"]
"#,
    )
    .expect("write manifest");
    std::fs::write(
        integrations.join("restore-agent.toml"),
        r#"schema_version = 1
id = "restore-agent"
display_name = "Restore Agent"
kind = "terminal-agent"
first_party = true

[session_identity]
mode = "native-or-phux"
native_env = "PHUX_FAKE_SESSION_ID"
restore = "external-cli"
resume_args = ["--resume", "${PHUX_AGENT_SESSION_ID}"]
fresh_args = ["--new", "${PHUX_AGENT_SESSION_ID}"]

[launch]
command = ["${PHUX_PLUGIN_ROOT}/scripts/fake-agent.sh"]
working_directory = "plugin-root"
"#,
    )
    .expect("write integration");
    std::fs::write(
        scripts.join("fake-agent.sh"),
        r#"#!/bin/sh
printf 'FAKE_AGENT_ARGS=%s\n' "$*"
printf 'FAKE_AGENT_ENV=%s\n' "$PHUX_FAKE_SESSION_ID"
printf 'FAKE_AGENT_CWD=%s\n' "$PWD"
exec sleep 60
"#,
    )
    .expect("write fake agent");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let script = scripts.join("fake-agent.sh");
        let mut permissions = std::fs::metadata(&script)
            .expect("stat fake agent")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(script, permissions).expect("make fake agent executable");
    }
    std::fs::write(
        xdg.join("phux/config.toml"),
        format!(
            "[[plugins]]\nmanifest = {:?}\nenabled = true\n",
            plugin.join("phux-plugin.toml").to_string_lossy()
        ),
    )
    .expect("write config");
    xdg
}

#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
#[allow(
    clippy::too_many_lines,
    reason = "one linear process-lifecycle scenario keeps restart and stale-owner assertions together"
)]
fn native_agent_session_is_replayed_after_pane_restart_and_rejects_stale_ownership() {
    let root = tempfile::tempdir().expect("restore test tempdir");
    let xdg = write_fake_agent_plugin(root.path());
    let source_archive = root.path().join("source.json");
    let replay_archive = root.path().join("replay.json");
    let stale_archive = root.path().join("stale.json");
    let source_archive_arg = source_archive.to_string_lossy().into_owned();
    let replay_archive_arg = replay_archive.to_string_lossy().into_owned();
    let stale_archive_arg = stale_archive.to_string_lossy().into_owned();

    let source = start("agent-restart");
    let source_socket = source.socket_text();
    let (code, stdout, stderr) = run_with_xdg(
        &[
            "launch",
            "restore-agent",
            "--socket",
            &source_socket,
            "--json",
        ],
        &xdg,
    );
    assert_eq!(code, 0, "fresh agent launch failed: {stderr}");
    let launch: serde_json::Value = serde_json::from_str(&stdout).expect("launch JSON");
    assert_eq!(launch["integration"], "restore-agent");
    let launched_id = launch["terminal_id"].as_u64().expect("terminal id");
    let launched_selector = format!("@{launched_id}");
    wait_painted_then(&source_socket, &launched_selector, "FAKE_AGENT_ARGS=--new");
    let (code, _, stderr) = run(&["kill", "--yes", "--socket", &source_socket, "@1"]);
    assert_eq!(
        code, 0,
        "remove the pre-agent seed pane before save: {stderr}"
    );
    wait_for_terminal_absent(&source_socket, "@1");
    let (code, _, stderr) = run_with_xdg(
        &[
            "workspace",
            "save",
            "--socket",
            &source_socket,
            "--output",
            &source_archive_arg,
        ],
        &xdg,
    );
    assert_eq!(code, 0, "agent archive save failed: {stderr}");

    let mut archived: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&source_archive).expect("read source archive"))
            .expect("parse source archive");
    assert_eq!(archived["schema_version"], 2);
    let agent = archived["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|session| session["name"] == "agent-restart")
        .and_then(|session| session["windows"][0]["panes"][0]["agent_session"].as_object())
        .expect("saved agent session");
    let native_id = agent["native_id"].as_str().expect("native id").to_owned();
    assert!(
        uuid::Uuid::parse_str(&native_id).is_ok(),
        "fresh identity is a UUID: {native_id}"
    );
    archived["sessions"]
        .as_array_mut()
        .expect("sessions")
        .iter_mut()
        .find(|session| session["name"] == "agent-restart")
        .expect("agent session")["windows"][0]["panes"][0]["cwd"] =
        serde_json::Value::String("/archived/cwd/must-not-win".to_owned());
    std::fs::write(
        &source_archive,
        serde_json::to_vec_pretty(&archived).expect("render cwd-edited archive"),
    )
    .expect("write cwd-edited archive");
    drop(source);

    let dest = start("replay-seed");
    let dest_socket = dest.socket_text();
    let (code, stdout, stderr) = run_with_xdg(
        &[
            "workspace",
            "restore",
            &source_archive_arg,
            "--socket",
            &dest_socket,
        ],
        &xdg,
    );
    assert_eq!(code, 0, "native restore failed: {stderr}");
    let summary: serde_json::Value = serde_json::from_str(&stdout).expect("restore summary");
    assert!(
        summary["restored"]
            .as_array()
            .expect("restored")
            .iter()
            .any(|name| name == "agent-restart")
    );
    let resume_marker = format!("FAKE_AGENT_ARGS=--resume {native_id}");
    wait_painted_then(&dest_socket, "agent-restart", &resume_marker);
    let plugin_cwd = root
        .path()
        .join("plugin")
        .canonicalize()
        .expect("canonical plugin root");
    let (code, _, stderr) = run(&[
        "wait",
        "--until",
        &format!("FAKE_AGENT_CWD={}", plugin_cwd.display()),
        "--timeout",
        AGENT_LINE_BUDGET_SECS,
        "--socket",
        &dest_socket,
        "agent-restart",
    ]);
    assert_eq!(
        code, 0,
        "restore must use current integration working directory: {stderr}"
    );

    let (code, _, stderr) = run_with_xdg(
        &[
            "workspace",
            "save",
            "--socket",
            &dest_socket,
            "--output",
            &replay_archive_arg,
        ],
        &xdg,
    );
    assert_eq!(code, 0, "replayed archive save failed: {stderr}");
    let replayed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&replay_archive).expect("read replay archive"))
            .expect("parse replay archive");
    let replayed_id = replayed["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|session| session["name"] == "agent-restart")
        .and_then(|session| {
            session["windows"][0]["panes"][0]["agent_session"]["native_id"].as_str()
        });
    assert_eq!(replayed_id, Some(native_id.as_str()));

    let mut stale = archived;
    let stale_agent = stale["sessions"]
        .as_array_mut()
        .expect("sessions")
        .iter_mut()
        .find(|session| session["name"] == "agent-restart")
        .and_then(|session| session["windows"][0]["panes"][0]["agent_session"].as_object_mut())
        .expect("saved agent session");
    stale_agent.insert(
        "plugin_id".to_owned(),
        serde_json::Value::String("com.phux.wrong-owner".to_owned()),
    );
    std::fs::write(
        &stale_archive,
        serde_json::to_vec_pretty(&stale).expect("render stale archive"),
    )
    .expect("write stale archive");
    let stale_dest = start("stale-seed");
    let stale_socket = stale_dest.socket_text();
    let (code, _, stderr) = run_with_xdg(
        &[
            "workspace",
            "restore",
            &stale_archive_arg,
            "--socket",
            &stale_socket,
        ],
        &xdg,
    );
    assert_eq!(code, 1, "stale owner must fail closed");
    assert!(
        stderr.contains("not owning plugin"),
        "ownership refusal must be explicit: {stderr}"
    );
    let (code, stdout, stderr) = run(&["ls", "--json", "--socket", &stale_socket]);
    assert_eq!(code, 0, "list stale destination: {stderr}");
    let listing: serde_json::Value = serde_json::from_str(&stdout).expect("listing JSON");
    assert!(
        !listing["sessions"]
            .as_array()
            .expect("sessions")
            .iter()
            .any(|session| session["name"] == "agent-restart"),
        "ownership mismatch must not create the archived session"
    );
}
