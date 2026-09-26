#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const PHUX: &str = env!("CARGO_BIN_EXE_phux");
const PLUGIN_ID: &str = "com.phux.demo.agent-tools";

struct ServerGuard(common::ServerGuard);

impl std::ops::Deref for ServerGuard {
    type Target = common::ServerGuard;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ServerGuard {
    fn start(session: &str) -> Self {
        Self(common::ServerGuard::builder("ab").session(session).start())
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
