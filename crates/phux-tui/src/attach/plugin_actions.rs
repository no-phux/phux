//! Plugin actions in the TUI (phux-r82.5).
//!
//! Plugin manifest `[[actions]]` were previously reachable only through
//! `phux config run PLUGIN ACTION` and the MCP tool — nothing in the TUI
//! surfaced them. This module makes enabled plugins *felt* in the client
//! without touching the wire (ADR-0017: the TUI is not protocol-privileged;
//! everything here is client-local config + child-process execution):
//!
//! * [`entries_from_manifests`] flattens the driver's load-once manifest
//!   snapshot (see `attach::driver`; same load-once policy as the
//!   keybindings snapshot) into per-action entries.
//! * The command palette lists them as namespaced rows
//!   (`plugin: <plugin-name>: <action title>`) under a "Plugin" header —
//!   see [`super::action_registry::palette_items`].
//! * [`merge_plugin_bindings`] folds each action's optional manifest
//!   `keys = "..."` into the prefix table, with **user config always
//!   winning** on conflict (exact-chord or ambiguous-prefix); conflicts
//!   log a warning, never panic, and never disable the user's bindings.
//! * [`spawn_plugin_action`] executes a chosen action through the same
//!   `phux-plugin` child-process runtime the CLI uses, off the input loop
//!   (spawned task), reporting completion over a channel the driver
//!   selects on. Failures surface as a dismissable toast overlay built by
//!   [`failure_toast`]; successes just log.
//!
//! Palette rows and merged bindings both commit the
//! [`PLUGIN_ACTION_NAME`] dispatcher action carrying
//! `plugin = <id>, action = <id>` args, so keybinds and the palette share
//! the single `run_action` dispatch path (the architectural invariant).

use std::collections::BTreeMap;
use std::time::Duration;

use phux_config::keybind::{ResolvedAction, Resolver};
use phux_config::plugin::PluginManifest;
use phux_config::{Action, KeybindingsCfg, ParamAction};
use tokio::sync::mpsc::UnboundedSender;

/// The dispatcher action plugin palette rows and merged bindings commit.
///
/// Listed in [`super::input_dispatch::ACTION_NAMES`] and handled by a
/// `run_action` arm; exempt from the static palette registry because its
/// rows are built dynamically from the plugin snapshot.
pub const PLUGIN_ACTION_NAME: &str = "plugin-action";

/// Cap on a TUI-triggered plugin action's runtime. The CLI runs
/// uncapped (the user watches it); a TUI action runs detached from any
/// visible process, so a hung plugin must not leak a child forever
/// (`phux-plugin` kills the process on timeout).
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

fn plugin_args(plugin_id: &str, action_id: &str) -> BTreeMap<String, toml::Value> {
    let mut args = BTreeMap::new();
    args.insert(
        "plugin".to_owned(),
        toml::Value::String(plugin_id.to_owned()),
    );
    args.insert(
        "action".to_owned(),
        toml::Value::String(action_id.to_owned()),
    );
    args
}

/// Flatten loaded manifests into per-action entries.
///
/// Pure; separated from the manifest-loading I/O in the driver (which
/// resolves manifests relative to the canonical config path via
/// [`phux_config::plugin::load_enabled_manifests`], the same resolution
/// `phux config run` uses, skipping broken manifests with a warning) so
/// tests can drive it with in-memory manifests.
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

