#[test]
fn config_plugins_json_is_machine_readable() {
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

[[actions]]
id = "summarize"
title = "Summarize"
command = ["sh", "-c", "printf summarize"]

[[agents]]
id = "codex"
label = "Codex"
state = "blocked"
attention = "high"
contexts = ["workspace"]

[[panes]]
id = "board"
title = "Board"
command = ["true"]

[[profiles]]
id = "agent-bench"
title = "Agent Bench"
contexts = ["workspace"]
agents = ["codex"]
actions = ["summarize"]

[[profiles.panes]]
id = "board"
pane = "board"
role = "monitor"
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

    let (code, stdout, stderr) =
        run_with_xdg(&["config", "plugins", "--json"], &tmp.path().join("xdg"));

    assert_eq!(
        code, 0,
        "`config plugins --json` should exit 0; stderr={stderr}"
    );
    assert!(
        !stdout.contains(BANNER_FRAGMENT) && !stderr.contains(BANNER_FRAGMENT),
        "`config plugins --json` must not print the banner; stdout={stdout:?} stderr={stderr:?}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(value["plugins"][0]["id"], "example.agent-tools");
    assert_eq!(value["plugins"][0]["actions"][0]["id"], "summarize");
    assert_eq!(value["plugins"][0]["agents"][0]["id"], "codex");
    assert_eq!(value["plugins"][0]["agents"][0]["state"], "blocked");
    assert_eq!(value["plugins"][0]["profiles"][0]["id"], "agent-bench");
    assert_eq!(
        value["plugins"][0]["profiles"][0]["panes"][0]["role"],
        "monitor"
    );
    assert_eq!(value["plugins"][0]["enabled"], true);
}

#[test]
fn config_plugins_json_resolves_relative_manifest_paths() {
    let tmp = TempDir::new().expect("tempdir");
    let config_dir = tmp.path().join("xdg").join("phux");
    let plugin_dir = config_dir.join("plugins").join("agent-tools");
    std::fs::create_dir_all(&plugin_dir).expect("create plugin dir");
    std::fs::write(
        plugin_dir.join("phux-plugin.toml"),
        r#"
id = "example.relative"
name = "Relative"
version = "0.1.0"
min_phux_version = "0.0.2"

[[actions]]
id = "open"
title = "Open"
command = ["true"]
"#,
    )
    .expect("write manifest");
    std::fs::write(
        config_dir.join("config.toml"),
        r#"
[[plugins]]
manifest = "./plugins/agent-tools/phux-plugin.toml"
enabled = false
"#,
    )
    .expect("write config");

    let (code, stdout, stderr) =
        run_with_xdg(&["config", "plugins", "--json"], &tmp.path().join("xdg"));

    assert_eq!(
        code, 0,
        "`config plugins --json` should resolve relative manifests; stderr={stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(value["plugins"][0]["id"], "example.relative");
    assert_eq!(value["plugins"][0]["actions"][0]["id"], "open");
    assert_eq!(value["plugins"][0]["enabled"], false);
}
