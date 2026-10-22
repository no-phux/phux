//! `phux config keys`: the key bindings in effect and the layer that set each.

use std::collections::BTreeMap;
use std::process::ExitCode;

use phux_config::keybind::{BindingDiagnostic, Resolver, binding_layer};
use phux_config::loader as config_loader;
use phux_config::{Action, ConfigProvenance, KeybindingsCfg, LayerSource};
use phux_tui::attach::plugin_actions;

/// One effective binding, as printed.
struct Row {
    table: &'static str,
    keys: String,
    action: String,
    args: BTreeMap<String, toml::Value>,
    source: String,
}

/// `phux config keys [ACTION] [--json]`.
pub(super) fn run_config_keys(filter: Option<&str>, json: bool) -> ExitCode {
    let path = config_loader::config_path();
    let user_input = match std::fs::read_to_string(&path) {
        Ok(body) => body,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            error!("could not read {}: {err}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let loaded = phux_config::parse_with_defaults(&user_input, &path).and_then(|cfg| {
        phux_config::merged_config_with_provenance(&user_input, &path)
            .map(|(_, provenance)| (cfg, provenance))
    });
    let (cfg, provenance) = match loaded {
        Ok(loaded) => loaded,
        Err(err) => {
            error!("{err}");
            return ExitCode::FAILURE;
        }
    };

    // Plugin keys merge exactly as the terminal UI merges them.
    let manifests = if cfg.plugins.is_empty() {
        Vec::new()
    } else {
        phux_config::plugin::load_enabled_manifests(&path, &cfg.plugins)
    };
    let mut keybindings = cfg.keybindings;
    plugin_actions::merge_plugin_bindings(
        &mut keybindings,
        &plugin_actions::entries_from_manifests(&manifests),
    );
    let (_, skipped) = Resolver::new_lenient(&keybindings);

    let prefix_source = binding_source(&provenance, "keybindings", "prefix", None);
    let rows: Vec<Row> = rows(&keybindings, &provenance)
        .into_iter()
        .filter(|row| filter.is_none_or(|action| row.action == action))
        .collect();
    if json {
        return print_json(&keybindings.prefix, &prefix_source, &rows, &skipped);
    }
    print_human(&keybindings.prefix, &prefix_source, &rows, filter);
    for diagnostic in &skipped {
        warn!("skipped {diagnostic}");
    }
    ExitCode::SUCCESS
}

/// Global bindings first, then the prefix table, each in key order.
fn rows(keybindings: &KeybindingsCfg, provenance: &ConfigProvenance) -> Vec<Row> {
    let tables = [
        ("global", &keybindings.global, None),
        (
            "prefix-table",
            &keybindings.prefix_table,
            Some(&keybindings.prefix),
        ),
    ];
    let mut rows = Vec::new();
    for (table, bindings, prefix) in tables {
        let table_path = format!("keybindings.{table}");
        for (binding, action) in bindings {
            let (action, args) = match action {
                Action::Bare(name) => (name.clone(), BTreeMap::new()),
                Action::Parameterized(p) => (p.action.clone(), p.args.clone()),
            };
            let plugin = (action == "plugin-action")
                .then(|| args.get("plugin").and_then(toml::Value::as_str))
                .flatten();
            rows.push(Row {
                table,
                keys: prefix.map_or_else(|| binding.clone(), |p| format!("{p} {binding}")),
                source: binding_source(provenance, &table_path, binding, plugin),
                action,
                args,
            });
        }
    }
    rows
}

/// The layer that set a binding (`defaults`, a layer's file name), or the
/// plugin that contributed it.
fn binding_source(
    provenance: &ConfigProvenance,
    table_path: &str,
    binding: &str,
    plugin: Option<&str>,
) -> String {
    let layer = binding_layer(provenance, table_path, binding)
        .and_then(|layer| provenance.layers.get(layer));
    match (layer, plugin) {
        (Some(LayerSource::Defaults), _) => "defaults".to_owned(),
        (Some(LayerSource::Extended(path) | LayerSource::User(path)), _) => {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
        }
        (None, Some(plugin)) => format!("plugin {plugin}"),
        (None, None) => "plugin".to_owned(),
    }
}

