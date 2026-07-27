use std::path::Path;
use std::process::ExitCode;

use phux_config::{ConfigProvenance, LayerSource};

use super::LoadedPlugin;
use super::live_feed::AgentProjection;

/// `phux config show --layers --json`: the provenance document.
///
/// Layer indexes are 1-based and shared between the `layers` and
/// `keys` sections. Scalar keys carry `layer` only; array keys add
/// `element_layers`, one entry per element in order.
pub(super) fn print_layers_json(config_path: &Path, provenance: &ConfigProvenance) -> ExitCode {
    let layers: Vec<_> = provenance
        .layers
        .iter()
        .enumerate()
        .map(|(i, layer)| {
            let (kind, path) = match layer {
                LayerSource::Defaults => ("defaults", None),
                LayerSource::Extended(p) => ("extended", Some(p.display().to_string())),
                LayerSource::User(p) => ("user", Some(p.display().to_string())),
            };
            serde_json::json!({
                "index": i + 1,
                "kind": kind,
                "path": path,
            })
        })
        .collect();
    let keys: Vec<_> = provenance
        .keys
        .iter()
        .map(|(key, origin)| {
            let mut entry = serde_json::Map::new();
            entry.insert("key".to_owned(), serde_json::json!(key));
            entry.insert("layer".to_owned(), serde_json::json!(origin.layer + 1));
            if let Some(elements) = &origin.elements {
                let element_layers: Vec<_> = elements.iter().map(|layer| layer + 1).collect();
                entry.insert(
                    "element_layers".to_owned(),
                    serde_json::json!(element_layers),
                );
            }
            serde_json::Value::Object(entry)
        })
        .collect();
    print_json(
        &serde_json::json!({
            "schema_version": 1,
            "config_path": config_path.display().to_string(),
            "layers": layers,
            "keys": keys,
        }),
        "config layers",
    )
}

pub(super) fn print_plugins_json(plugins: &[LoadedPlugin]) -> ExitCode {
    let plugins: Vec<_> = plugins.iter().map(plugin_json).collect();
    print_json(
        &serde_json::json!({
            "schema_version": 1,
            "plugins": plugins,
        }),
        "plugins",
    )
}

/// The merged agents document (phux-r82.10). `schema_version` bumped
/// 1 -> 2 when the rows grew `source` / `declared` / `runtime` and
/// `state`/`attention` became the *effective* (runtime-first) values; the
/// document also gained the top-level `live` flag. See
/// `docs/consumers/agents.md` section 4.6 for the schema notes.
pub(super) fn print_agents_json(agents: &[AgentProjection], live: bool) -> ExitCode {
    print_json(
        &serde_json::json!({
            "schema_version": 2,
            "live": live,
            "agents": agents,
        }),
        "agents",
    )
}

fn plugin_json(plugin: &LoadedPlugin) -> serde_json::Value {
    let manifest = &plugin.manifest;
    serde_json::json!({
        "id": manifest.id,
        "name": manifest.name,
        "version": manifest.version,
        "min_phux_version": manifest.min_phux_version,
        "description": manifest.description,
        "manifest_path": manifest.manifest_path,
        "plugin_root": manifest.plugin_root,
        "enabled": plugin.enabled,
        "platforms": manifest.platforms,
        "build": manifest.build,
        "agents": manifest.agents,
        "actions": manifest.actions,
        "events": manifest.events,
        "panes": manifest.panes,
        "links": manifest.links,
        "workspaces": manifest.workspaces,
    })
}

fn print_json(value: &serde_json::Value, label: &str) -> ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(rendered) => {
            outln!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("phux: could not render {label} JSON: {err}");
            ExitCode::FAILURE
        }
    }
}
