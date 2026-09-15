#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Idle lifetime for this file's harness server, as a backstop UNDER the
/// `Drop` kill (ADR-0063). The guard is still the primary cleanup; it cannot
/// run if the test process is `SIGKILL`ed or the runner is reaped mid-job, and
/// what leaks then is a daemon holding a live PTY on a socket nobody will
/// ever look at again. Ten minutes is far longer than any gap between this
/// file's client connections, so it can only fire after the harness is gone.
const SERVER_IDLE_LIMIT_SECS: &str = "600";

const PHUX: &str = env!("CARGO_BIN_EXE_phux");
const SOCKET_DEADLINE: Duration = Duration::from_secs(20);
const SOCKET_POLL: Duration = Duration::from_millis(50);

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct ServerGuard {
    _process: common::ServerProcess,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl ServerGuard {
    fn start(session: &str) -> Self {
        let dir = tempfile::tempdir().expect("create temp dir for socket");
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket = dir
            .path()
            .join(format!("wa-{}-{n}.sock", std::process::id()));
        let child = Command::new(PHUX)
            .args(["server", "--session", session, "--socket"])
            .arg(&socket)
            .args(["--exit-after-idle", SERVER_IDLE_LIMIT_SECS])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux server");

        let guard = Self {
            _process: common::ServerProcess::from_child(child, socket.clone()),
            socket,
            _dir: dir,
        };
        guard.wait_for_socket();
        guard
    }

    fn wait_for_socket(&self) {
        let deadline = Instant::now() + SOCKET_DEADLINE;
        while Instant::now() < deadline {
            if self.socket.exists() {
                return;
            }
            std::thread::sleep(SOCKET_POLL);
        }
        panic!(
            "phux server did not bind {} within {SOCKET_DEADLINE:?}",
            self.socket.display()
        );
    }

    fn run(args: &[&str]) -> (i32, String, String) {
        let out = Command::new(PHUX)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("run phux command");
        (
            out.status.code().expect("phux exited with code"),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
    fn run_with_xdg(args: &[&str], xdg: &std::path::Path) -> (i32, String, String) {
        let out = Command::new(PHUX)
            .env("XDG_CONFIG_HOME", xdg)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("run phux command");
        (
            out.status.code().expect("phux exited with code"),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn socket_text(&self) -> String {
        self.socket.to_string_lossy().into_owned()
    }
}

/// How long the fake agent's `/bin/sh` gets to be forked, exec'd, scheduled
/// and to paint its first line (phux-dnhf).
///
/// This is an AMBIENT budget, not a subject budget. Nothing about the archive
/// contract is being measured while it runs — only whether the machine got
/// around to running a shell script. phux-m64c measured a freshly spawned
/// `/bin/sh` taking 7.8 seconds to execute its first instruction on a loaded
/// developer box, and this test's own 5s budget is what timed out ("wait timed
/// out after 34 polls", exit 124) during a fleet-load bisect that then blamed
/// an innocent commit. Generous here costs nothing: `phux wait` returns the
/// instant its condition holds, so the passing path is unchanged and only the
/// failing path waits longer — and when it does fail, it fails against a
/// budget that names the environment instead of the feature.
const AGENT_PAINT_BUDGET_SECS: &str = "60";

/// How long a *specific* line is then given to appear, once the agent has
/// proved it is painting at all.
///
/// This is the SUBJECT budget and is deliberately short and deliberately
/// unchanged: the fake agent writes all of its lines in one burst, so once the
/// first has landed the rest are already there. A timeout at this stage means
/// the restored argv is genuinely wrong, which is exactly what this test
/// exists to catch.
const AGENT_LINE_BUDGET_SECS: &str = "5";

/// Poll until `selector` is gone from `socket`'s registry.
///
/// Ambient budget, for the same reason as [`AGENT_PAINT_BUDGET_SECS`]: every
/// iteration forks a whole `phux` process, so what this deadline mostly buys
/// is process startup rather than anything about the reap. It is a
/// precondition of the save that follows, never the subject of an assertion.
fn wait_for_terminal_absent(socket: &str, selector: &str) {
    let budget = Duration::from_secs(60);
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        let (code, _, _) = ServerGuard::run(&["snapshot", "--json", "--socket", socket, selector]);
        if code != 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{selector} was still present {budget:?} after kill");
}

/// PHA-406 L18 review item 9: `workspace save` reads a session's *real* L3
/// layout envelope (the split a `phux spawn --target --split` write
/// produces) instead of `GET_STATE`'s `WindowInfo.layout`, which the
/// reference server never populates. Full round trip: split a pane on a
/// source server, save (asserting the archive already carries a real
/// `Split` node, not a flat one-pane-per-window fallback), restore into a
/// fresh server, then save that server too and assert the same split shape
/// survived the round trip.
#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
#[allow(
    clippy::too_many_lines,
    reason = "one linear save-restore-resave round trip keeps its own assertions together"
)]
fn workspace_save_captures_and_restore_replays_a_real_split_tree() {
    let source = ServerGuard::start("source");
    let dest = ServerGuard::start("seed");
    let archive_dir = tempfile::tempdir().expect("archive tempdir");
    let source_archive = archive_dir.path().join("source.json");
    let dest_archive = archive_dir.path().join("dest.json");
    let cwd = archive_dir.path().to_string_lossy().into_owned();
    let source_socket = source.socket_text();
    let dest_socket = dest.socket_text();

    let (code, stdout, stderr) = ServerGuard::run(&[
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

    // A second pane placed beside the first writes a real two-leaf split
    // into `split-bench`'s layout envelope (`SplitPreservingFocus`) — this
    // is the tree `GET_STATE` can never see, only `GET_METADATA` can.
    let (code, _stdout, stderr) = ServerGuard::run(&[
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

    let (code, stdout, stderr) = ServerGuard::run(&[
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

    let (code, stdout, stderr) = ServerGuard::run(&[
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
    assert!(
        summary["restored"]
            .as_array()
            .expect("restored array")
            .iter()
            .any(|name| name == "split-bench")
    );

    // Saving the just-restored server proves the round trip end to end: the
    // replayed layout envelope decodes back into the same split shape.
    let (code, stdout, stderr) = ServerGuard::run(&[
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

/// PHA-406 L18 verification review items 1+2: a stored layout envelope can
/// both miss live panes entirely (a headless `phux spawn` with no
/// `--target` never touches L3 layout — it only joins the session) and go
/// on referencing one that has since closed (nothing ever prunes a dead
/// leaf out of a stored layout). Before the fix, `workspace save` used to
/// (1) silently drop the headless pane from the archive, and (2) archive
/// the dead leaf as a `{"active":false,"cols":0,"rows":0}` phantom that
/// `workspace restore` then recreated as a brand new, empty shell — a
/// restored workspace with the wrong pane count either way.
///
/// Repro, in one session: split a pane into the layout, kill it (leaving a
/// dead leaf nothing prunes), then spawn a second, headless pane with no
/// `--target` (leaving a live pane the layout never named). `workspace
/// save` must reconcile both: the dead leaf is dropped from its window,
/// and the headless pane lands in a synthesized `"unplaced"` window with a
/// stderr warning naming the session. Saving the restored destination
/// again and comparing pane counts proves the round trip preserves
/// exactly the two live panes the source actually had — never three
/// (the dead leaf resurrected) and never one (the headless pane dropped).
#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
#[allow(
    clippy::too_many_lines,
    reason = "one linear repro-save-restore-resave round trip keeps its own assertions together"
)]
fn workspace_save_reconciles_a_dead_layout_leaf_and_a_headless_spawn() {
    // The server's own pre-seeded session (not a second one created via
    // `phux new`) is deliberately the only session on this server: a
    // headless spawn's "join the most recently active session" heuristic
    // (`resolve_spawn_ownership`, `crates/phux-server/src/runtime/attach.rs`)
    // only tracks *attach* activity — nothing in this headless flow ever
    // attaches — so with more than one session present it falls back to
    // "the first session in the registry", which is the pre-seeded one,
    // not a session created afterward. One session removes that ambiguity:
    // whichever fallback fires, it can only mean this session. The
    // pre-seeded session's seed pane is always wire id 1 (the first
    // resource a fresh server ever creates).
    let source = ServerGuard::start("reconcile-repro");
    let dest = ServerGuard::start("seed");
    let archive_dir = tempfile::tempdir().expect("archive tempdir");
    let source_archive = archive_dir.path().join("reconcile-source.json");
    let dest_archive = archive_dir.path().join("reconcile-dest.json");
    let source_socket = source.socket_text();
    let dest_socket = dest.socket_text();
    let seed_selector = "@1";

    // A pane placed into the layout, then killed: a dead layout leaf
    // (review item 2) that nothing ever prunes.
    let (code, stdout, stderr) = ServerGuard::run(&[
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
    let (code, _, stderr) =
        ServerGuard::run(&["kill", "--socket", &source_socket, &placed_selector]);
    assert_eq!(code, 0, "kill the placed pane before save: {stderr}");
    wait_for_terminal_absent(&source_socket, &placed_selector);

    // A headless spawn with no --target joins the session (the only one on
    // this server) but never touches its layout (review item 1).
    let (code, _stdout, stderr) =
        ServerGuard::run(&["spawn", "--socket", &source_socket, "--", "sleep", "30"]);
    assert_eq!(code, 0, "headless spawn failed: {stderr}");

    let (code, stdout, stderr) = ServerGuard::run(&[
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

    let (code, stdout, stderr) = ServerGuard::run(&[
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

    // Saving the just-restored destination proves the round trip end to
    // end: exactly the source's two live panes, never a phantom shell for
    // the pruned dead leaf and never a dropped headless pane.
    let (code, stdout, stderr) = ServerGuard::run(&[
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
fn workspace_archive_saves_and_restores_sessions() {
    let source = ServerGuard::start("source");
    let dest = ServerGuard::start("seed");
    let archive_dir = tempfile::tempdir().expect("archive tempdir");
    let archive_path = archive_dir.path().join("workspace.json");
    let archive = archive_path.to_string_lossy().into_owned();
    let cwd = archive_dir.path().to_string_lossy().into_owned();
    let source_socket = source.socket_text();
    let dest_socket = dest.socket_text();

    let (code, _stdout, stderr) = ServerGuard::run(&[
        "new",
        "--socket",
        &source_socket,
        "--json",
        "-s",
        "bench",
        "--cwd",
        &cwd,
    ]);
    assert_eq!(code, 0, "create bench session failed: {stderr}");

    let (code, stdout, stderr) = ServerGuard::run(&[
        "workspace",
        "save",
        "--socket",
        &source_socket,
        "--output",
        &archive,
    ]);
    assert_eq!(code, 0, "workspace save failed: {stderr}");
    assert!(stdout.is_empty(), "save --output should not print stdout");

    let (code, stdout, stderr) =
        ServerGuard::run(&["workspace", "restore", &archive, "--socket", &dest_socket]);
    assert_eq!(code, 0, "workspace restore failed: {stderr}");
    let summary: serde_json::Value = serde_json::from_str(&stdout).expect("restore summary JSON");
    assert_eq!(summary["schema_version"], 2);
    assert!(
        summary["restored"]
            .as_array()
            .expect("restored array")
            .len()
            >= 2
    );

    let (code, stdout, stderr) = ServerGuard::run(&["ls", "--json", "--socket", &dest_socket]);
    assert_eq!(code, 0, "ls after restore failed: {stderr}");
    let listing: serde_json::Value = serde_json::from_str(&stdout).expect("ls JSON");
    let sessions = listing["sessions"].as_array().expect("sessions array");
    assert!(sessions.iter().any(|session| session["name"] == "source"));
    assert!(sessions.iter().any(|session| session["name"] == "bench"));
}

#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
fn workspace_restore_starts_archived_command_process() {
    let dest = ServerGuard::start("seed");
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

    let (code, stdout, stderr) = ServerGuard::run(&[
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

    // Ambient budget: the marker is unique to this restore, so there is no
    // wrong-content failure mode to keep on a tight clock — the only way this
    // wait can expire is the machine not running the restored pane's command.
    let (code, _stdout, stderr) = ServerGuard::run(&[
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

    let (code, stdout, stderr) = ServerGuard::run(&[
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

    let source = ServerGuard::start("agent-restart");
    let source_socket = source.socket_text();
    let (code, stdout, stderr) = ServerGuard::run_with_xdg(
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
    // Two waits, not one, and the split is the point (phux-dnhf). The first
    // pays for the ambient cost of getting a shell script running at all; the
    // second measures the only thing this step is actually about — that the
    // fresh launch passed `--new`. Collapsing them into a single 5s budget is
    // what made this test look like a hard regression under fleet load.
    let (code, _, stderr) = ServerGuard::run(&[
        "wait",
        "--until",
        "FAKE_AGENT_ARGS=",
        "--timeout",
        AGENT_PAINT_BUDGET_SECS,
        "--socket",
        &source_socket,
        &launched_selector,
    ]);
    assert_eq!(
        code, 0,
        "the fake agent never painted at all within {AGENT_PAINT_BUDGET_SECS}s; \
         the machine did not schedule the plugin's shell script, which is an \
         environment problem and not an archive-contract failure: {stderr}"
    );
    let (code, _, stderr) = ServerGuard::run(&[
        "wait",
        "--until",
        "FAKE_AGENT_ARGS=--new",
        "--timeout",
        AGENT_LINE_BUDGET_SECS,
        "--socket",
        &source_socket,
        &launched_selector,
    ]);
    assert_eq!(code, 0, "fresh agent did not start: {stderr}");
    let (code, _, stderr) = ServerGuard::run(&["kill", "--socket", &source_socket, "@1"]);
    assert_eq!(
        code, 0,
        "remove the pre-agent seed pane before save: {stderr}"
    );
    wait_for_terminal_absent(&source_socket, "@1");
    let (code, _, stderr) = ServerGuard::run_with_xdg(
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

    let dest = ServerGuard::start("replay-seed");
    let dest_socket = dest.socket_text();
    let (code, stdout, stderr) = ServerGuard::run_with_xdg(
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
    // Same split as the fresh launch: ambient budget for the restored pane's
    // shell to run, subject budget for the argv it was handed.
    let (code, _, stderr) = ServerGuard::run(&[
        "wait",
        "--until",
        "FAKE_AGENT_ARGS=",
        "--timeout",
        AGENT_PAINT_BUDGET_SECS,
        "--socket",
        &dest_socket,
        "agent-restart",
    ]);
    assert_eq!(
        code, 0,
        "the restored agent never painted at all within {AGENT_PAINT_BUDGET_SECS}s; \
         the machine did not schedule the plugin's shell script, which is an \
         environment problem and not an archive-contract failure: {stderr}"
    );
    let (code, _, stderr) = ServerGuard::run(&[
        "wait",
        "--until",
        &resume_marker,
        "--timeout",
        AGENT_LINE_BUDGET_SECS,
        "--socket",
        &dest_socket,
        "agent-restart",
    ]);
    assert_eq!(code, 0, "resumed agent did not replay exact id: {stderr}");
    let plugin_cwd = root
        .path()
        .join("plugin")
        .canonicalize()
        .expect("canonical plugin root");
    let (code, _, stderr) = ServerGuard::run(&[
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

    let (code, _, stderr) = ServerGuard::run_with_xdg(
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
    let stale_dest = ServerGuard::start("stale-seed");
    let stale_socket = stale_dest.socket_text();
    let (code, _, stderr) = ServerGuard::run_with_xdg(
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
    let (code, stdout, stderr) = ServerGuard::run(&["ls", "--json", "--socket", &stale_socket]);
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
