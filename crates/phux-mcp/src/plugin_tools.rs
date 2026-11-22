//! `phux_plugin_action` and `phux_plugin_profile`: run one configured
//! plugin action, or list the profiles plugin manifests declare.

use std::path::PathBuf;
use std::time::Duration;

use phux_config::loader as config_loader;
use phux_config::plugin;
use phux_plugin::{ActionContext, ActionSource, PluginActionRequest};
use serde_json::{Value, json};

use crate::tools::{ToolError, ToolSpec, required_str, str_arg, strict_object};

pub(crate) const SPECS: &[ToolSpec] = &[
    ToolSpec::in_process("phux_plugin_action", action_schema, |args| {
        Box::pin(action(args))
    }),
    // A local config read with nothing to await.
    ToolSpec::in_process("phux_plugin_profile", profile_schema, |args| {
        Box::pin(std::future::ready(profile(args)))
    }),
];

/// The read-only profile tool's `config` argument, else the normal path.
fn config_path(args: &Value) -> PathBuf {
    str_arg(args, "config").map_or_else(config_loader::config_path, PathBuf::from)
}

async fn action(args: &Value) -> Result<Value, ToolError> {
    let request = action_request(args)?;
    let result = phux_plugin::run_configured_action_logged(&config_loader::config_path(), &request)
        .await
        .map_err(|err| ToolError::new(err.to_string()))?;
    serde_json::to_value(result)
        .map_err(|err| ToolError::new(format!("failed to serialize plugin action: {err}")))
}

fn action_request(args: &Value) -> Result<PluginActionRequest, ToolError> {
    strict_object(
        args,
        &[
            "plugin_id",
            "action_id",
            "timeout_secs",
            "action_target",
            "input",
        ],
        &["plugin_id", "action_id"],
    )?;
    let target = args
        .get("action_target")
        .map_or_else(
            || Ok(phux_plugin::ActionTarget::default()),
            |value| serde_json::from_value(value.clone()),
        )
        .map_err(|err| ToolError::new(format!("invalid action target: {err}")))?;
    let timeout = args
        .get("timeout_secs")
        .map(|v| {
            v.as_u64()
                .map(Duration::from_secs)
                .ok_or_else(|| ToolError::new("timeout_secs must be a nonnegative integer"))
        })
        .transpose()?;
    Ok(PluginActionRequest {
        plugin_id: required_str(args, "plugin_id")?.to_owned(),
        action_id: required_str(args, "action_id")?.to_owned(),
        timeout,
        cwd: None,
        context: ActionContext::supplied(
            ActionSource::Mcp,
            target,
            args.get("input").cloned().unwrap_or(Value::Null),
        ),
    })
}

fn action_schema() -> Value {
    json!({
        "name": "phux_plugin_action",
        "description": "Execute one locally configured plugin action on this machine. Runs argv directly from the plugin root; target facts never select a server or grant credentials.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "plugin_id": { "type": "string", "description": "Configured plugin id." },
                "action_id": { "type": "string", "description": "Plugin-local action id." },
                "timeout_secs": { "type": "integer", "minimum": 0, "description": "Give up after this many seconds. Omit to wait indefinitely." },
                "action_target": {
                    "type": "object",
                    "properties": {
                        "host": {"type":"string"}, "route": {"type":"string"},
                        "transport": {"type":"string"}, "satellite": {"type":"string"},
                        "authority": {"type":"string"}, "workspace": {"type":"string"},
                        "session": {"type":"string"}, "session_handle": {"type":"integer"},
                        "session_name": {"type":"string"}, "resource": {"type":"string"},
                        "resource_handle": {"type":"string"}, "activation": {"type":"integer"},
                        "instance": {"type":"string"}, "arrangement": {"type":"string"},
                        "group": {"type":"string"},
                        "entry": {"type":"string"}
                    },
                    "additionalProperties": false
                },
                "input": { "description": "Application-specific JSON input." }
            },
            "additionalProperties": false,
            "required": ["plugin_id", "action_id"]
        }
    })
}

fn profile(args: &Value) -> Result<Value, ToolError> {
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

fn profile_schema() -> Value {
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
command = ["sh", "-c", 'printf "%s" "$PHUX_ACTION_CONTEXT"']

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
        let request = action_request(&json!({
            "plugin_id": "example.bench",
            "action_id": "drive",
            "action_target": {"route": "hub", "satellite": "worker"},
            "input": {"job": "compile"},
        }))
        .expect("request");
        let result = phux_plugin::run_configured_action(&config, &request)
            .await
            .expect("runner succeeds");

        assert_eq!(result.plugin_id, "example.bench");
        assert_eq!(result.action_id, "drive");
        assert_eq!(result.exit_code, Some(0));
        let context: Value = serde_json::from_str(&result.stdout).expect("context");
        assert_eq!(context["source"], "mcp");
        assert_eq!(context["observed"], false);
        assert_eq!(context["target"]["route"], "hub");
        assert_eq!(context["target"]["satellite"], "worker");
        assert_eq!(context["input"]["job"], "compile");
        assert!(context.get("client").is_none());
    }

    #[test]
    fn plugin_action_tool_refuses_authority_overrides() {
        for key in ["config", "cwd", "socket", "client"] {
            let mut args = json!({"plugin_id": "example.bench", "action_id": "drive"});
            args[key] = json!("override");
            let error = action_request(&args).expect_err("override refused");
            assert!(error.0.contains(key));
        }
        let bogus_target = json!({
            "plugin_id": "example.bench",
            "action_id": "drive",
            "action_target": {"socket": "/tmp/foreign"}
        });
        let error = action_request(&bogus_target).expect_err("nested socket refused");
        assert!(error.0.contains("invalid action target"));
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
