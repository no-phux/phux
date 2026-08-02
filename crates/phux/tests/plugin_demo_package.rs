#![allow(clippy::expect_used, reason = "tests")]

use std::path::{Path, PathBuf};
use std::process::Command;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");
const PLUGIN_ID: &str = "com.phux.demo.agent-tools";
const INSPECT_ACTION: &str = "inspect";
const LIST_ACTION: &str = "list-integrations";
const VALIDATE_ACTION: &str = "validate-integrations";
const DETECT_ACTION: &str = "detect-agents";
const LAUNCH_BENCH_ACTION: &str = "launch-bench";
const SMOKE_AGENT_WRAP_ACTION: &str = "smoke-agent-wrap";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonical repo root")
}

fn demo_xdg() -> PathBuf {
    repo_root().join("examples/plugins/agent-tools/config")
}

fn run_demo(args: &[&str]) -> (i32, String, String) {
    run_demo_with_env(args, &[])
}

fn run_demo_with_env(args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    let out = Command::new(PHUX)
        .env("XDG_CONFIG_HOME", demo_xdg())
        .env("PHUX_BIN", PHUX)
        .envs(envs.iter().copied())
        .args(args)
        .output()
        .expect("run phux binary");
    (
        out.status.code().expect("phux exited via code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn stdout_from_json(output: &serde_json::Value) -> &str {
    output["stdout"].as_str().expect("stdout field")
}

fn without_dhat_footer(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|line| !line.starts_with("dhat: "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn checked_in_plugin_demo_lists_configured_actions() {
    let (code, stdout, stderr) = run_demo(&["config", "plugins"]);
    assert_eq!(code, 0, "config plugins should succeed; stderr={stderr}");
    assert_eq!(stdout.trim(), format!("{PLUGIN_ID} 0.1.0 (enabled)"));

    let (code, stdout, stderr) = run_demo(&["config", "plugins", "--json"]);
    assert_eq!(
        code, 0,
        "config plugins --json validates the manifest; stderr={stderr}"
    );
    let plugins: serde_json::Value = serde_json::from_str(&stdout).expect("plugins stdout is JSON");
    assert_eq!(plugins["plugins"][0]["id"], PLUGIN_ID);
    assert_eq!(plugins["plugins"][0]["enabled"], true);
    let actions = plugins["plugins"][0]["actions"]
        .as_array()
        .expect("actions array");
    for action in [INSPECT_ACTION, LIST_ACTION, VALIDATE_ACTION, DETECT_ACTION] {
        assert!(
            actions.iter().any(|item| item["id"] == action),
            "plugin manifest should expose {action}"
        );
    }
}

#[test]
fn inspect_action_reports_runtime_context() {
    let (code, stdout, stderr) = run_demo(&["config", "run", PLUGIN_ID, INSPECT_ACTION]);
    assert_eq!(code, 0, "config run should succeed; stderr={stderr}");
    assert!(stdout.contains("core=stable terminal/session host"));
    assert!(stdout.contains("plugin=agentic workflow package"));
    assert!(stdout.contains(&format!("plugin_id={PLUGIN_ID}")));
    assert!(stdout.contains(&format!("action_id={INSPECT_ACTION}")));

    let (code, stdout, stderr) = run_demo(&["config", "run", PLUGIN_ID, INSPECT_ACTION, "--json"]);
    assert_eq!(code, 0, "config run --json should succeed; stderr={stderr}");
    let output: serde_json::Value = serde_json::from_str(&stdout).expect("run stdout is JSON");
    assert_eq!(output["plugin_id"], PLUGIN_ID);
    assert_eq!(output["action_id"], INSPECT_ACTION);
    assert_eq!(output["outcome"], "completed");
    assert_eq!(output["exit_code"], 0);
    assert!(stdout_from_json(&output).contains("phux plugin demo"));
}

#[test]
fn integration_template_actions_are_local_and_validated() {
    let (code, stdout, stderr) = run_demo(&["config", "run", PLUGIN_ID, LIST_ACTION, "--json"]);
    assert_eq!(code, 0, "list integrations should succeed; stderr={stderr}");
    let output: serde_json::Value = serde_json::from_str(&stdout).expect("list stdout is JSON");
    let listed = stdout_from_json(&output);
    assert!(listed.contains("codex\tCodex\tterminal-agent\t0.2.0\topt-in\tcodex"));
    assert!(listed.contains("claude-code\tClaude Code\tterminal-agent\t0.2.0\topt-in\tclaude"));
    assert!(listed.contains("generic-shell-agent\tGeneric Shell Agent"));

    let (code, stdout, stderr) = run_demo(&["config", "run", PLUGIN_ID, VALIDATE_ACTION, "--json"]);
    assert_eq!(
        code, 0,
        "validate integrations should succeed; stderr={stderr}"
    );
    let output: serde_json::Value = serde_json::from_str(&stdout).expect("validate stdout is JSON");
    assert!(stdout_from_json(&output).contains("validated 4 integration templates"));

    let (code, stdout, stderr) = run_demo(&["config", "run", PLUGIN_ID, DETECT_ACTION, "--json"]);
    assert_eq!(
        code, 0,
        "detection should be disabled by default; stderr={stderr}"
    );
    let output: serde_json::Value =
        serde_json::from_str(&stdout).expect("disabled detection stdout is JSON");
    assert!(stdout_from_json(&output).contains("agent detection disabled"));
}

#[test]
fn agent_detection_is_opt_in_and_path_overridable() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let codex = tmp.path().join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexit 0\n").expect("fake codex");
    let mut perms = std::fs::metadata(&codex)
        .expect("fake codex metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&codex, perms).expect("fake codex executable");

    let fake_path = tmp.path().to_str().expect("utf8 tempdir");
    let (code, stdout, stderr) = run_demo_with_env(
        &["config", "run", PLUGIN_ID, DETECT_ACTION, "--json"],
        &[
            ("PHUX_AGENT_TOOLS_DETECT", "1"),
            ("PHUX_AGENT_TOOLS_PATH", fake_path),
        ],
    );
    assert_eq!(code, 0, "opt-in detection should succeed; stderr={stderr}");
    let output: serde_json::Value =
        serde_json::from_str(&stdout).expect("opt-in detection stdout is JSON");
    let detected = stdout_from_json(&output);
    assert!(detected.contains("codex\tCodex\tcodex\tavailable"));
    assert!(detected.contains("claude-code\tClaude Code\tclaude\tmissing"));
}

/// phux-r82.11: the agent-identity wrapper writes a `phux.agent/v1`
/// record at launch and clears it on exit. `smoke-agent-wrap` drives
/// `phux-agent-wrap.sh` against a stub `phux` (that logs its argv) and a
/// fake agent, asserting `phux agent set --name ... --kind ...` runs at
/// launch and `phux agent clear` runs on exit — the record-write path the
/// sidebar consumes, exercised with no server.
#[test]
fn agent_wrap_writes_and_clears_the_identity_record() {
    let (code, stdout, stderr) = run_demo(&[
        "config",
        "run",
        PLUGIN_ID,
        SMOKE_AGENT_WRAP_ACTION,
        "--json",
    ]);
    assert_eq!(
        code, 0,
        "agent wrap smoke should succeed; stderr={stderr}\nstdout={stdout}"
    );
    let output: serde_json::Value =
        serde_json::from_str(&stdout).expect("agent wrap smoke stdout is JSON");
    assert_eq!(output["action_id"], SMOKE_AGENT_WRAP_ACTION);
    assert_eq!(output["exit_code"], 0);
    assert!(
        stdout_from_json(&output).contains("agent wrap smoke ok"),
        "smoke script should confirm the set/clear record path: {output}"
    );
}

#[test]
fn launch_bench_reports_no_server_as_action_failure() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path().join("bench.tsv");
    let socket = tmp.path().join("stale.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("stale socket");
    drop(listener);
    let state_text = state.to_str().expect("utf8 state path");
    let socket_text = socket.to_str().expect("utf8 socket path");
    let (code, stdout, stderr) = run_demo_with_env(
        &["config", "run", PLUGIN_ID, LAUNCH_BENCH_ACTION, "--json"],
        &[
            ("PHUX_SOCKET", socket_text),
            ("PHUX_AGENT_BENCH_STATE", state_text),
            ("PHUX_AGENT_BENCH_ROLES", "codex"),
        ],
    );

    assert_ne!(code, 0, "stale socket should fail the action");
    let wrapper_stderr = without_dhat_footer(&stderr);
    assert!(
        wrapper_stderr.is_empty(),
        "wrapper stderr should stay empty for JSON output: {stderr}"
    );
    let output: serde_json::Value = serde_json::from_str(&stdout).expect("action JSON");
    assert_ne!(output["exit_code"], 0);
    assert!(
        output["stderr"]
            .as_str()
            .expect("action stderr")
            .contains("phux: no server running at"),
        "action stderr should explain failure: {output}"
    );
}
