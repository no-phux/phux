//! The settings editor's model (ADR-0023: edits round-trip through the
//! user's `config.toml`):
//!
//! 1. [`CATALOG`]: one [`SettingSpec`] per scalar schema key (composite keys
//!    are composition, not knobs). A test pins it to the schema both ways.
//! 2. [`SettingsSnapshot`]: the resolved layer stack, answering each key's
//!    effective value, shipped default, and origin layer.
//! 3. The writer ([`apply_edit`] / [`write_edit`]).

mod write;

use std::path::Path;

pub use write::replace_atomically;
pub use write::{Edit, EditOutcome, apply_edit, write_edit};

use crate::{
    ConfigError, ConfigProvenance, LayerSource, MAX_AGENT_LOG_BYTES, MAX_EVENT_JOURNAL_BYTES,
    MAX_EVENT_JOURNAL_ENTRIES, MAX_HISTORY_BYTES, MAX_RETAIN_ON_EXIT_MAX,
};

/// A top-level `config.toml` table an editor shows. `Theme` has no
/// [`CATALOG`] rows; its free-form slots are read via
/// [`SettingsSnapshot::value_at`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingSection {
    /// `[defaults]`.
    Defaults,
    /// `[keybindings]`.
    Keybindings,
    /// `[status]`.
    Status,
    /// `[sidebar]`.
    Sidebar,
    /// `[chrome]`.
    Chrome,
    /// `[notifications]`.
    Notifications,
    /// `[theme]`.
    Theme,
    /// `[experimental]`.
    Experimental,
    /// `[voice]`.
    Voice,
    /// `[organize]`.
    Organize,
    /// `[limits]`.
    Limits,
}

impl SettingSection {
    /// Every section, in display order.
    pub const ALL: &'static [Self] = &[
        Self::Defaults,
        Self::Keybindings,
        Self::Status,
        Self::Sidebar,
        Self::Chrome,
        Self::Notifications,
        Self::Theme,
        Self::Experimental,
        Self::Voice,
        Self::Organize,
        Self::Limits,
    ];

    /// Human title for a section header.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Defaults => "Defaults",
            Self::Keybindings => "Keybindings",
            Self::Status => "Status bar",
            Self::Sidebar => "Sidebar",
            Self::Chrome => "Chrome",
            Self::Notifications => "Notifications",
            Self::Theme => "Theme",
            Self::Experimental => "Experimental",
            Self::Voice => "Voice",
            Self::Organize => "Organize",
            Self::Limits => "Limits",
        }
    }

    /// The TOML table name, as the user writes it between brackets.
    #[must_use]
    pub const fn table(self) -> &'static str {
        match self {
            Self::Defaults => "defaults",
            Self::Keybindings => "keybindings",
            Self::Status => "status",
            Self::Sidebar => "sidebar",
            Self::Chrome => "chrome",
            Self::Notifications => "notifications",
            Self::Theme => "theme",
            Self::Experimental => "experimental",
            Self::Voice => "voice",
            Self::Organize => "organize",
            Self::Limits => "limits",
        }
    }
}

/// How a setting's value is shaped, for an editor choosing a control.
///
/// The `Optional*` kinds and [`Argv`](Self::Argv) may be unset (the consumer
/// applies its fallback); the others always have a shipped value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    /// `true` / `false`.
    Bool,
    /// An integer within `[min, max]`, both inclusive.
    Integer {
        /// Smallest accepted value.
        min: i64,
        /// Largest accepted value.
        max: i64,
    },
    /// Free text.
    Text,
    /// One of the schema enum's kebab-case variant names.
    Choice(&'static [&'static str]),
    /// Tri-state: unset, `true`, or `false`.
    OptionalBool,
    /// Unset, or free text.
    OptionalText,
    /// Unset, or an integer within `[min, max]`.
    OptionalInteger {
        /// Smallest accepted value.
        min: i64,
        /// Largest accepted value.
        max: i64,
    },
    /// Unset, or an argv: an array of strings, one per word.
    Argv,
    /// A keybinding chord under the `crate::keybind` grammar, e.g. `C-a`.
    Chord,
    /// A theme color string; a label for editors, not validated here.
    Color,
}

