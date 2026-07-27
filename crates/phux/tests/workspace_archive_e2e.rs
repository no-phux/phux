#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
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
    child: Child,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
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
            child,
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

    fn socket_text(&self) -> String {
        self.socket.to_string_lossy().into_owned()
    }
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
    assert_eq!(summary["schema_version"], 1);
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

    let (code, _stdout, stderr) = ServerGuard::run(&[
        "wait",
        "--until",
        &marker,
        "--timeout",
        "5",
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
