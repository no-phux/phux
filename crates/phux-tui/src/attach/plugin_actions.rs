//! Plugin manifest `[[actions]]` in the TUI, client-local (ADR-0017).
//!
//! [`entries_from_manifests`] flattens the load-once manifest snapshot, the
//! palette lists them under a "Plugin" header, [`merge_plugin_bindings`]
//! folds their `keys` into the prefix table (user config always wins), and
//! `plugin_job` runs one through the `phux-plugin` runtime off
//! the input loop, toasting failures (`failure_toast`). Rows and bindings
//! both commit [`PLUGIN_ACTION_NAME`], sharing the `run_action` path.

use std::collections::BTreeMap;
use std::time::Duration;

use phux_config::keybind::{ResolvedAction, Resolver, parse_chord_sequence};
use phux_config::plugin::PluginManifest;
use phux_config::{Action, KeybindingsCfg, ParamAction};

use super::jobs::{Job, JobDone, JobKind, Toast};

/// The dispatcher action plugin palette rows and merged bindings commit
/// (built dynamically, so exempt from the static registry).
pub const PLUGIN_ACTION_NAME: &str = "plugin-action";

/// Cap on a TUI-triggered action's runtime: it runs detached, so a hung
/// plugin must not leak a child (`phux-plugin` kills it on timeout).
const PLUGIN_ACTION_TIMEOUT: Duration = Duration::from_secs(30);

/// One enabled plugin action, snapshotted at driver start.
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
    /// The palette row label: namespaced so plugin rows can't be mistaken
    /// for built-in actions.
    #[must_use]
    pub fn palette_label(&self) -> String {
        format!("plugin: {}: {}", self.plugin_name, self.title)
    }

    /// The [`ResolvedAction`] this entry commits — the same shape a
    /// keybinding or palette row produces, flowing through `run_action`.
    #[must_use]
    pub fn resolved_action(&self) -> ResolvedAction {
        ResolvedAction {
            action: PLUGIN_ACTION_NAME.to_owned(),
            args: plugin_args(&self.plugin_id, &self.action_id),
        }
    }

    /// The equivalent config-schema [`Action`], for prefix-table merging.
    fn config_action(&self) -> Action {
        Action::Parameterized(ParamAction {
            action: PLUGIN_ACTION_NAME.to_owned(),
            args: plugin_args(&self.plugin_id, &self.action_id),
        })
    }
}

/// The [`PLUGIN_ACTION_NAME`] argument naming the plugin.
pub const PLUGIN_ARG: &str = "plugin";

/// The [`PLUGIN_ACTION_NAME`] argument naming the plugin's action. A config
/// binding can't spell it `action`, which names the action itself
/// (`{ action = "plugin-action", plugin = "x", id = "y" }`).
pub const PLUGIN_ACTION_ID_ARG: &str = "id";

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

/// Flatten loaded manifests into per-action entries (pure; the driver does
/// the loading).
#[must_use]
pub fn entries_from_manifests(manifests: &[PluginManifest]) -> Vec<PluginActionEntry> {
    manifests
        .iter()
        .flat_map(|manifest| {
            manifest.actions.iter().map(|action| PluginActionEntry {
                plugin_id: manifest.id.clone(),
                plugin_name: manifest.name.clone(),
                action_id: action.id.clone(),
                title: action.title.clone(),
                keys: action.keys.clone(),
            })
        })
        .collect()
}

/// Merge plugin `keys` bindings into the prefix table.
///
/// User config always wins: a chord that collides, fails to parse, or is ambiguous with an
/// existing binding is dropped with a warning (validated by test-building a
/// [`Resolver`], so a bad plugin binding cannot disable every binding), and
/// so is a key the config unbound with `"none"`
/// ([`KeybindingsCfg::unbound`], in any spelling).
///
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
        kb.prefix_table
            .insert(keys.to_owned(), entry.config_action());
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

