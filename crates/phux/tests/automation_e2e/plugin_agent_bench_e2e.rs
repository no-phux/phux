#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

use crate::common;

use std::path::PathBuf;
use std::process::Stdio;

fn repo_root() -> PathBuf {
    crate::runner::manifest_dir()
        .join("../..")
        .canonicalize()
        .expect("canonical repo root")
}

fn run_with_env(args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    crate::common::Out::of(
        common::phux_cmd(crate::runner::phux_bin())
        // Plugin actions launch nested `phux` commands. CI does not install
        // the just-built binary on PATH, so always pass its exact location.
        .env("PHUX_BIN", crate::runner::phux_bin())
        .envs(envs.iter().copied())
        .args(args)
        .stdin(Stdio::null()),
    )
    .into_parts()
}

fn bench(script: &str, envs: &[(&str, &str)]) -> (i32, String, String) {
    crate::common::Out::of(
        std::process::Command::new("/bin/sh")
            .arg(repo_root().join("plugins/agent-tools/scripts").join(script))
            .env("PHUX_BIN", crate::runner::phux_bin())
            .envs(envs.iter().copied())
            .stdin(Stdio::null()),
    )
    .into_parts()
}

#[test]
#[ignore = "spawns a real phux server; run explicitly when validating agent bench actions."]
fn agent_bench_launches_lists_and_drives_role_session() {
    let server = common::ServerGuard::builder("ab").session("seed").start();
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = repo_root();
    let xdg = repo.join("plugins/agent-tools/config");
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

    let (code, launched, stderr) = bench("launch-bench.sh", &envs);
    assert_eq!(code, 0, "launch-bench failed: {stderr}");
    assert!(launched.contains("codex"));
    assert!(launched.contains("claude-code"));

    let (code, listed, stderr) = bench("list-bench.sh", &envs);
    assert_eq!(code, 0, "list-bench failed: {stderr}");
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
    let (code, stdout, stderr) = bench("drive-bench.sh", &drive_envs);
    assert_eq!(code, 0, "drive-bench failed: {stderr}");
    assert!(stdout.contains(&format!("target={profile}-codex")));

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
