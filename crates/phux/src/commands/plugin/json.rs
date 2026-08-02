use std::process::ExitCode;

use super::registry::RegistryEntry;

pub(super) fn print_plugins_json(entries: &[RegistryEntry]) -> ExitCode {
    let plugins: Vec<_> = entries.iter().map(plugin_json).collect();
    let doc = serde_json::json!({
        "schema_version": 1,
        "plugins": plugins,
    });
    print_json(&doc)
}

pub(super) fn print_plugin_json(key: &str, entry: &RegistryEntry) -> ExitCode {
    let doc = serde_json::json!({
        "schema_version": 1,
        key: plugin_json(entry),
    });
    print_json(&doc)
}

pub(super) fn print_validation_json(entries: &[RegistryEntry]) -> ExitCode {
    let plugins: Vec<_> = entries.iter().map(plugin_json).collect();
    let doc = serde_json::json!({
        "schema_version": 1,
        "valid": true,
        "plugins": plugins,
    });
    print_json(&doc)
}

fn plugin_json(entry: &RegistryEntry) -> serde_json::Value {
    serde_json::json!({
        "id": entry.manifest.id,
        "name": entry.manifest.name,
        "version": entry.manifest.version,
        "min_phux_version": entry.manifest.min_phux_version,
        "description": entry.manifest.description,
        "manifest": entry.manifest_text,
        "manifest_path": entry.manifest_path,
        "plugin_root": entry.manifest.plugin_root,
        "enabled": entry.enabled,
        "platforms": entry.manifest.platforms,
        "build": entry.manifest.build,
        "actions": entry.manifest.actions,
        "events": entry.manifest.events,
        "panes": entry.manifest.panes,
        "links": entry.manifest.links,
        "workspaces": entry.manifest.workspaces,
    })
}

pub(super) fn print_json(value: &serde_json::Value) -> ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(rendered) => {
            outln!("{rendered}");
            ExitCode::SUCCESS
        }
        // Only ever called on a `--json` path, so the failure is the
        // contract line (code `json_serialize`), never prose.
        Err(err) => crate::commands::json_err::emit(
            true,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::JSON_SERIALIZE,
                format!("could not render plugin JSON: {err}"),
                "this is a phux bug; run `phux doctor` and report it",
            ),
            1,
        ),
    }
}
