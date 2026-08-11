mod install;
mod json;
mod lock;
mod registry;

use std::path::Path;
use std::process::ExitCode;

use phux_config::loader as config_loader;
use phux_config::plugin;

use crate::commands::PluginAction;
use json::{print_plugin_json, print_plugins_json, print_validation_json};
use registry::{
    RegistryEntry, find_entry, load_registry, load_registry_from_path, manifest_path_for_config,
    push_entry, read_config_document, reject_symlinked_config, remove_entry, set_enabled,
    update_entry, write_config_document,
};

pub(crate) fn run_plugin(action: &PluginAction) -> ExitCode {
    match action {
        PluginAction::List { json } => run_list(*json),
        PluginAction::Link {
            manifest,
            disabled,
            json,
        } => run_link(manifest, !disabled, *json),
        PluginAction::Install {
            reference,
            rev,
            disabled,
            json,
        } => install::run_install(reference, rev.as_deref(), *disabled, *json),
        PluginAction::Update { name, json } => install::run_update(name.as_deref(), *json),
        PluginAction::Unlink { id, json } => run_unlink(id, *json),
        PluginAction::Enable { id, json } => run_set_enabled(id, true, *json),
        PluginAction::Disable { id, json } => run_set_enabled(id, false, *json),
        PluginAction::Validate { manifest, json } => run_validate(manifest.as_deref(), *json),
    }
}

fn run_list(json: bool) -> ExitCode {
    match load_registry() {
        Ok(entries) if json => print_plugins_json(&entries),
        Ok(entries) => {
            if entries.is_empty() {
                outln!("No plugins configured. Install one with `phux plugin install SOURCE`.");
                return ExitCode::SUCCESS;
            }
            for entry in entries {
                let state = if entry.enabled { "enabled" } else { "disabled" };
                outln!("{} {} ({state})", entry.manifest.id, entry.manifest.version);
            }
            ExitCode::SUCCESS
        }
        Err(err) => fail(json, &err),
    }
}

fn run_link(manifest_arg: &Path, enabled: bool, json: bool) -> ExitCode {
    let manifest = match plugin::load_plugin_manifest(manifest_arg) {
        Ok(manifest) => manifest,
        Err(err) => {
            return fail(
                json,
                &format!("could not load {}: {err}", manifest_arg.display()),
            );
        }
    };
    let entry = match upsert_config_entry(manifest, enabled) {
        Ok(entry) => entry,
        Err(err) => return fail(json, &err),
    };
    if json {
        print_plugin_json("plugin", &entry)
    } else {
        let state = if enabled { "enabled" } else { "disabled" };
        outln!("linked {} ({state})", entry.manifest.id);
        ExitCode::SUCCESS
    }
}

/// Add or update `manifest`'s `[[plugins]]` entry in `config.toml` (the
/// shared write path behind `phux plugin link` and `phux plugin install`).
fn upsert_config_entry(
    manifest: plugin::PluginManifest,
    enabled: bool,
) -> Result<RegistryEntry, String> {
    let config_path = config_loader::config_path();
    let stored_manifest = manifest_path_for_config(&manifest.manifest_path, &config_path);
    reject_symlinked_config(&config_path)?;
    let mut doc = read_config_document(&config_path)?;
    let mut updated = false;
    for entry in load_registry_from_path(&config_path)? {
        if entry.manifest.id == manifest.id {
            update_entry(&mut doc, entry.index, &stored_manifest, enabled)?;
            updated = true;
            break;
        }
    }
    if !updated {
        push_entry(&mut doc, &stored_manifest, enabled)?;
    }
    write_config_document(&config_path, &doc)?;
    Ok(RegistryEntry {
        index: 0,
        manifest_text: stored_manifest,
        manifest_path: manifest.manifest_path.clone(),
        enabled,
        manifest,
    })
}

