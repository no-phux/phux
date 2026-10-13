//! `phux_plugin_action` and `phux_plugin_profile`: run one configured
//! plugin action, or list the profiles plugin manifests declare.

use std::path::PathBuf;
use std::time::Duration;

use phux_config::loader as config_loader;
use phux_config::plugin;
use serde_json::{Value, json};

use crate::tools::{ToolError, num_arg, required_str, str_arg};

/// The `config` argument, else the normal phux config path.
fn config_path(args: &Value) -> PathBuf {
    str_arg(args, "config").map_or_else(config_loader::config_path, PathBuf::from)
}

pub(crate) async fn action(args: &Value) -> Result<Value, ToolError> {
    let request = phux_plugin::PluginActionRequest {
        plugin_id: required_str(args, "plugin_id")?.to_owned(),
        action_id: required_str(args, "action_id")?.to_owned(),
        timeout: num_arg(args, "timeout_secs").map(Duration::from_secs),
        cwd: str_arg(args, "cwd").map(PathBuf::from),
    };
    let result = phux_plugin::run_configured_action_logged(&config_path(args), &request)
        .await
        .map_err(|err| ToolError::new(err.to_string()))?;
    serde_json::to_value(result)
        .map_err(|err| ToolError::new(format!("failed to serialize plugin action: {err}")))
}

pub(crate) fn action_schema() -> Value {
    json!({
        "name": "phux_plugin_action",
        "description": "Execute one action declared by a configured phux plugin manifest. Runs argv directly from the plugin root; no hidden shell expansion.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "plugin_id": { "type": "string", "description": "Configured plugin id." },
                "action_id": { "type": "string", "description": "Plugin-local action id." },
                "timeout_secs": { "type": "number", "description": "Give up after this many seconds. Omit to wait indefinitely." },
                "cwd": { "type": "string", "description": "Override cwd. Relative paths resolve under the plugin root." },
                "config": { "type": "string", "description": "Override config.toml path. Defaults to the normal phux config path." }
            },
            "required": ["plugin_id", "action_id"]
        }
    })
}

pub(crate) fn profile(args: &Value) -> Result<Value, ToolError> {
    let plugin_filter = str_arg(args, "plugin_id");
    let profile_filter = str_arg(args, "profile_id");
    let config_path = config_path(args);
    let cfg =
        config_loader::load_from(&config_path).map_err(|err| ToolError::new(err.to_string()))?;

    let mut profiles = Vec::new();
    for entry in cfg.plugins {
        let manifest_path = plugin::resolve_manifest_path(&entry.manifest, &config_path);
        let manifest = plugin::load_plugin_manifest(&manifest_path).map_err(|err| {
            ToolError::new(format!("could not load {}: {err}", manifest_path.display()))
        })?;
        if plugin_filter.is_some_and(|id| manifest.id.as_str() != id) {
            continue;
        }
        for profile in manifest.profiles {
            if profile_filter.is_some_and(|id| profile.id.as_str() != id) {
                continue;
            }
            profiles.push(json!({
                "plugin_id": manifest.id,
                "plugin_name": manifest.name,
                "enabled": entry.enabled,
                "profile": profile,
            }));
        }
    }

    if profiles.is_empty() && (plugin_filter.is_some() || profile_filter.is_some()) {
        return Err(ToolError::new("no matching plugin profile"));
    }
    Ok(json!({ "profiles": profiles, "count": profiles.len() }))
}

pub(crate) fn profile_schema() -> Value {
    json!({
        "name": "phux_plugin_profile",
        "description": "List configured plugin profiles: the manifest-level agents, actions, events, and pane roles that compose an agent bench.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "plugin_id": { "type": "string", "description": "Optional configured plugin id filter." },
                "profile_id": { "type": "string", "description": "Optional plugin-local profile id filter." },
                "config": { "type": "string", "description": "Override config.toml path. Defaults to the normal phux config path." }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const MANIFEST: &str = r#"
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
command = ["sh", "-c", "printf mcp"]

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

[[profiles]]
id = "agent-bench"
title = "Agent Bench"
agents = ["codex"]
actions = ["drive"]
events = ["idle"]

[[profiles.panes]]
id = "bench-role"
pane = "bench"
role = "driver"
"#;

    /// A config enabling one plugin with [`MANIFEST`].
    fn fixture(tmp: &TempDir) -> PathBuf {
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&plugin_dir).expect("create plugin dir");
        let manifest = plugin_dir.join("phux-plugin.toml");
        std::fs::write(&manifest, MANIFEST).expect("write manifest");
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            format!(
                "[[plugins]]\nmanifest = \"{}\"\nenabled = true\n",
                manifest.display()
            ),
        )
        .expect("write config");
        config_path
    }

    #[tokio::test]
    async fn plugin_action_tool_executes_configured_action() {
        let tmp = TempDir::new().expect("tempdir");
        let config = fixture(&tmp);
        let result = action(&json!({
            "plugin_id": "example.bench",
            "action_id": "drive",
            "config": config,
        }))
        .await
        .expect("tool succeeds");

        assert_eq!(result["plugin_id"], "example.bench");
        assert_eq!(result["action_id"], "drive");
        assert_eq!(result["outcome"], "completed");
        assert_eq!(result["exit_code"], 0);
        assert_eq!(result["stdout"], "mcp");
    }

    #[test]
    fn plugin_profile_lists_manifest_profiles_and_errors_on_a_filtered_miss() {
        let tmp = TempDir::new().expect("tempdir");
        let config = fixture(&tmp);

        let result = profile(&json!({
            "config": config,
            "plugin_id": "example.bench",
            "profile_id": "agent-bench",
        }))
        .expect("tool succeeds");
        assert_eq!(result["count"], json!(1));
        let listed = &result["profiles"][0];
        assert_eq!(listed["plugin_id"], json!("example.bench"));
        assert_eq!(listed["enabled"], json!(true));
        assert_eq!(listed["profile"]["id"], json!("agent-bench"));
        assert_eq!(listed["profile"]["actions"], json!(["drive"]));
        assert_eq!(listed["profile"]["panes"][0]["role"], json!("driver"));

        let err = profile(&json!({ "config": config, "profile_id": "missing" }))
            .expect_err("filtered miss errors");
        assert!(err.0.contains("no matching plugin profile"));
    }
}
