#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

use std::path::{Path, PathBuf};
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
const PLUGIN_ID: &str = "com.phux.demo.agent-tools";
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
            .join(format!("ab-{}-{n}.sock", std::process::id()));
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

    fn socket_text(&self) -> String {
        self.socket.to_string_lossy().into_owned()
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonical repo root")
}

fn run_with_env(args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    let out = Command::new(PHUX)
        // Plugin actions launch nested `phux` commands. CI does not install
        // the just-built binary on PATH, so always pass its exact location.
        .env("PHUX_BIN", PHUX)
        .envs(envs.iter().copied())
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

fn action_stdout(stdout: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(stdout).expect("action JSON");
    value["stdout"].as_str().expect("stdout field").to_owned()
}

#[test]
#[ignore = "spawns a real phux server; run explicitly when validating agent bench actions."]
fn agent_bench_launches_lists_and_drives_role_session() {
    let server = ServerGuard::start("seed");
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = repo_root();
    let xdg = repo.join("examples/plugins/agent-tools/config");
    let profile = format!("bench-{}", std::process::id());
    let state = tmp.path().join("bench.tsv");
    let state_text = state.to_string_lossy().into_owned();
    let socket = server.socket_text();
    let xdg_text = xdg.to_string_lossy().into_owned();
    let workspace = tmp.path().to_string_lossy().into_owned();

    let envs = [
        ("XDG_CONFIG_HOME", xdg_text.as_str()),
        ("PHUX_SOCKET", socket.as_str()),
        ("PHUX_AGENT_BENCH_PROFILE", profile.as_str()),
        ("PHUX_AGENT_BENCH_ROLES", "codex claude-code"),
        ("PHUX_AGENT_BENCH_STATE", state_text.as_str()),
        ("PHUX_AGENT_BENCH_WORKSPACE", workspace.as_str()),
    ];

    let (code, stdout, stderr) = run_with_env(
        &["config", "run", PLUGIN_ID, "launch-bench", "--json"],
        &envs,
    );
    assert_eq!(code, 0, "launch-bench failed: {stderr}");
    let launched = action_stdout(&stdout);
    assert!(launched.contains("codex"));
    assert!(launched.contains("claude-code"));

    let (code, stdout, stderr) =
        run_with_env(&["config", "run", PLUGIN_ID, "list-bench", "--json"], &envs);
    assert_eq!(code, 0, "list-bench failed: {stderr}");
    let listed = action_stdout(&stdout);
    assert!(listed.contains(&format!("{profile}-codex")));
    assert!(listed.contains(&format!("{profile}-claude-code")));

    let marker = format!("PHUX_AGENT_BENCH_MARKER_{}", std::process::id());
    let keyed = format!("echo {marker}");
    let drive_envs = [
        ("XDG_CONFIG_HOME", xdg_text.as_str()),
        ("PHUX_SOCKET", socket.as_str()),
        ("PHUX_AGENT_BENCH_PROFILE", profile.as_str()),
        ("PHUX_AGENT_BENCH_STATE", state_text.as_str()),
        ("PHUX_AGENT_BENCH_ROLE", "codex"),
        ("PHUX_AGENT_BENCH_KEYS", keyed.as_str()),
    ];
    let (code, stdout, stderr) = run_with_env(
        &["config", "run", PLUGIN_ID, "drive-bench", "--json"],
        &drive_envs,
    );
    assert_eq!(code, 0, "drive-bench failed: {stderr}");
    assert!(action_stdout(&stdout).contains(&format!("target={profile}-codex")));

    let target = format!("{profile}-codex");
    let (code, _stdout, stderr) = run_with_env(
        &[
            "wait",
            "--socket",
            &socket,
            "--until",
            &marker,
            "--timeout",
            "10",
            &target,
        ],
        &[],
    );
    assert_eq!(code, 0, "marker should appear in codex role pane: {stderr}");
}