fn run_unlink(id: &str, json: bool) -> ExitCode {
    let config_path = config_loader::config_path();
    if let Err(err) = reject_symlinked_config(&config_path) {
        return fail(json, &err);
    }
    let entry = match find_entry(&config_path, id) {
        Ok(entry) => entry,
        Err(err) => return fail(json, &err),
    };
    let mut doc = match read_config_document(&config_path) {
        Ok(doc) => doc,
        Err(err) => return fail(json, &err),
    };
    if let Err(err) = remove_entry(&mut doc, entry.index) {
        return fail(json, &err);
    }
    if let Err(err) = write_config_document(&config_path, &doc) {
        return fail(json, &err);
    }
    if json {
        print_plugin_json("removed", &entry)
    } else {
        outln!("unlinked {}", entry.manifest.id);
        ExitCode::SUCCESS
    }
}

fn run_set_enabled(id: &str, enabled: bool, json: bool) -> ExitCode {
    let config_path = config_loader::config_path();
    if let Err(err) = reject_symlinked_config(&config_path) {
        return fail(json, &err);
    }
    let mut entry = match find_entry(&config_path, id) {
        Ok(entry) => entry,
        Err(err) => return fail(json, &err),
    };
    let mut doc = match read_config_document(&config_path) {
        Ok(doc) => doc,
        Err(err) => return fail(json, &err),
    };
    if let Err(err) = set_enabled(&mut doc, entry.index, enabled) {
        return fail(json, &err);
    }
    if let Err(err) = write_config_document(&config_path, &doc) {
        return fail(json, &err);
    }
    entry.enabled = enabled;
    if json {
        print_plugin_json("plugin", &entry)
    } else {
        let state = if enabled { "enabled" } else { "disabled" };
        outln!("{} {state}", entry.manifest.id);
        ExitCode::SUCCESS
    }
}

fn run_validate(manifest_arg: Option<&Path>, json: bool) -> ExitCode {
    manifest_arg.map_or_else(
        || validate_registry(json),
        |path| validate_manifest(path, json),
    )
}

fn validate_manifest(path: &Path, json: bool) -> ExitCode {
    match plugin::load_plugin_manifest(path) {
        Ok(manifest) if json => {
            let entry = RegistryEntry {
                index: 0,
                manifest_text: path.to_string_lossy().into_owned(),
                manifest_path: manifest.manifest_path.clone(),
                enabled: true,
                manifest,
            };
            print_validation_json(&[entry])
        }
        Ok(manifest) => {
            outln!("valid {}", manifest.id);
            ExitCode::SUCCESS
        }
        Err(err) => fail(json, &format!("could not load {}: {err}", path.display())),
    }
}

fn validate_registry(json: bool) -> ExitCode {
    match load_registry() {
        Ok(entries) if json => print_validation_json(&entries),
        Ok(entries) => {
            for entry in entries {
                outln!("valid {}", entry.manifest.id);
            }
            ExitCode::SUCCESS
        }
        Err(err) => fail(json, &err),
    }
}

/// How many configured plugin manifests load cleanly.
///
/// Exposed for `phux doctor`, which needs the verdict and not the entries.
/// Returning a count rather than `Vec<RegistryEntry>` keeps the registry's
/// internals private to this module — a diagnostic has no business reaching
/// into them.
pub(crate) fn valid_manifest_count() -> Result<usize, String> {
    load_registry().map(|entries| entries.len())
}

/// Report a plugin-registry failure: the historical prose line without
/// `--json` (byte-identical, so scripts that grep stderr keep working), or
/// one line of the shared JSON error contract with it (phux-i0e8.8.3).
pub(super) fn fail(json: bool, message: &str) -> ExitCode {
    if json {
        return crate::commands::json_err::emit(
            true,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::REGISTRY,
                message,
                "run `phux plugin list` to see configured plugins; \
                 `phux config path` names the config file",
            ),
            1,
        );
    }
    eprintln!("phux: {message}");
    ExitCode::FAILURE
}