/// When a change to a setting takes effect (`docs/consumers/tui.md` §4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Applies {
    /// On `reload-config` / `phux config reload`.
    LiveReload,
    /// Read once when a client attaches.
    NextAttach,
    /// Read once at server start.
    NextSpawn,
}

/// One scalar setting of the schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingSpec {
    /// Dotted TOML path of bare segments, e.g. `defaults.history-limit`.
    pub key: &'static str,
    /// The section the key lives in.
    pub section: SettingSection,
    /// The value's shape.
    pub kind: SettingKind,
    /// One line, at most 72 characters, no trailing period.
    pub summary: &'static str,
    /// One to three sentences: what it does, the units, the gotcha.
    pub detail: &'static str,
    /// When a change takes effect.
    pub applies: Applies,
}

impl SettingSpec {
    /// The last dotted segment: the key as written inside its table.
    #[must_use]
    pub fn leaf(&self) -> &str {
        self.key.rsplit_once('.').map_or(self.key, |(_, leaf)| leaf)
    }

    /// Everything before the last segment: the table the key lives in.
    #[must_use]
    pub fn table(&self) -> &str {
        self.key.rsplit_once('.').map_or("", |(table, _)| table)
    }
}

// Integer bounds as `i64`.
const U16_MAX: i64 = u16::MAX as i64;
const U32_MAX: i64 = u32::MAX as i64;
const HISTORY_BYTES_MAX: i64 = MAX_HISTORY_BYTES as i64;
const AGENT_LOG_BYTES_MAX: i64 = MAX_AGENT_LOG_BYTES as i64;
const APPROVAL_TTL_MAX: i64 = crate::MAX_APPROVAL_TTL_SECS as i64;
const APPROVAL_MAX_PENDING_MAX: i64 = crate::MAX_APPROVAL_MAX_PENDING as i64;
const APPROVAL_MAX_PENDING_TOTAL_MAX: i64 = crate::MAX_APPROVAL_MAX_PENDING_TOTAL as i64;
/// A metadata cap below the server's own agent-session record write would
/// break session metadata (see `check::limits_findings`).
#[allow(
    clippy::cast_possible_wrap,
    reason = "MAX_AGENT_SESSION_RECORD_BYTES is a small compile-time constant (4096)"
)]
const METADATA_VALUE_BYTES_MIN: i64 =
    phux_protocol::wire::frame::MAX_AGENT_SESSION_RECORD_BYTES as i64;
const EVENT_JOURNAL_ENTRIES_MAX: i64 = MAX_EVENT_JOURNAL_ENTRIES as i64;
const EVENT_JOURNAL_BYTES_MAX: i64 = MAX_EVENT_JOURNAL_BYTES as i64;
const RETAIN_ON_EXIT_MAX_MAX: i64 = MAX_RETAIN_ON_EXIT_MAX as i64;
/// Editor caps (see the rows' detail text).
const WHICH_KEY_DELAY_MAX_MS: i64 = 60_000;
const VOICE_TIMEOUT_MAX_SECS: i64 = 3_600;

// Serde variant names of the schema enums.
const CLIPBOARD_WRITE: &[&str] = &["allow", "ask", "deny"];

const CWD_INHERITANCE: &[&str] = &[
    "inherit-focused",
    "home",
    "session-root",
    "last-cwd-per-window",
];
const WINDOW_SIZE: &[&str] = &["smallest", "largest", "latest", "manual"];
const STATUS_POSITION: &[&str] = &["bottom", "top"];
const SIDEBAR_POSITION: &[&str] = &["left", "right"];
const NOTIFY_MODE: &[&str] = &["terminal", "system", "bell", "off"];
const NOTIFY_PROTOCOL: &[&str] = &["auto", "osc9", "osc777", "osc99"];

/// Every scalar setting of the schema, in section then field order.
pub const CATALOG: &[SettingSpec] = &join_catalog();

