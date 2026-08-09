//! Declarative plugin manifest parsing for phux config consumers.

mod link;
mod loader;
mod source;
mod validate;
mod version;
mod workspace;

use std::path::{Path, PathBuf};

pub use loader::load_plugin_manifest;
use serde::{Deserialize, Serialize};
pub use version::CURRENT_PHUX_VERSION;

/// A plugin declared in `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginConfigEntry {
    /// Path to a `phux-plugin.toml` manifest.
    pub manifest: PathBuf,
    /// Whether this plugin is active for consumers that execute plugins.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Parsed `phux-plugin.toml` manifest.
// `Eq` is not derived: `PluginManifestWidget.opts` carries `toml::Value`
// (not `Eq` because of `f64`), matching the schema-crate convention.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginManifest {
    /// Globally unique plugin id.
    pub id: String,
    /// Human-readable plugin name.
    pub name: String,
    /// Plugin package version.
    pub version: String,
    /// Oldest phux version the manifest targets.
    pub min_phux_version: String,
    /// Optional human-readable summary.
    pub description: Option<String>,
    /// Canonical manifest path.
    pub manifest_path: PathBuf,
    /// Directory containing the manifest.
    pub plugin_root: PathBuf,
    /// Supported platforms, when declared.
    pub platforms: Option<Vec<PluginPlatform>>,
    /// Build commands declared by the plugin.
    pub build: Vec<PluginManifestBuild>,
    /// Agent states declared by the plugin.
    pub agents: Vec<PluginManifestAgent>,
    /// Action entrypoints declared by the plugin.
    pub actions: Vec<PluginManifestAction>,
    /// Event hook entrypoints declared by the plugin.
    pub events: Vec<PluginManifestEvent>,
    /// Pane entrypoints declared by the plugin.
    pub panes: Vec<PluginManifestPane>,
    /// Link/route handlers declared by the plugin.
    pub links: Vec<PluginManifestLinkHandler>,
    /// Workspace profiles declared by the plugin.
    pub workspaces: Vec<PluginManifestWorkspace>,
    /// Status-bar widgets contributed by the plugin (phux-r82.6).
    pub widgets: Vec<PluginManifestWidget>,
}

/// Platform names accepted in plugin manifests.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum PluginPlatform {
    /// Linux.
    Linux,
    /// macOS.
    Macos,
    /// Windows.
    Windows,
}

/// Build command declared in a plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginManifestBuild {
    /// Optional platform override for this build step.
    pub platforms: Option<Vec<PluginPlatform>>,
    /// Command argv to execute.
    pub command: Vec<String>,
}

/// Agent state declared in a plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginManifestAgent {
    /// Plugin-local agent id.
    pub id: String,
    /// Human-readable agent label.
    pub label: String,
    /// Optional human-readable description.
    pub description: Option<String>,
    /// Current state reported by this declarative surface.
    pub state: PluginAgentState,
    /// Attention level consumers may use for sorting or notification badges.
    pub attention: PluginAgentAttention,
    /// Context names where this agent is relevant.
    pub contexts: Vec<String>,
}

/// Normalized state labels for agent-aware consumers.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PluginAgentState {
    /// State cannot be determined yet.
    #[default]
    Unknown,
    /// Agent is available and not actively working.
    Idle,
    /// Agent is currently doing work.
    Working,
    /// Agent is waiting for human input or otherwise blocked.
    Blocked,
}

/// Normalized attention priority for agent-aware consumers.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PluginAgentAttention {
    /// Explicitly no attention requested.
    None,
    /// Low-priority background signal.
    Low,
    /// Normal attention priority.
    #[default]
    Normal,
    /// High-priority signal that should be surfaced prominently.
    High,
}

/// Action entrypoint declared in a plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginManifestAction {
    /// Plugin-local action id.
    pub id: String,
    /// Human-readable action title.
    pub title: String,
    /// Optional human-readable description.
    pub description: Option<String>,
    /// Context names where this action is relevant.
    pub contexts: Vec<String>,
    /// Optional platform override for this action.
    pub platforms: Option<Vec<PluginPlatform>>,
    /// Command argv to execute.
    pub command: Vec<String>,
    /// Optional prefix-table chord sequence (e.g. `"g"` or `"g s"`,
    /// chord syntax per [`crate::keybind`]) the TUI merges into its
    /// prefix table so this action can fire from a keybinding.
    /// Contributed bindings never override user config: on any conflict
    /// (same chord, or an ambiguous-prefix relationship) the user's
    /// binding wins and the plugin's is dropped with a logged warning.
    #[serde(default)]
    pub keys: Option<String>,
}

/// Event hook entrypoint declared in a plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginManifestEvent {
    /// Plugin-local event hook id.
    pub id: String,
    /// Human-readable event hook title.
    pub title: String,
    /// Optional human-readable description.
    pub description: Option<String>,
    /// Event name this hook observes.
    pub on: String,
    /// Optional platform override for this hook.
    pub platforms: Option<Vec<PluginPlatform>>,
    /// Command argv to execute.
    pub command: Vec<String>,
}

/// Pane entrypoint declared in a plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginManifestPane {
    /// Plugin-local pane id.
    pub id: String,
    /// Human-readable pane title.
    pub title: String,
    /// Optional human-readable description.
    pub description: Option<String>,
    /// Optional platform override for this pane.
    pub platforms: Option<Vec<PluginPlatform>>,
    /// Where a future runtime host should place the pane.
    pub placement: PluginPanePlacement,
    /// Command argv to execute.
    pub command: Vec<String>,
}

