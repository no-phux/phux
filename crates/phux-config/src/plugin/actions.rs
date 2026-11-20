//! Plugin action discovery and shortcut rules shared across clients.

use std::collections::BTreeMap;

use crate::keybind::{ResolvedAction, Resolver, parse_chord_sequence};
use crate::{Action, KeybindingsCfg, ParamAction};

use super::{PluginManifest, PluginManifestAction, PluginPlatform};

/// The action name used by plugin bindings and palette selections.
pub const PLUGIN_ACTION_NAME: &str = "plugin-action";

/// One platform-compatible action from the enabled plugin manifests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginActionEntry {
    /// Configured plugin id (manifest `id`).
    pub plugin_id: String,
    /// Human-readable plugin name (manifest `name`).
    pub plugin_name: String,
    /// Plugin-local action id.
    pub action_id: String,
    /// Human-readable action title.
    pub title: String,
    /// Optional manifest `keys` chord sequence for the prefix table.
    pub keys: Option<String>,
}

impl PluginActionEntry {
    /// The action committed by a palette selection or a merged keybinding.
    #[must_use]
    pub fn resolved_action(&self) -> ResolvedAction {
        let ParamAction { action, args } = self.parameterized_action();
        ResolvedAction { action, args }
    }

    fn parameterized_action(&self) -> ParamAction {
        ParamAction {
            action: PLUGIN_ACTION_NAME.to_owned(),
            args: plugin_args(&self.plugin_id, &self.action_id),
        }
    }
}

/// The [`PLUGIN_ACTION_NAME`] argument naming the plugin.
const PLUGIN_ARG: &str = "plugin";

/// The [`PLUGIN_ACTION_NAME`] argument naming the plugin's action. A config
/// binding can't spell it `action`, which names the action itself
/// (`{ action = "plugin-action", plugin = "x", id = "y" }`).
const PLUGIN_ACTION_ID_ARG: &str = "id";

/// The older spelling of [`PLUGIN_ACTION_ID_ARG`], still accepted from
/// callers that build the args directly.
const LEGACY_PLUGIN_ACTION_ID_ARG: &str = "action";

fn plugin_args(plugin_id: &str, action_id: &str) -> BTreeMap<String, toml::Value> {
    BTreeMap::from([
        (
            PLUGIN_ARG.to_owned(),
            toml::Value::String(plugin_id.to_owned()),
        ),
        (
            PLUGIN_ACTION_ID_ARG.to_owned(),
            toml::Value::String(action_id.to_owned()),
        ),
    ])
}

/// The `(plugin, action id)` a [`PLUGIN_ACTION_NAME`] invocation names:
/// `plugin` and `id`, or the older `action` in place of `id`. `None` when
/// either is missing or not a string.
#[must_use]
pub fn plugin_action_target(args: &BTreeMap<String, toml::Value>) -> Option<(&str, &str)> {
    let plugin = args.get(PLUGIN_ARG)?.as_str()?;
    let action_id = args
        .get(PLUGIN_ACTION_ID_ARG)
        .or_else(|| args.get(LEGACY_PLUGIN_ACTION_ID_ARG))?
        .as_str()?;
    Some((plugin, action_id))
}

/// Actions allowed by both the manifest and action platform lists, in manifest order.
pub fn supported_actions(
    manifests: &[PluginManifest],
) -> impl Iterator<Item = (&PluginManifest, &PluginManifestAction)> {
    manifests
        .iter()
        .filter(|manifest| platforms_allow(manifest.platforms.as_deref()))
        .flat_map(|manifest| {
            manifest
                .actions
                .iter()
                .filter(|action| platforms_allow(action.platforms.as_deref()))
                .map(move |action| (manifest, action))
        })
}

/// Snapshot supported actions for keybindings and palette entries.
#[must_use]
pub fn entries_from_manifests(manifests: &[PluginManifest]) -> Vec<PluginActionEntry> {
    supported_actions(manifests)
        .map(|(manifest, action)| PluginActionEntry {
            plugin_id: manifest.id.clone(),
            plugin_name: manifest.name.clone(),
            action_id: action.id.clone(),
            title: action.title.clone(),
            keys: action.keys.clone(),
        })
        .collect()
}

/// Whether a manifest platform list allows this machine. An omitted list allows every OS.
#[must_use]
pub fn platforms_allow(platforms: Option<&[PluginPlatform]>) -> bool {
    platforms.is_none_or(|platforms| {
        platforms.iter().any(|platform| match platform {
            PluginPlatform::Linux => cfg!(target_os = "linux"),
            PluginPlatform::Macos => cfg!(target_os = "macos"),
            PluginPlatform::Windows => cfg!(target_os = "windows"),
        })
    })
}

/// Merge plugin `keys` bindings into the prefix table.
///
/// User config wins: a colliding, unparsable, ambiguous, or `"none"`-unbound
/// chord is dropped with a warning (validated against a test [`Resolver`]).
/// Between plugins, the first in `[[plugins]]` order wins.
pub fn merge_plugin_bindings(kb: &mut KeybindingsCfg, entries: &[PluginActionEntry]) {
    for entry in entries {
        let Some(keys) = entry.keys.as_deref() else {
            continue;
        };
        if is_unbound(kb, keys) {
            tracing::warn!(
                plugin = %entry.plugin_id,
                action = %entry.action_id,
                keys,
                "plugin binding claims a key your config unbound with \"none\"; leaving it unbound",
            );
            continue;
        }
        if let Some(existing) = kb.prefix_table.get(keys) {
            tracing::warn!(
                plugin = %entry.plugin_id,
                action = %entry.action_id,
                keys,
                existing = ?existing,
                "plugin binding conflicts with an existing prefix-table entry; keeping the existing binding",
            );
            continue;
        }
        // Tentative insert + resolver validation catches unparsable chords,
        // differently spelled duplicates (`?` vs `S-/`), and ambiguous
        // prefixes.
        kb.prefix_table.insert(
            keys.to_owned(),
            Action::Parameterized(entry.parameterized_action()),
        );
        if let Err(err) = Resolver::new(kb) {
            kb.prefix_table.remove(keys);
            tracing::warn!(
                plugin = %entry.plugin_id,
                action = %entry.action_id,
                keys,
                error = %err,
                "plugin binding rejected (conflict or bad chord); user bindings win",
            );
        }
    }
}

/// `true` when `keys` names a prefix-table key the config unbound.
fn is_unbound(kb: &KeybindingsCfg, keys: &str) -> bool {
    parse_chord_sequence(keys).is_ok_and(|sequence| kb.unbound.contains(&sequence))
}

#[cfg(test)]
mod tests;
