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