/// The outcome of one plugin action run, as its failure toast reads it.
#[derive(Debug)]
pub struct PluginRunResult {
    /// Configured plugin id.
    pub plugin_id: String,
    /// Plugin-local action id.
    pub action_id: String,
    /// The runtime's structured output, or a stringified pre-exec error
    /// (config/manifest load failure, unknown plugin/action, spawn
    /// failure).
    pub result: Result<phux_plugin::PluginActionOutput, String>,
}

/// One plugin action as a background job (ADR-0172), run through
/// [`phux_plugin::run_configured_action_logged`] (the `phux config run`
/// runtime, which also records the run for `phux plugin log`). A failure
/// finishes with its toast; a success only logs.
pub(in crate::attach) fn plugin_job(plugin_id: String, action_id: String) -> Job {
    Job::new(
        JobKind::PluginAction,
        run_plugin_action(plugin_id, action_id),
    )
}

async fn run_plugin_action(plugin_id: String, action_id: String) -> JobDone {
    let config_path = phux_config::loader::config_path();
    let request = phux_plugin::PluginActionRequest {
        plugin_id: plugin_id.clone(),
        action_id: action_id.clone(),
        timeout: Some(PLUGIN_ACTION_TIMEOUT),
        cwd: None,
    };
    let result = PluginRunResult {
        result: phux_plugin::run_configured_action_logged(&config_path, &request)
            .await
            .map_err(|err| err.to_string()),
        plugin_id,
        action_id,
    };
    tracing::info!(
        plugin = %result.plugin_id,
        action = %result.action_id,
        ok = run_succeeded(&result),
        "plugin action finished",
    );
    failure_toast(&result).map_or_else(JobDone::quiet, JobDone::toast)
}

/// `true` when the run completed with exit code 0.
#[must_use]
pub fn run_succeeded(result: &PluginRunResult) -> bool {
    matches!(
        &result.result,
        Ok(output)
            if output.outcome == phux_plugin::PluginActionOutcome::Completed
                && output.exit_code == Some(0)
    )
}

/// Cap on toast body lines; output beyond it is elided (head is dropped —
/// the tail of stderr usually carries the actual error).
const TOAST_MAX_LINES: usize = 12;

/// Build the failure toast for a finished run, or `None` when the run
/// succeeded (successes only log — no modal to dismiss for the happy path).
#[must_use]
pub(in crate::attach) fn failure_toast(result: &PluginRunResult) -> Option<Toast> {
    if run_succeeded(result) {
        return None;
    }
    let mut lines = vec![format!("{} {}", result.plugin_id, result.action_id)];
    match &result.result {
        Err(message) => lines.push(message.clone()),
        Ok(output) => {
            match output.outcome {
                phux_plugin::PluginActionOutcome::TimedOut => {
                    lines.push(format!(
                        "timed out after {}s (process killed)",
                        PLUGIN_ACTION_TIMEOUT.as_secs()
                    ));
                }
                phux_plugin::PluginActionOutcome::Completed => {
                    lines.push(output.exit_code.map_or_else(
                        || "killed by signal".to_owned(),
                        |code| format!("exit code {code}"),
                    ));
                }
            }
            append_output_tail(&mut lines, "stderr", &output.stderr);
            append_output_tail(&mut lines, "stdout", &output.stdout);
        }
    }
    Some(Toast::lines("Plugin action failed", lines))
}

/// Append the tail of a captured stream to `lines`, respecting
/// [`TOAST_MAX_LINES`]. Empty streams contribute nothing.
fn append_output_tail(lines: &mut Vec<String>, label: &str, captured: &str) {
    let captured = captured.trim_end();
    if captured.is_empty() || lines.len() >= TOAST_MAX_LINES {
        return;
    }
    lines.push(format!("--- {label} ---"));
    let all: Vec<&str> = captured.lines().collect();
    let budget = TOAST_MAX_LINES.saturating_sub(lines.len());
    let skipped = all.len().saturating_sub(budget);
    if skipped > 0 {
        lines.push(format!("[... {skipped} earlier lines elided ...]"));
    }
    for line in all.iter().skip(skipped).take(budget) {
        lines.push((*line).to_owned());
    }
}

#[cfg(test)]
mod tests;