/// Merge plugin-contributed `keys` bindings into the prefix table.
///
/// User config ALWAYS wins: a plugin chord that collides with an existing
/// prefix-table entry (exact chord string), that fails to parse, or that
/// forms an ambiguous-prefix relationship with any existing binding is
/// dropped with a `tracing::warn!` — never a panic, and never at the cost
/// of the user's own bindings. Each candidate is validated by test-building
/// a [`Resolver`] over the merged table, so a bad plugin binding can't
/// poison resolver construction later (which would silently disable every
/// keybinding).
///
/// Between two plugins contending for the same chord, the first (config
/// `[[plugins]]` order) wins — deterministic, and the loser is logged.
pub fn merge_plugin_bindings(kb: &mut KeybindingsCfg, entries: &[PluginActionEntry]) {
    for entry in entries {
        let Some(keys) = entry.keys.as_deref() else {
            continue;
        };
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
        // Tentative insert + resolver validation: catches unparsable
        // chords, chord-equivalent duplicates spelled differently (e.g.
        // `?` vs `S-/`), and ambiguous-prefix relationships with user
        // bindings — any of which would otherwise make `Resolver::new`
        // fail wholesale at attach time.
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

/// Completion report for one spawned plugin action, delivered to the
/// driver's `select!` loop over the plugin-events channel.
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

/// Execute one plugin action off the input loop.
///
/// Spawns a task that routes through the same
/// [`phux_plugin::run_configured_action`] child-process runtime as
/// `phux config run PLUGIN ACTION`, then reports the outcome on `tx`.
/// The TUI never blocks: the input loop keeps running while the child
/// does. A dropped receiver (loop exited) makes the send a no-op.
pub fn spawn_plugin_action(
    tx: UnboundedSender<PluginRunResult>,
    plugin_id: String,
    action_id: String,
) {
    tokio::spawn(async move {
        let config_path = phux_config::loader::config_path();
        let request = phux_plugin::PluginActionRequest {
            plugin_id: plugin_id.clone(),
            action_id: action_id.clone(),
            timeout: Some(PLUGIN_ACTION_TIMEOUT),
            cwd: None,
        };
        let result = phux_plugin::run_configured_action(&config_path, &request)
            .await
            .map_err(|err| err.to_string());
        let _ = tx.send(PluginRunResult {
            plugin_id,
            action_id,
            result,
        });
    });
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

/// Build the failure toast `(title, body_lines)` for a finished run, or
/// `None` when the run succeeded (successes only log — no modal to
/// dismiss for the happy path).
#[must_use]
pub fn failure_toast(result: &PluginRunResult) -> Option<(String, Vec<String>)> {
    if run_succeeded(result) {
        return None;
    }
    let title = format!("plugin: {} {} failed", result.plugin_id, result.action_id);
    let mut lines = Vec::new();
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
    Some((title, lines))
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
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;
    use phux_config::keybind::{Feed, parse_chord};

    fn entry(plugin: &str, action: &str, keys: Option<&str>) -> PluginActionEntry {
        PluginActionEntry {
            plugin_id: plugin.to_owned(),
            plugin_name: format!("{plugin} name"),
            action_id: action.to_owned(),
            title: format!("{action} title"),
            keys: keys.map(ToOwned::to_owned),
        }
    }

    fn base_kb() -> KeybindingsCfg {
        let mut prefix_table = BTreeMap::new();
        prefix_table.insert("d".to_owned(), Action::Bare("detach".to_owned()));
        KeybindingsCfg {
            prefix: "C-a".to_owned(),
            prefix_table,
            global: BTreeMap::new(),
            ..KeybindingsCfg::default()
        }
    }

    #[test]
    fn resolved_action_carries_plugin_and_action_args() {
        let e = entry("com.example.tools", "summarize", None);
        let ra = e.resolved_action();
        assert_eq!(ra.action, PLUGIN_ACTION_NAME);
        assert_eq!(
            ra.args.get("plugin"),
            Some(&toml::Value::String("com.example.tools".to_owned()))
        );
        assert_eq!(
            ra.args.get("action"),
            Some(&toml::Value::String("summarize".to_owned()))
        );
    }

    #[test]
    fn merge_inserts_plugin_binding_and_resolver_fires_it() {
        let mut kb = base_kb();
        merge_plugin_bindings(&mut kb, &[entry("p", "a", Some("g"))]);
        assert!(kb.prefix_table.contains_key("g"), "binding merged");

        // The merged table resolves `C-a g` to the plugin action.
        let mut resolver = Resolver::new(&kb).expect("merged table builds");
        assert_eq!(
            resolver.feed(parse_chord("C-a").unwrap()),
            Feed::Partial,
            "prefix chord starts the sequence"
        );
        let Feed::Resolved(fired) = resolver.feed(parse_chord("g").unwrap()) else {
            panic!("expected plugin binding to resolve");
        };
        assert_eq!(fired.action, PLUGIN_ACTION_NAME);
        assert_eq!(
            fired.args.get("plugin"),
            Some(&toml::Value::String("p".to_owned()))
        );
        assert_eq!(
            fired.args.get("action"),
            Some(&toml::Value::String("a".to_owned()))
        );
    }

    #[test]
    fn merge_user_binding_wins_on_exact_chord_conflict() {
        let mut kb = base_kb(); // user binds `d` -> detach
        merge_plugin_bindings(&mut kb, &[entry("p", "a", Some("d"))]);
        assert_eq!(
            kb.prefix_table.get("d"),
            Some(&Action::Bare("detach".to_owned())),
            "user's binding must survive the merge untouched"
        );
        assert_eq!(kb.prefix_table.len(), 1, "plugin binding dropped");
    }

    #[test]
    fn merge_user_binding_wins_on_chord_equivalent_spelling() {
        // `?` and `S-/` are the same chord (shifted-glyph rule); a plugin
        // spelling the user's chord differently must still lose.
        let mut kb = base_kb();
        kb.prefix_table
            .insert("?".to_owned(), Action::Bare("show-help".to_owned()));
        merge_plugin_bindings(&mut kb, &[entry("p", "a", Some("S-/"))]);
        assert!(
            !kb.prefix_table.contains_key("S-/"),
            "chord-equivalent plugin binding dropped"
        );
        assert_eq!(
            kb.prefix_table.get("?"),
            Some(&Action::Bare("show-help".to_owned()))
        );
        // The surviving table still builds a resolver (nothing poisoned).
        Resolver::new(&kb).expect("table remains valid");
    }

    #[test]
    fn merge_drops_ambiguous_prefix_binding_without_panicking() {
        // User binds the two-chord sequence `g s`; a plugin claiming bare
        // `g` would shadow it (ambiguous prefix). The plugin loses.
        let mut kb = base_kb();
        kb.prefix_table
            .insert("g s".to_owned(), Action::Bare("new-window".to_owned()));
        merge_plugin_bindings(&mut kb, &[entry("p", "a", Some("g"))]);
        assert!(
            !kb.prefix_table.contains_key("g"),
            "ambiguous plugin chord dropped"
        );
        Resolver::new(&kb).expect("user bindings unaffected");
    }

    #[test]
    fn merge_drops_unparsable_chord_without_panicking() {
        let mut kb = base_kb();
        merge_plugin_bindings(&mut kb, &[entry("p", "a", Some("NotAKey-"))]);
        assert!(!kb.prefix_table.contains_key("NotAKey-"));
        Resolver::new(&kb).expect("user bindings unaffected");
    }

    #[test]
    fn merge_first_plugin_wins_between_two_plugins() {
        let mut kb = base_kb();
        merge_plugin_bindings(
            &mut kb,
            &[
                entry("first", "a", Some("g")),
                entry("second", "b", Some("g")),
            ],
        );
        let Some(Action::Parameterized(p)) = kb.prefix_table.get("g") else {
            panic!("expected merged parameterized binding");
        };
        assert_eq!(
            p.args.get("plugin"),
            Some(&toml::Value::String("first".to_owned())),
            "config-order first plugin keeps the chord"
        );
    }

    #[test]
    fn merge_skips_entries_without_keys() {
        let mut kb = base_kb();
        merge_plugin_bindings(&mut kb, &[entry("p", "a", None)]);
        assert_eq!(kb.prefix_table.len(), 1, "nothing merged");
    }

    #[test]
    fn failure_toast_is_none_on_success() {
        let result = PluginRunResult {
            plugin_id: "p".to_owned(),
            action_id: "a".to_owned(),
            result: Ok(phux_plugin::PluginActionOutput {
                schema_version: 1,
                plugin_id: "p".to_owned(),
                action_id: "a".to_owned(),
                command: vec!["true".to_owned()],
                cwd: std::path::PathBuf::from("/"),
                outcome: phux_plugin::PluginActionOutcome::Completed,
                exit_code: Some(0),
                stdout: "fine".to_owned(),
                stderr: String::new(),
                duration_ms: 1,
            }),
        };
        assert!(run_succeeded(&result));
        assert!(failure_toast(&result).is_none());
    }

    #[test]
    fn failure_toast_carries_exit_code_and_stderr_tail() {
        let result = PluginRunResult {
            plugin_id: "p".to_owned(),
            action_id: "a".to_owned(),
            result: Ok(phux_plugin::PluginActionOutput {
                schema_version: 1,
                plugin_id: "p".to_owned(),
                action_id: "a".to_owned(),
                command: vec!["false".to_owned()],
                cwd: std::path::PathBuf::from("/"),
                outcome: phux_plugin::PluginActionOutcome::Completed,
                exit_code: Some(2),
                stdout: String::new(),
                stderr: "boom: file not found".to_owned(),
                duration_ms: 1,
            }),
        };
        let (title, lines) = failure_toast(&result).expect("failure produces a toast");
        assert!(title.contains("p a failed"), "title was {title}");
        assert!(lines.iter().any(|l| l.contains("exit code 2")));
        assert!(lines.iter().any(|l| l.contains("boom: file not found")));
    }

    #[test]
    fn failure_toast_elides_long_output_keeping_the_tail() {
        let mut stderr = String::new();
        for i in 0..100 {
            use std::fmt::Write as _;
            let _ = writeln!(stderr, "line {i}");
        }
        let result = PluginRunResult {
            plugin_id: "p".to_owned(),
            action_id: "a".to_owned(),
            result: Ok(phux_plugin::PluginActionOutput {
                schema_version: 1,
                plugin_id: "p".to_owned(),
                action_id: "a".to_owned(),
                command: vec!["x".to_owned()],
                cwd: std::path::PathBuf::from("/"),
                outcome: phux_plugin::PluginActionOutcome::Completed,
                exit_code: Some(1),
                stdout: String::new(),
                stderr,
                duration_ms: 1,
            }),
        };
        let (_, lines) = failure_toast(&result).expect("toast");
        assert!(lines.len() <= TOAST_MAX_LINES + 1, "got {}", lines.len());
        assert!(
            lines.iter().any(|l| l.contains("line 99")),
            "tail line kept: {lines:?}"
        );
        assert!(lines.iter().any(|l| l.contains("elided")));
    }

    #[test]
    fn failure_toast_reports_pre_exec_error() {
        let result = PluginRunResult {
            plugin_id: "p".to_owned(),
            action_id: "a".to_owned(),
            result: Err("plugin \"p\" is not configured".to_owned()),
        };
        let (_, lines) = failure_toast(&result).expect("toast");
        assert!(lines[0].contains("not configured"));
    }

    #[test]
    fn entries_flatten_manifest_actions_with_keys() {
        // In-memory manifest shape; only the fields entries care about
        // are meaningful here.
        let manifest = PluginManifest {
            id: "com.example.tools".to_owned(),
            name: "Tools".to_owned(),
            version: "0.1.0".to_owned(),
            min_phux_version: "0.0.2".to_owned(),
            description: None,
            manifest_path: std::path::PathBuf::from("/x/phux-plugin.toml"),
            plugin_root: std::path::PathBuf::from("/x"),
            platforms: None,
            build: Vec::new(),
            agents: Vec::new(),
            actions: vec![
                phux_config::plugin::PluginManifestAction {
                    id: "summarize".to_owned(),
                    title: "Summarize pane".to_owned(),
                    description: None,
                    contexts: Vec::new(),
                    platforms: None,
                    command: vec!["true".to_owned()],
                    keys: Some("g".to_owned()),
                },
                phux_config::plugin::PluginManifestAction {
                    id: "report".to_owned(),
                    title: "Report".to_owned(),
                    description: None,
                    contexts: Vec::new(),
                    platforms: None,
                    command: vec!["true".to_owned()],
                    keys: None,
                },
            ],
            events: Vec::new(),
            panes: Vec::new(),
            links: Vec::new(),
            workspaces: Vec::new(),
            widgets: Vec::new(),
        };
        let entries = entries_from_manifests(&[manifest]);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].palette_label(), "plugin: Tools: Summarize pane");
        assert_eq!(entries[0].keys.as_deref(), Some("g"));
        assert_eq!(entries[1].keys, None);
    }
}
