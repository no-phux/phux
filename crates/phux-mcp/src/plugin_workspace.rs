use std::path::PathBuf;

use phux_config::loader as config_loader;
use phux_config::plugin;
use serde_json::{Value, json};

use crate::tools::ToolError;

pub(crate) fn call(args: &Value) -> Result<Value, ToolError> {
    let plugin_filter = str_arg(args, "plugin_id");
    let workspace_filter = str_arg(args, "workspace_id");
    let config_path =
        str_arg(args, "config").map_or_else(config_loader::config_path, PathBuf::from);
    let cfg =
        config_loader::load_from(&config_path).map_err(|err| ToolError::new(err.to_string()))?;

    let mut workspaces = Vec::new();
    for entry in cfg.plugins {
        let manifest_path = plugin::resolve_manifest_path(&entry.manifest, &config_path);
        let manifest = plugin::load_plugin_manifest(&manifest_path).map_err(|err| {
            ToolError::new(format!("could not load {}: {err}", manifest_path.display()))
        })?;
        if plugin_filter.is_some_and(|id| manifest.id.as_str() != id) {
            continue;
        }
        for workspace in manifest.workspaces {
            if workspace_filter.is_some_and(|id| workspace.id.as_str() != id) {
                continue;
            }
            workspaces.push(json!({
                "plugin_id": manifest.id,
                "plugin_name": manifest.name,
                "enabled": entry.enabled,
                "workspace": workspace,
            }));
        }
    }

    if workspaces.is_empty() && (plugin_filter.is_some() || workspace_filter.is_some()) {
        return Err(ToolError::new("no matching plugin workspace"));
    }
    Ok(json!({ "workspaces": workspaces, "count": workspaces.len() }))
}

pub(crate) fn schema() -> Value {
    json!({
        "name": "phux_plugin_workspace",
        "description": "List configured plugin workspace profiles: the manifest-level agents, actions, events, and pane roles that compose an agent bench.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "plugin_id": { "type": "string", "description": "Optional configured plugin id filter." },
                "workspace_id": { "type": "string", "description": "Optional plugin-local workspace id filter." },
                "config": { "type": "string", "description": "Override config.toml path. Defaults to the normal phux config path." }
            }
        }
    })
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture(tmp: &TempDir) -> PathBuf {
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&plugin_dir).expect("create plugin dir");
        std::fs::write(
            plugin_dir.join("phux-plugin.toml"),
            r#"
id = "example.bench"
name = "Agent Bench"
version = "0.1.0"
min_phux_version = "0.0.2"

[[agents]]
id = "codex"
label = "Codex"
state = "idle"
attention = "normal"

[[actions]]
id = "drive"
title = "Drive"
command = ["sh", "-c", "printf drive"]

[[events]]
id = "idle"
title = "Idle"
on = "idle"
command = ["sh", "-c", "printf idle"]

[[panes]]
id = "bench"
title = "Bench"
placement = "tab"
command = ["sh"]

[[workspaces]]
id = "agent-bench"
title = "Agent Bench"
agents = ["codex"]
actions = ["drive"]
events = ["idle"]

[[workspaces.panes]]
id = "bench-role"
pane = "bench"
role = "driver"
"#,
        )
        .expect("write manifest");
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            format!(
                r#"
[[plugins]]
manifest = "{}"
enabled = true
"#,
                plugin_dir.join("phux-plugin.toml").display()
            ),
        )
        .expect("write config");
        config_path
    }

    #[test]
    fn plugin_workspace_lists_manifest_profiles() {
        let tmp = TempDir::new().expect("tempdir");
        let config = fixture(&tmp);

        let result = call(&json!({
            "config": config,
            "plugin_id": "example.bench",
            "workspace_id": "agent-bench",
        }))
        .expect("tool succeeds");

        assert_eq!(result["count"], json!(1));
        let workspace = &result["workspaces"][0];
        assert_eq!(workspace["plugin_id"], json!("example.bench"));
        assert_eq!(workspace["enabled"], json!(true));
        assert_eq!(workspace["workspace"]["id"], json!("agent-bench"));
        assert_eq!(workspace["workspace"]["actions"], json!(["drive"]));
        assert_eq!(workspace["workspace"]["panes"][0]["role"], json!("driver"));
    }

    #[test]
    fn plugin_workspace_errors_on_filtered_miss() {
        let tmp = TempDir::new().expect("tempdir");
        let config = fixture(&tmp);

        let err = call(&json!({
            "config": config,
            "workspace_id": "missing",
        }))
        .expect_err("filtered miss errors");

        assert!(err.0.contains("no matching plugin workspace"));
    }
}
