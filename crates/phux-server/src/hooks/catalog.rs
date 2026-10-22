//! What the dispatcher matches against: config hook entries plus enabled
//! plugins' event hooks, resolved once at server start.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use phux_config::plugin::{self, PluginManifest, PluginPlatform};
use phux_config::{Config, HookEntry, vocab};
use tracing::{debug, warn};

/// One `[[events]]` entry resolved from an **enabled** plugin manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginEventHook {
    /// Manifest-global plugin id.
    pub plugin_id: String,
    /// Plugin-local event hook id.
    pub event_id: String,
    /// Event name this hook observes (matched against [`super::HookEvent::name`]).
    pub on: String,
    /// Command argv to execute.
    pub command: Vec<String>,
    /// Directory containing the manifest; the hook's working directory.
    pub plugin_root: PathBuf,
}

/// Config hook entries plus enabled plugins' event hooks.
#[derive(Debug, Clone, Default)]
pub struct HookCatalog {
    /// `[[hooks.<name>]]` entries keyed by hook name.
    pub config_hooks: BTreeMap<String, Vec<HookEntry>>,
    /// Event hooks resolved from enabled plugin manifests.
    pub plugin_events: Vec<PluginEventHook>,
}

impl HookCatalog {
    /// Nothing to dispatch (the runtime then spawns no dispatcher).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.config_hooks.is_empty() && self.plugin_events.is_empty()
    }

    /// Build a catalog from a loaded config. Plugin manifest paths resolve
    /// against the config's directory; disabled plugins and other-platform
    /// events are skipped; unloadable manifests are logged. Config entries
    /// that can never act are warned about once but kept; plugin events whose
    /// `on` is not a hook event are warned about and left out.
    #[must_use]
    pub fn from_config(cfg: &Config, config_path: &Path) -> Self {
        for problem in offending_config_hooks(&cfg.hooks) {
            warn!(
                hook = %problem.label,
                "hooks: {}; run `phux config check`",
                problem.detail,
            );
        }
        let mut plugin_events = Vec::new();
        for entry in &cfg.plugins {
            if !entry.enabled {
                debug!(manifest = %entry.manifest.display(), "hooks: plugin disabled; skipping its events");
                continue;
            }
            let manifest_path = plugin::resolve_manifest_path(&entry.manifest, config_path);
            match plugin::load_plugin_manifest(&manifest_path) {
                Ok(manifest) => plugin_events.extend(manifest_event_hooks(manifest)),
                Err(err) => {
                    warn!(
                        manifest = %manifest_path.display(),
                        error = %err,
                        "hooks: could not load plugin manifest; skipping its events",
                    );
                }
            }
        }
        Self {
            config_hooks: cfg.hooks.clone(),
            plugin_events,
        }
    }
}

/// The event hooks one enabled manifest contributes on this platform. An
/// event that names no hook event would never match, so it is logged (the
/// same finding `phux plugin validate` refuses) and left out.
fn manifest_event_hooks(manifest: PluginManifest) -> Vec<PluginEventHook> {
    let mut hooks = Vec::new();
    for event in manifest.events {
        if !platform_enabled(event.platforms.as_deref()) {
            continue;
        }
        if let Some(dead) = plugin::dead_event(&event) {
            warn!(
                plugin = %manifest.id,
                manifest = %manifest.manifest_path.display(),
                "hooks: plugin {dead}; run `phux plugin validate`",
            );
            continue;
        }
        hooks.push(PluginEventHook {
            plugin_id: manifest.id.clone(),
            event_id: event.id,
            on: event.on,
            command: event.command,
            plugin_root: manifest.plugin_root.clone(),
        });
    }
    hooks
}

/// A config hook entry that can never act, with a log label.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct HookConfigProblem {
    pub(super) label: String,
    pub(super) detail: String,
}

/// Every config hook entry `phux config check` would flag, one record each,
/// using the same vocabulary (unknown event, unknown `when` keys, a
/// never-executable action).
pub(super) fn offending_config_hooks(
    hooks: &BTreeMap<String, Vec<HookEntry>>,
) -> Vec<HookConfigProblem> {
    let mut problems = Vec::new();
    for (event, entries) in hooks {
        let Some(context_keys) = vocab::hook_context_keys(event) else {
            let suggestion = vocab::did_you_mean(event, vocab::HOOK_EVENTS)
                .map(|hit| format!(" (did you mean `{hit}`?)"))
                .unwrap_or_default();
            problems.push(HookConfigProblem {
                label: format!("hooks.{event}"),
                detail: format!("unknown event `{event}`{suggestion}; its entries will never fire"),
            });
            continue;
        };
        for (index, entry) in entries.iter().enumerate() {
            let details = hook_entry_problems(event, context_keys, entry);
            if !details.is_empty() {
                problems.push(HookConfigProblem {
                    label: format!("hooks.{event}[{index}]"),
                    detail: details.join("; "),
                });
            }
        }
    }
    problems
}

/// Every reason one entry of a known `event` can never act.
fn hook_entry_problems(event: &str, context_keys: &[&str], entry: &HookEntry) -> Vec<String> {
    let mut details: Vec<String> = vocab::unknown_hook_when_keys(context_keys, &entry.when)
        .map(|key| {
            format!(
                "when key `{key}` can never match (`{event}` context keys: {})",
                context_keys.join(", "),
            )
        })
        .collect();
    if let Some(name) = vocab::dead_hook_action(&entry.action) {
        details.push(format!(
            "action `{name}` never executes server-side (only `run` with a usable \
             `command` does); a match still consumes the event"
        ));
    }
    details
}

/// Whether the current OS is allowed (`None` means every platform).
fn platform_enabled(platforms: Option<&[PluginPlatform]>) -> bool {
    let Some(platforms) = platforms else {
        return true;
    };
    current_platform().is_some_and(|current| platforms.contains(&current))
}

/// The manifest-vocabulary name of the OS this server runs on.
const fn current_platform() -> Option<PluginPlatform> {
    if cfg!(target_os = "macos") {
        Some(PluginPlatform::Macos)
    } else if cfg!(target_os = "linux") {
        Some(PluginPlatform::Linux)
    } else if cfg!(target_os = "windows") {
        Some(PluginPlatform::Windows)
    } else {
        None
    }
}
