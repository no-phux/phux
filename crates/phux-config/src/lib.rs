//! phux-config: the typed `config.toml` schema (`docs/consumers/tui.md` §4),
//! its layered loader, and the status-bar widget contract.
//!
//! Parse errors carry `line:col` derived from the TOML span; errors on a
//! merged layer stack carry no position rather than a fabricated one.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

pub mod check;
pub mod connector;
pub mod distro;
mod error;
pub mod instance;
pub mod integration;
pub mod keybind;
pub mod known_authorities;
mod layer;
pub mod loader;
pub mod overlay;
pub mod plugin;
pub mod production;
pub mod project;
pub mod remote;
pub mod satellite;
pub mod scaffold;
mod schema;
pub mod session_name;
pub mod settings;
pub mod socket;
pub mod theme;
pub mod toml_registry;
pub mod vocab;
pub mod widget;

pub use check::{CheckReport, Fault, Finding};
pub use connector::ConnectorConfigEntry;
pub use error::{ConfigError, byte_offset_to_line_col};
pub use layer::{
    ConfigProvenance, KeyOrigin, LayerSource, MAX_EXTENDS_DEPTH, merged_config_with_provenance,
};
pub use project::ProjectConfigEntry;
pub use remote::RemoteConfigEntry;
pub use satellite::SatelliteConfigEntry;
pub use schema::{
    Action, ChromeCfg, Config, CwdInheritance, DEFAULT_AGENT_LOG_BYTES,
    DEFAULT_APPROVAL_MAX_PENDING, DEFAULT_APPROVAL_MAX_PENDING_TOTAL, DEFAULT_APPROVAL_TTL_SECS,
    DEFAULT_EVENT_JOURNAL_BYTES, DEFAULT_EVENT_JOURNAL_ENTRIES, DEFAULT_HISTORY_BYTES,
    DEFAULT_METADATA_VALUE_BYTES, DEFAULT_RETAIN_ON_EXIT_MAX, DEFAULT_RETAIN_ON_EXIT_MAX_SECS,
    DEFAULT_RETAIN_ON_EXIT_SECS, DefaultsCfg, ExperimentalCfg, HookEntry, KeybindingsCfg,
    LimitsCfg, MAX_AGENT_LOG_BYTES, MAX_APPROVAL_MAX_PENDING, MAX_APPROVAL_MAX_PENDING_TOTAL,
    MAX_APPROVAL_TTL_SECS, MAX_EVENT_JOURNAL_BYTES, MAX_EVENT_JOURNAL_ENTRIES, MAX_HISTORY_BYTES,
    MAX_RETAIN_ON_EXIT_MAX, ParamAction, PolicyCfg, PolicyMode, ScrollbackLimits, SidebarCfg,
    SidebarPosition, StatusCfg, StatusPosition, ThemeCfg, VoiceCfg, Widget, WidgetSpec, WindowSize,
};
pub use session_name::{
    NameRng, RANDOM_NAME_PLACEHOLDER, random_name, render_session_name_template,
    render_session_name_template_with, template_has_random_name,
};
pub use settings::{
    Applies, CATALOG, Edit, EditOutcome, SettingKind, SettingSection, SettingSpec, SettingsSnapshot,
};
pub use widget::{
    Cell, CellStyle, SessionNameWidget, SpacerWidget, StatusBar, StatusWidget, TextWidget,
    TimeWidget, WidgetCells, WidgetContext, WidgetError, WidgetFactory, WidgetRegistry, WindowInfo,
    WindowsWidget, row_to_string,
};

use std::path::Path;

/// The shipped default config, embedded at compile time. The loader layers
/// the user's file over it; [`parse_str`] alone does not.
///
/// ```
/// let cfg = phux_config::parse_str(
///     phux_config::DEFAULT_CONFIG_TOML,
///     std::path::Path::new("default.toml"),
/// ).expect("embedded defaults must parse");
/// assert_eq!(cfg.keybindings.prefix, "C-a");
/// ```
pub const DEFAULT_CONFIG_TOML: &str = include_str!("default.toml");

/// Parse one TOML config string, without the shipped defaults. `path` is
/// used only for error reporting.
///
/// # Errors
///
/// [`ConfigError::Parse`] for invalid TOML or a schema mismatch (unknown
/// fields are rejected).
pub fn parse_str(input: &str, path: &Path) -> Result<Config, ConfigError> {
    toml::from_str::<Config>(input)
        .map_err(|e| ConfigError::parse(path, input, e.span(), e.message()))
}

/// Parse `user_input` over [`DEFAULT_CONFIG_TOML`] and any `extends` layers
/// (ADR-0039).
///
/// Every leaf a later layer sets wins and tables merge recursively. Arrays
/// replace, unless the key is spelled `x-append`, which appends to the
/// inherited `x`. `path` also anchors relative `extends` entries, which are
/// read from disk.
///
/// # Errors
///
/// [`ConfigError::Parse`] for a parse or schema failure in any layer;
/// [`ConfigError::LayerRead`], [`ConfigError::LayerCycle`], or
/// [`ConfigError::Layer`] for layer-resolution failures.
pub fn parse_with_defaults(user_input: &str, path: &Path) -> Result<Config, ConfigError> {
    let merged = merged_config_table(user_input, path)?;
    deserialize_merged(merged, user_input, path)
}

/// [`parse_with_defaults`] with an aggregate byte budget over `user_input`
/// and every inherited file (the embedded defaults excluded).
///
/// # Errors
/// As [`parse_with_defaults`]; exhausting the budget is a
/// [`ConfigError::LayerRead`] with [`std::io::ErrorKind::FileTooLarge`].
pub fn parse_with_defaults_bounded(
    user_input: &str,
    path: &Path,
    max_read_bytes: usize,
) -> Result<Config, ConfigError> {
    let (merged, _) = layer::merged_with_budget(user_input, path, Some(max_read_bytes))?;
    deserialize_merged(merged, user_input, path)
}

/// Deserialize an already-merged layer table into a [`Config`].
pub(crate) fn deserialize_merged(
    merged: toml::Table,
    user_input: &str,
    path: &Path,
) -> Result<Config, ConfigError> {
    toml::Value::Table(merged)
        .try_into()
        .map_err(|e: toml::de::Error| ConfigError::parse(path, user_input, e.span(), e.message()))
}

/// The merged layer stack as a TOML table, without deserializing: what
/// `phux config show` renders. See [`merged_config_with_provenance`] for the
/// per-key attribution.
///
/// # Errors
///
/// As [`parse_with_defaults`], minus schema failures.
pub fn merged_config_table(user_input: &str, path: &Path) -> Result<toml::Table, ConfigError> {
    merged_config_with_provenance(user_input, path).map(|(table, _)| table)
}
