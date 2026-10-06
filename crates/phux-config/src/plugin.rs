//! Declarative plugin manifest parsing for phux config consumers.

mod loader;
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
    /// Sidebar sections contributed by the plugin (`[[sidebar]]`).
    pub sidebar: Vec<PluginManifestSidebar>,
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
    /// Optional prefix-table chord sequence (e.g. `"g s"`) merged into the
    /// TUI's prefix table; on any conflict the user's binding wins.
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
    /// Where the TUI places the pane.
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

/// Status-bar widget contributed by a plugin (`[[widgets]]`): a widget spec
/// plus a plugin-local `id` and the `slot` it appends to, after the user's
/// own widgets.
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

/// Sidebar section contributed by a plugin (`[[sidebar]]`).
///
/// A titled, fixed-height panel whose rows are this session's panes, each
/// rendered from `format`. A pane contributes a row only when every token
/// `format` names resolves for it (see [`crate::vocab::SIDEBAR_TOKENS`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginManifestSidebar {
    /// Plugin-local section id.
    pub id: String,
    /// Section header text.
    pub title: String,
    /// Row template; `{token}` occurrences are replaced per pane.
    pub format: String,
    /// Rows the section reserves under its header, whatever its population.
    pub rows: u8,
}

/// Default [`PluginManifestSidebar::rows`].
pub const SIDEBAR_SECTION_DEFAULT_ROWS: u8 = 3;
/// Largest accepted [`PluginManifestSidebar::rows`].
pub const SIDEBAR_SECTION_MAX_ROWS: u8 = 8;

/// Placement requested by a plugin pane entrypoint.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PluginPanePlacement {
    /// Floating box over the pane area, in no window; closes when its
    /// process exits or the user dismisses it (ADR-0147).
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
#[must_use]
pub fn resolve_manifest_path(manifest: &Path, config_path: &Path) -> PathBuf {
    if manifest.is_absolute() {
        return manifest.to_path_buf();
    }
    config_path
        .parent()
        .map_or_else(|| manifest.to_path_buf(), |parent| parent.join(manifest))
}

/// Load the manifests of every enabled plugin in `entries`.
///
/// Best-effort: a manifest that fails to load is skipped with a warning, so
/// one broken plugin cannot take down the consumer. Use
/// [`load_plugin_manifest`] for per-manifest errors.
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
