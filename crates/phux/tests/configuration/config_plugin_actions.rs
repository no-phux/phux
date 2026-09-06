#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::process::Command;

use tempfile::TempDir;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

fn write_fixture(tmp: &TempDir, enabled: bool, command: &str) -> std::path::PathBuf {
    let plugin_dir = tmp.path().join("plugin");
    std::fs::create_dir_all(&plugin_dir).expect("create plugin dir");
    std::fs::write(
        plugin_dir.join("phux-plugin.toml"),
        format!(
            r#"
id = "example.actions"
name = "Actions"
version = "0.1.0"
min_phux_version = "0.0.2"

[[actions]]
id = "probe"
title = "Probe"
command = ["sh", "-c", {command:?}]
"#
        ),
    )
    .expect("write manifest");

    let config_dir = tmp.path().join("xdg").join("phux");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            r#"
[[plugins]]
manifest = "{}"
enabled = {enabled}
"#,
            plugin_dir.join("phux-plugin.toml").display()
        ),
    )
    .expect("write config");
    tmp.path().join("xdg")
}

fn run_with_xdg(args: &[&str], xdg: &std::path::Path) -> (i32, String, String) {
    let out = Command::new(PHUX)
        .env("XDG_CONFIG_HOME", xdg)
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
fn config_run_json_executes_manifest_action_with_captured_output() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = write_fixture(&tmp, true, "printf out; printf err >&2; exit 3");

    let (code, stdout, stderr) = run_with_xdg(
        &["config", "run", "example.actions", "probe", "--json"],
        &xdg,
    );

    assert_eq!(
        code, 3,
        "action exit should become process exit; stderr={stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["plugin_id"], "example.actions");
    assert_eq!(value["action_id"], "probe");
    assert_eq!(value["outcome"], "completed");
    assert_eq!(value["exit_code"], 3);
    assert_eq!(value["stdout"], "out");
    assert_eq!(value["stderr"], "err");
}

#[test]
fn config_run_refuses_disabled_plugin_without_stdout() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = write_fixture(&tmp, false, "printf should-not-run");

    let (code, stdout, stderr) = run_with_xdg(&["config", "run", "example.actions", "probe"], &xdg);

    assert_ne!(code, 0);
    assert!(stdout.is_empty(), "disabled action must not write stdout");
    assert!(
        stderr.contains("disabled"),
        "diagnostic should name disabled plugin; stderr={stderr:?}"
    );
}

#[test]
fn config_run_timeout_returns_125_and_json_timeout() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = write_fixture(&tmp, true, "sleep 5");

    let (code, stdout, stderr) = run_with_xdg(
        &[
            "config",
            "run",
            "example.actions",
            "probe",
            "--timeout",
            "1",
            "--json",
        ],
        &xdg,
    );

    assert_eq!(
        code, 125,
        "timeout should use wrapper failure code; stderr={stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(value["outcome"], "timed_out");
    assert_eq!(value["exit_code"], serde_json::Value::Null);
}
