#![allow(clippy::expect_used, reason = "tests")]

use std::process::Command;

use tempfile::TempDir;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");
/// The stderr build banner, now a plain `phux <version>` line, matched in
/// full so the absence assertions below stay meaningful.
const BANNER_FRAGMENT: &str = concat!("phux ", env!("CARGO_PKG_VERSION"));

fn run_with_xdg(args: &[&str], xdg_config_home: &std::path::Path) -> (i32, String, String) {
    let out = Command::new(PHUX)
        .env("XDG_CONFIG_HOME", xdg_config_home)
        .args(args)
        .output()
        .expect("run phux binary");
    (
        out.status.code().expect("phux exited via code, not signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn config_agents_json_projects_plugin_agent_state() {
    let tmp = TempDir::new().expect("tempdir");
    let plugin_dir = tmp.path().join("plugin");
    std::fs::create_dir_all(&plugin_dir).expect("create plugin dir");
    let manifest = plugin_dir.join("phux-plugin.toml");
    std::fs::write(
        &manifest,
        r#"
id = "example.agent-tools"
name = "Agent Tools"
version = "0.1.0"
min_phux_version = "0.0.2"

[[agents]]
id = "codex"
label = "Codex"
description = "Coding agent"
state = "working"
attention = "normal"
contexts = ["workspace", "pane"]
"#,
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
enabled = true
"#,
            manifest.display()
        ),
    )
    .expect("write config");

    // A socket path nothing listens on: the projection must degrade to
    // the declared manifest values instead of picking up whatever server
    // happens to run on the developer's default socket.
    let dead_socket = tmp.path().join("no-server.sock");
    let dead_socket = dead_socket.to_str().expect("utf-8 tmp path");
    let (code, stdout, stderr) = run_with_xdg(
        &["config", "agents", "--json", "--socket", dead_socket],
        &tmp.path().join("xdg"),
    );

    assert_eq!(
        code, 0,
        "`config agents --json` should exit 0; stderr={stderr}"
    );
    assert!(
        !stdout.contains(BANNER_FRAGMENT) && !stderr.contains(BANNER_FRAGMENT),
        "`config agents --json` must not print the banner; stdout={stdout:?} stderr={stderr:?}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["live"], false, "no server answered: {stdout}");
    assert_eq!(value["agents"][0]["plugin_id"], "example.agent-tools");
    assert_eq!(value["agents"][0]["id"], "codex");
    assert_eq!(value["agents"][0]["label"], "Codex");
    assert_eq!(value["agents"][0]["state"], "working");
    assert_eq!(value["agents"][0]["attention"], "normal");
    assert_eq!(value["agents"][0]["contexts"][0], "workspace");
    // phux-r82.10: schema_version 2 carries merge provenance — with no
    // server the effective values fall back to the declared baseline.
    assert_eq!(value["agents"][0]["source"], "manifest");
    assert_eq!(value["agents"][0]["declared"]["state"], "working");
    assert_eq!(value["agents"][0]["declared"]["attention"], "normal");
    assert_eq!(value["agents"][0]["runtime"], serde_json::Value::Null);
}
