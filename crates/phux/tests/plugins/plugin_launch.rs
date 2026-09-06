#![allow(clippy::expect_used, reason = "tests")]

//! `phux launch` end-to-end against the checked-in agent-tools plugin
//! (phux-ark7, ADR-0042). The `--print` dry run resolves a template's
//! `[launch]` command into a spawnable argv without needing a server, so it
//! exercises the whole resolution path (config -> enabled plugin ->
//! integration template -> `${PHUX_PLUGIN_ROOT}` expansion) in one shot.

use std::path::{Path, PathBuf};
use std::process::Command;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");
const PLUGIN_ID: &str = "com.phux.demo.agent-tools";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonical repo root")
}

fn demo_xdg() -> PathBuf {
    repo_root().join("examples/plugins/agent-tools/config")
}

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(PHUX)
        .env("XDG_CONFIG_HOME", demo_xdg())
        .args(args)
        .output()
        .expect("run phux binary");
    (
        out.status.code().expect("phux exited via code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn launch_print_resolves_codex_through_the_identity_wrapper() {
    let (code, stdout, stderr) = run(&["launch", "codex", "--print", "--json"]);
    assert_eq!(code, 0, "launch --print should succeed; stderr={stderr}");
    let output: serde_json::Value = serde_json::from_str(&stdout).expect("launch print JSON");

    assert_eq!(output["integration"], "codex");
    assert_eq!(output["plugin"], PLUGIN_ID);
    assert_eq!(output["working_directory"], "workspace");

    let argv: Vec<String> = output["argv"]
        .as_array()
        .expect("argv array")
        .iter()
        .map(|v| v.as_str().expect("argv element is string").to_owned())
        .collect();

    assert_eq!(argv.first().map(String::as_str), Some("sh"));
    // The wrapper path is absolute (the ${PHUX_PLUGIN_ROOT} placeholder was
    // expanded to the plugin root) and points at the real wrapper script.
    assert!(
        argv[1].starts_with('/') && argv[1].ends_with("scripts/phux-agent-wrap.sh"),
        "expected an absolute wrapper path, got {:?}",
        argv[1]
    );
    assert!(
        !argv.iter().any(|arg| arg.contains("${PHUX_PLUGIN_ROOT}")),
        "placeholder must be expanded: {argv:?}"
    );
    // The wrapper is asked to declare the codex identity and run codex.
    assert!(argv.iter().any(|arg| arg == "--kind"));
    assert_eq!(argv.last().map(String::as_str), Some("codex"));
}

#[test]
fn launch_print_appends_extra_args_after_the_agent_command() {
    let (code, stdout, stderr) = run(&[
        "launch", "codex", "--print", "--json", "--", "--model", "o3",
    ]);
    assert_eq!(code, 0, "launch --print with extra args; stderr={stderr}");
    let output: serde_json::Value = serde_json::from_str(&stdout).expect("launch print JSON");
    let argv: Vec<String> = output["argv"]
        .as_array()
        .expect("argv array")
        .iter()
        .map(|v| v.as_str().expect("string").to_owned())
        .collect();
    // Extra args land at the very end, after the wrapped `codex`.
    let tail = &argv[argv.len() - 3..];
    assert_eq!(tail, ["codex", "--model", "o3"]);
}

#[test]
fn launch_list_enumerates_the_first_party_integrations() {
    let (code, stdout, stderr) = run(&["launch", "--list", "--json"]);
    assert_eq!(code, 0, "launch --list should succeed; stderr={stderr}");
    let output: serde_json::Value = serde_json::from_str(&stdout).expect("launch list JSON");
    let ids: Vec<&str> = output["integrations"]
        .as_array()
        .expect("integrations array")
        .iter()
        .map(|item| item["integration"].as_str().expect("integration id"))
        .collect();
    for expected in ["codex", "claude-code", "gemini-cli", "generic-shell-agent"] {
        assert!(ids.contains(&expected), "missing {expected} in {ids:?}");
    }
}

#[test]
fn launch_unknown_integration_fails_with_available_hint() {
    let (code, stdout, stderr) = run(&["launch", "nonesuch", "--print"]);
    assert_ne!(code, 0, "unknown integration should fail; stdout={stdout}");
    assert!(
        stderr.contains("no launchable integration named") && stderr.contains("codex"),
        "error should list available integrations: {stderr}"
    );
}