/// `direction=vertical amount=5`, strings unquoted.
fn render_args(args: &BTreeMap<String, toml::Value>) -> String {
    args.iter()
        .map(|(key, value)| match value {
            toml::Value::String(s) => format!("{key}={s}"),
            other => format!("{key}={other}"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn print_human(prefix: &str, prefix_source: &str, rows: &[Row], filter: Option<&str>) {
    outln!("prefix  {prefix}  ({prefix_source})");
    outln!();
    if rows.is_empty() {
        match filter {
            Some(action) => outln!("no key is bound to {action}"),
            None => outln!("no key bindings"),
        }
        return;
    }
    let args: Vec<String> = rows.iter().map(|row| render_args(&row.args)).collect();
    let keys_w = rows.iter().map(|r| r.keys.len()).max().unwrap_or(0).max(3);
    let action_w = rows
        .iter()
        .map(|r| r.action.len())
        .max()
        .unwrap_or(0)
        .max(6);
    let args_w = args.iter().map(String::len).max().unwrap_or(0).max(4);
    outln!(
        "{:<keys_w$}  {:<action_w$}  {:<args_w$}  SOURCE",
        "KEY",
        "ACTION",
        "ARGS"
    );
    for (row, args) in rows.iter().zip(&args) {
        outln!(
            "{:<keys_w$}  {:<action_w$}  {:<args_w$}  {}",
            row.keys,
            row.action,
            args,
            row.source
        );
    }
}

fn print_json(
    prefix: &str,
    prefix_source: &str,
    rows: &[Row],
    skipped: &[BindingDiagnostic],
) -> ExitCode {
    let bindings: Vec<_> = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "table": row.table,
                "keys": row.keys,
                "action": row.action,
                "args": row.args,
                "source": row.source,
            })
        })
        .collect();
    let skipped: Vec<_> = skipped
        .iter()
        .map(|d| serde_json::json!({ "binding": d.binding, "error": d.error.to_string() }))
        .collect();
    crate::output::json(&serde_json::json!({
        "schema_version": 1,
        "prefix": { "keys": prefix, "source": prefix_source },
        "bindings": bindings,
        "skipped": skipped,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows_for(user: &str) -> Vec<Row> {
        let path = std::path::Path::new("/tmp/phux-keys-test/config.toml");
        let cfg = phux_config::parse_with_defaults(user, path).expect("parses");
        let (_, provenance) =
            phux_config::merged_config_with_provenance(user, path).expect("merges");
        rows(&cfg.keybindings, &provenance)
    }

    #[test]
    fn shipped_bindings_are_attributed_to_the_defaults() {
        let rows = rows_for("");
        let zoom = rows.iter().find(|r| r.keys == "C-a z").expect("prefix z");
        assert_eq!(
            (zoom.action.as_str(), zoom.source.as_str()),
            ("toggle-zoom", "defaults")
        );
        let split = rows.iter().find(|r| r.keys == "C-a %").expect("prefix %");
        assert_eq!(render_args(&split.args), "direction=vertical");
    }

    #[test]
    fn your_bindings_name_your_file_and_unbound_keys_disappear() {
        let rows = rows_for(
            "[keybindings]\nprefix = \"C-b\"\n[keybindings.global]\n\"M-Enter\" = \"toggle-zoom\"\n\
             [keybindings.prefix-table]\nx = \"none\"\n",
        );
        let global = rows.iter().find(|r| r.keys == "M-Enter").expect("global");
        assert_eq!(
            (global.table, global.source.as_str()),
            ("global", "config.toml")
        );
        assert!(rows.iter().any(|r| r.keys == "C-b z"), "prefix applies");
        assert!(!rows.iter().any(|r| r.keys == "C-b x"), "x unbound");
    }
}