mod catalog_defaults;
mod catalog_ui;

/// The `[defaults]` rows then the rest, joined at compile time.
const fn join_catalog() -> [SettingSpec; catalog_defaults::ROWS.len() + catalog_ui::ROWS.len()] {
    let mut rows =
        [catalog_defaults::ROWS[0]; catalog_defaults::ROWS.len() + catalog_ui::ROWS.len()];
    let mut i = 0;
    while i < catalog_defaults::ROWS.len() {
        rows[i] = catalog_defaults::ROWS[i];
        i += 1;
    }
    let mut j = 0;
    while j < catalog_ui::ROWS.len() {
        rows[catalog_defaults::ROWS.len() + j] = catalog_ui::ROWS[j];
        j += 1;
    }
    rows
}

/// Render a value the way a settings page shows it: strings bare, `unset`
/// for absent, an array as shell-quoted words, anything else as TOML.
#[must_use]
pub fn render_value(value: Option<&toml::Value>) -> String {
    match value {
        None => "unset".to_owned(),
        Some(toml::Value::String(s)) => s.clone(),
        Some(toml::Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                toml::Value::String(word) => shell_quote(word),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(" "),
        Some(other) => other.to_string(),
    }
}

/// Single-quote `word` when it contains anything a shell would interpret;
/// otherwise return it bare.
fn shell_quote(word: &str) -> String {
    let is_plain = !word.is_empty()
        && word.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    '_' | '-' | '.' | '/' | ':' | '=' | '@' | '%' | '+' | ',' | '{' | '}'
                )
        });
    if is_plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// Serialize a typed config back to a table, so every schema-defaulted key
/// is present.
fn schema_table(config: crate::Config, path: &Path) -> Result<toml::Table, ConfigError> {
    toml::Table::try_from(config).map_err(|err| ConfigError::parse(path, "", None, err.to_string()))
}

/// Walk a dotted path of bare segments into `table`.
fn value_at<'t>(table: &'t toml::Table, dotted: &str) -> Option<&'t toml::Value> {
    let mut segments = dotted.split('.');
    let mut current = table.get(segments.next()?)?;
    for segment in segments {
        current = current.as_table()?.get(segment)?;
    }
    Some(current)
}

/// The resolved layer stack of one config file.
///
/// Both tables round-trip through [`crate::Config`], so keys the schema
/// defaults (but `default.toml` leaves commented out) are present; optional
/// keys stay absent.
#[derive(Debug, Clone)]
pub struct SettingsSnapshot {
    effective: toml::Table,
    defaults: toml::Table,
    provenance: ConfigProvenance,
}

impl SettingsSnapshot {
    /// Resolve `user_input` (from `path`) with its full layer stack.
    ///
    /// # Errors
    ///
    /// Whatever [`crate::parse_with_defaults`] returns.
    pub fn load(user_input: &str, path: &Path) -> Result<Self, ConfigError> {
        let (merged, provenance) = crate::merged_config_with_provenance(user_input, path)?;
        let effective = schema_table(crate::deserialize_merged(merged, user_input, path)?, path)?;
        let defaults = schema_table(crate::parse_with_defaults("", path)?, path)?;
        Ok(Self {
            effective,
            defaults,
            provenance,
        })
    }

    /// The effective value at a dotted key (catalogue or `theme.<slot>`).
    #[must_use]
    pub fn value_at(&self, dotted_key: &str) -> Option<&toml::Value> {
        value_at(&self.effective, dotted_key)
    }

    /// The layer above the embedded defaults that set `dotted_key`, if any.
    #[must_use]
    pub fn origin_at(&self, dotted_key: &str) -> Option<&LayerSource> {
        let origin = self.provenance.keys.get(dotted_key)?;
        self.provenance
            .layers
            .get(origin.layer)
            .filter(|layer| **layer != LayerSource::Defaults)
    }

    /// The shipped default at a dotted key, if any.
    #[must_use]
    pub fn default_at(&self, dotted_key: &str) -> Option<&toml::Value> {
        value_at(&self.defaults, dotted_key)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests;
