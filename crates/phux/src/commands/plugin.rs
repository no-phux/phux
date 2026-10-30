mod install;
mod json;
mod lock;
mod log;
mod registry;
mod scaffold;

use std::path::Path;
use std::process::ExitCode;

use phux_config::loader as config_loader;
use phux_config::plugin;

use crate::commands::PluginAction;
use json::{print_plugin_json, print_plugins_json, print_validation_json};
use registry::{
    RegistryEntry, find_entry, load_registry, load_registry_from_path, manifest_path_for_config,
    own_index, push_entry, read_config_document, reject_symlinked_config, remove_entry,
    set_enabled, update_entry, write_config_document,
};

pub(crate) fn run_plugin(action: &PluginAction) -> ExitCode {
    match action {
        PluginAction::List { json } => run_list(*json),
        PluginAction::Link {
            manifest,
            disabled,
            json,
        } => run_link(manifest, !disabled, *json),
        PluginAction::New { name, dir, json } => scaffold::run_new(name, dir.as_deref(), *json),
        PluginAction::Install {
            reference,
            rev,
            subdir,
            disabled,
            yes,
            json,
        } => install::run_install(
            reference,
            rev.as_deref(),
            subdir.as_deref(),
            *disabled,
            *yes,
            *json,
        ),
        PluginAction::Update {
            name,
            latest,
            yes,
            json,
        } => install::run_update(name.as_deref(), *latest, *yes, *json),
        PluginAction::Unlink { id, json } => run_unlink(id, *json),
        PluginAction::Enable { id, json } => run_set_enabled(id, true, *json),
        PluginAction::Disable { id, json } => run_set_enabled(id, false, *json),
        PluginAction::Validate { manifest, json } => run_validate(manifest.as_deref(), *json),
        PluginAction::Log {
            limit,
            failed,
            json,
            startup,
        } => log::run_log(*limit, *failed, *json, startup.as_ref().map(String::as_str)),
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
            // Only a table of this file's own is edited; an inherited entry
            // is left to its layer and gets one here instead.
            if let Some(own) = own_index(&config_path, &entry.manifest_path)? {
                update_entry(&mut doc, own, &stored_manifest, enabled)?;
                updated = true;
            }
            break;
        }
    }
    if !updated {
        push_entry(&mut doc, &stored_manifest, enabled)?;
    }
    write_config_document(&config_path, &doc)?;
    Ok(RegistryEntry {
        key: "plugins",
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
    if let Err(err) = remove_entry(&mut doc, entry.key, entry.index) {
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
    if let Err(err) = set_enabled(&mut doc, entry.key, entry.index, enabled) {
        return fail(json, &err);
    }
    if let Err(err) = write_config_document(&config_path, &doc) {
        return fail(json, &err);
    }
    entry.enabled = enabled;
    // `[[startup]]` sidecars reconcile from this doorbell. No server is
    // not a failure: the next start reads the file.
    signal_startup_reload(json);
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
    let manifest = match plugin::load_plugin_manifest(path) {
        Ok(manifest) => manifest,
        Err(err) => return fail(json, &format!("could not load {}: {err}", path.display())),
    };
    let entry = RegistryEntry {
        key: "plugins",
        index: 0,
        manifest_text: path.to_string_lossy().into_owned(),
        manifest_path: manifest.manifest_path.clone(),
        enabled: true,
        manifest,
    };
    report_validation(&[entry], json)
}

fn validate_registry(json: bool) -> ExitCode {
    match load_registry() {
        Ok(entries) => report_validation(&entries, json),
        Err(err) => fail(json, &err),
    }
}

/// Verdict for manifests that loaded: valid unless an entry loads but can
/// never act (an `[[events]]` hook whose `on` is not a hook event), which
/// the loader tolerates and the server only warns about.
fn report_validation(entries: &[RegistryEntry], json: bool) -> ExitCode {
    let problems = dead_entry_problems(entries);
    if !problems.is_empty() {
        return fail(json, &problems.join("; "));
    }
    if json {
        return print_validation_json(entries);
    }
    for entry in entries {
        outln!("valid {}", entry.manifest.id);
    }
    ExitCode::SUCCESS
}

/// One line per dead manifest entry, naming the manifest it is in.
fn dead_entry_problems(entries: &[RegistryEntry]) -> Vec<String> {
    entries
        .iter()
        .flat_map(|entry| {
            plugin::dead_events(&entry.manifest)
                .into_iter()
                .map(|dead| format!("{}: {dead}", entry.manifest_path.display()))
        })
        .collect()
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

/// Ring the config-reload doorbell so a running server reconciles
/// `[[startup]]` sidecars. A missing server is quiet: enable and disable
/// still succeed, and the next server start reads the file.
fn signal_startup_reload(json: bool) {
    let socket_path = phux_server::runtime::default_socket_path();
    let Ok(rt) = crate::commands::cli_runtime() else {
        return;
    };
    match rt.block_on(super::config::ring_config_reload(&socket_path)) {
        Ok(()) => {
            if !json {
                outln!("signalled the running server");
            }
        }
        Err(super::config::ReloadRingError::NoServer(_)) => {}
        Err(super::config::ReloadRingError::Unconfirmed(refusal)) => {
            error!("config reload could not be confirmed: {refusal}");
        }
    }
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
    error!("{message}");
    ExitCode::FAILURE
}