/// Link or route handler declared in a plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginManifestLinkHandler {
    /// Plugin-local link handler id.
    pub id: String,
    /// Human-readable link handler title.
    pub title: String,
    /// Optional human-readable description.
    pub description: Option<String>,
    /// Context names where this handler is relevant.
    pub contexts: Vec<String>,
    /// URI schemes this handler accepts.
    pub schemes: Vec<String>,
    /// Route/link patterns this handler accepts.
    pub patterns: Vec<String>,
    /// Optional platform override for this handler.
    pub platforms: Option<Vec<PluginPlatform>>,
    /// Command argv to execute.
    pub command: Vec<String>,
}

/// Workspace composition profile declared in a plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginManifestWorkspace {
    /// Plugin-local workspace id.
    pub id: String,
    /// Human-readable workspace title.
    pub title: String,
    /// Optional human-readable description.
    pub description: Option<String>,
    /// Context names where this workspace is relevant.
    pub contexts: Vec<String>,
    /// Agent ids this workspace composes.
    pub agents: Vec<String>,
    /// Action ids this workspace surfaces.
    pub actions: Vec<String>,
    /// Event ids this workspace subscribes to.
    pub events: Vec<String>,
    /// Pane roles this workspace wants phux to create or restore.
    pub panes: Vec<PluginWorkspacePane>,
}

/// Pane role inside a plugin workspace profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginWorkspacePane {
    /// Plugin-local workspace pane role id.
    pub id: String,
    /// Referenced [`PluginManifestPane::id`].
    pub pane: String,
    /// Role label used by composition tools.
    pub role: String,
    /// Optional human-readable description.
    pub description: Option<String>,
}

/// Status-bar widget contributed by a plugin manifest (phux-r82.6,
/// `[[widgets]]` in `phux-plugin.toml`).
///
/// The entry is a [`crate::WidgetSpec`]-shaped table (`kind` plus
/// kind-specific options) with two extra fields: a plugin-local `id` and
/// the bar `slot` to append to. Contributions never displace user config:
/// the TUI appends them after the user's own `[status]` widgets, and an
/// entry whose spec fails widget validation is dropped with a logged
/// warning (mirroring the [`PluginManifestAction::keys`] conflict policy).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginManifestWidget {
    /// Plugin-local widget id.
    pub id: String,
    /// Which status-bar slot the widget appends to.
    pub slot: PluginWidgetSlot,
    /// Widget kind (`exec`, `text`, ... — any registered kind).
    pub kind: String,
    /// Kind-specific options, exactly as a `[status]` widget table would
    /// carry them.
    pub opts: std::collections::BTreeMap<String, toml::Value>,
}

/// Status-bar slot a plugin widget appends to.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PluginWidgetSlot {
    /// Append to the left slot.
    Left,
    /// Append to the center slot.
    Center,
    /// Append to the right slot (the conventional home for indicators).
    #[default]
    Right,
}

/// Placement requested by a plugin pane entrypoint.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PluginPanePlacement {
    /// Temporary overlay over the focused pane.
    #[default]
    Overlay,
    /// Split next to the focused pane.
    Split,
    /// New window/tab.
    Tab,
    /// Zoomed pane view.
    Zoomed,
}

/// Resolve a configured manifest path against the config file's directory.
///
/// Absolute paths pass through; relative paths resolve under
/// `config_path`'s parent (the documented `[[plugins]]` contract — see
/// `docs/consumers/tui.md`).
#[must_use]
pub fn resolve_manifest_path(manifest: &Path, config_path: &Path) -> PathBuf {
    if manifest.is_absolute() {
        return manifest.to_path_buf();
    }
    config_path
        .parent()
        .map_or_else(|| manifest.to_path_buf(), |parent| parent.join(manifest))
}

/// Load the manifests of every **enabled** plugin in `entries`,
/// resolving relative manifest paths against `config_path`'s directory.
///
/// Best-effort by design: a manifest that fails to load or validate is
/// skipped with a `tracing::warn!` rather than failing the whole batch —
/// one broken plugin must not take down a consumer (e.g. the attach TUI)
/// that only wants to surface the healthy ones. Disabled entries are
/// skipped silently. Callers that need per-manifest errors should use
/// [`load_plugin_manifest`] directly.
#[must_use]
pub fn load_enabled_manifests(
    config_path: &Path,
    entries: &[PluginConfigEntry],
) -> Vec<PluginManifest> {
    let mut manifests = Vec::new();
    for entry in entries {
        if !entry.enabled {
            continue;
        }
        let manifest_path = resolve_manifest_path(&entry.manifest, config_path);
        match load_plugin_manifest(&manifest_path) {
            Ok(manifest) => manifests.push(manifest),
            Err(err) => {
                tracing::warn!(
                    manifest = %manifest_path.display(),
                    error = %err,
                    "skipping plugin manifest that failed to load",
                );
            }
        }
    }
    manifests
}

/// Error raised while reading or validating a plugin manifest.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginManifestError {
    /// I/O failure while reading the manifest.
    #[error("plugin manifest io: {0}")]
    Io(#[from] std::io::Error),
    /// TOML parse failure.
    #[error("{}: {message}", path.display())]
    Parse {
        /// Manifest path.
        path: PathBuf,
        /// Parse message.
        message: String,
    },
    /// Schema validation failure after TOML parsing.
    #[error("{0}")]
    Invalid(String),
}

const fn default_true() -> bool {
    true
}
