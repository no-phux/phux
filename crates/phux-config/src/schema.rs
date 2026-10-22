//! Typed schema for `config.toml` (`docs/consumers/tui.md` §4).
//!
//! TOML keys are kebab-case; serde renames bridge them to `snake_case`.
//! Every table is `#[serde(default)]`, so a missing key takes the value from
//! the table's `Default` impl, and an empty file parses to [`Config::default`].

#![allow(clippy::derive_partial_eq_without_eq)] // `toml::Value` is not `Eq`

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    OrganizeCfg, connector::ConnectorConfigEntry, plugin::PluginConfigEntry,
    project::ProjectConfigEntry, remote::RemoteConfigEntry, satellite::SatelliteConfigEntry,
    uplink::UplinkConfigEntry,
};

/// Top-level config (`docs/consumers/tui.md` §4.2).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Server-wide defaults: shell, `TERM`, scrollback, spawn policy.
    pub defaults: DefaultsCfg,
    /// Prefix, prefix-table, and global keybindings.
    pub keybindings: KeybindingsCfg,
    /// Status-bar slot composition.
    pub status: StatusCfg,
    /// Window sidebar (`[sidebar]`).
    pub sidebar: SidebarCfg,
    /// Responsive-chrome breakpoints (`[chrome]`).
    pub chrome: ChromeCfg,
    /// Agent notifications the TUI posts (`[notifications]`).
    pub notifications: NotificationsCfg,
    /// Event hooks (`[[hooks.<event>]]`), keyed by event name.
    pub hooks: BTreeMap<String, Vec<HookEntry>>,
    /// Declarative plugin manifests composed into this config.
    pub plugins: Vec<PluginConfigEntry>,
    /// Hub-and-spoke federation satellites declared for this host.
    pub satellites: Vec<SatelliteConfigEntry>,
    /// Outbound relay links this server supervises (ADR-0052).
    pub connector: Vec<ConnectorConfigEntry>,
    /// Hubs this server dials out to as a satellite (ADR-0182), written by
    /// `phux host join`.
    pub uplink: Vec<UplinkConfigEntry>,
    /// Remote servers this machine attaches to (ADR-0055), written by
    /// `phux host add`.
    pub remote: Vec<RemoteConfigEntry>,
    /// Named projects `phux project open NAME` resolves (ADR-0152).
    pub projects: Vec<ProjectConfigEntry>,
    /// Color slots: free-form `slot -> color` strings.
    pub theme: ThemeCfg,
    /// Opt-in unstable features; may change without notice.
    pub experimental: ExperimentalCfg,
    /// `[policy]`: the server's authorization posture
    /// (`docs/spec/workload-auth.md` §8).
    pub policy: PolicyCfg,
    /// `[voice]`: the server-side transcriber behind `TRANSCRIBE`.
    pub voice: VoiceCfg,
    /// `[organize]`: the judge behind `phux organize`.
    pub organize: OrganizeCfg,
    /// `[limits]`: server-enforced ceilings that are not per-pane defaults.
    pub limits: LimitsCfg,
}

mod defaults;
mod keybindings;

pub use defaults::*;
pub use keybindings::*;

// ---------------------------------------------------------------------------
// [status] / [sidebar] / [chrome]
// ---------------------------------------------------------------------------

/// `[status]` table: three widget slots plus the row the bar reserves.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct StatusCfg {
    /// Left slot.
    pub left: Vec<Widget>,
    /// Center slot.
    pub center: Vec<Widget>,
    /// Right slot.
    pub right: Vec<Widget>,
    /// Which outer-terminal row the bar reserves.
    pub position: StatusPosition,
}

/// Which row the status bar occupies.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum StatusPosition {
    /// The bottom row.
    Bottom,
    /// The top row (default).
    #[default]
    Top,
}

/// `[sidebar]`: the vertical window list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SidebarCfg {
    /// Show the sidebar (default `true`).
    pub enabled: bool,
    /// Width in columns (default `20`). `0` sizes automatically to a quarter
    /// of the viewport, bounded to 28-40 columns.
    pub width: u16,
    /// Which edge the sidebar docks to.
    pub position: SidebarPosition,
    /// Share of the sidebar body the Agents panel keeps, as a percent
    /// (default `50`, half). `0` drops the panel entirely.
    ///
    /// ADR-0112: the Agents panel on top and the Sessions panel below share
    /// the body at a **fixed** line, whatever the agent population, so
    /// gaining or resolving an agent never moves Sessions. An empty Agents
    /// panel keeps its header and a quiet placeholder. Plugin sections claim
    /// their fixed bands first, and the whole sidebar yields below
    /// `width + chrome.min-pane-cols` columns.
    #[serde(rename = "agents-share")]
    pub agents_share: u8,
    /// Segment the Sessions area by machine (ADR-0140). Default `true`.
    ///
    /// The attached server's sessions are always shown live. The other
    /// machines (this one, when attached to a remote, and every `[[remote]]`
    /// host) come from a *hosts provider*: a command that prints the
    /// `phux.hosts/v1` document, re-run every `hosts-refresh-secs`. `false`
    /// lists only the attached server's sessions and runs no provider.
    pub hosts: bool,
    /// Hosts provider argv. Empty (the default) runs this binary's own
    /// `ls --all --json`; any command printing the same shape replaces it,
    /// which is the same seam a plugin uses.
    #[serde(rename = "hosts-provider")]
    pub hosts_provider: Vec<String>,
    /// Seconds between hosts provider runs. Default `10`; at least 2.
    #[serde(rename = "hosts-refresh-secs")]
    pub hosts_refresh_secs: u64,
}

impl Default for SidebarCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            width: 20,
            position: SidebarPosition::default(),
            agents_share: 50,
            hosts: true,
            hosts_provider: Vec::new(),
            hosts_refresh_secs: 10,
        }
    }
}

/// Which edge the sidebar docks to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SidebarPosition {
    /// Left (default).
    #[default]
    Left,
    /// Right.
    Right,
}

/// `[chrome]`: responsive-chrome breakpoints (`docs/consumers/tui.md` §4.5).
/// `0` disables a threshold; a very large value pins the opposite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ChromeCfg {
    /// Width at or below which overlays go full-bleed horizontally.
    pub compact_cols: u16,
    /// Height at or below which overlays go full-bleed vertically.
    pub compact_rows: u16,
    /// Narrowest pane area worth tiling into; the sidebar is not reserved
    /// below `sidebar.width + min-pane-cols`.
    pub min_pane_cols: u16,
}

impl Default for ChromeCfg {
    fn default() -> Self {
        Self {
            compact_cols: 64,
            compact_rows: 18,
            min_pane_cols: 40,
        }
    }
}

/// `[notifications]`: how the TUI tells you an agent needs you or finished
/// (`docs/consumers/tui.md` §"Notifications"). Read by the client, live on
/// reload.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent TOML toggle the user writes by name"
)]
pub struct NotificationsCfg {
    /// Where a notification goes.
    pub mode: NotifyMode,
    /// Which escape sequence `terminal` mode writes.
    pub terminal_protocol: NotifyProtocol,
    /// Notify when an agent becomes blocked or asks for a human.
    pub blocked: bool,
    /// Notify when an agent that was working finishes.
    pub done: bool,
    /// Stay quiet about the pane you are looking at while the terminal
    /// window has focus.
    pub suppress_focused: bool,
    /// Also ring the bell (`terminal`) or play the default sound (`system`).
    pub sound: bool,
}

impl Default for NotificationsCfg {
    fn default() -> Self {
        Self {
            mode: NotifyMode::Terminal,
            terminal_protocol: NotifyProtocol::Auto,
            blocked: true,
            done: true,
            suppress_focused: true,
            sound: false,
        }
    }
}

/// `[notifications] mode`: the delivery channel.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NotifyMode {
    /// An OSC notification to the outer terminal; survives SSH.
    #[default]
    Terminal,
    /// The desktop notifier of the machine the TUI runs on (`osascript` on
    /// macOS, `notify-send` elsewhere).
    System,
    /// The terminal bell only.
    Bell,
    /// No notifications.
    Off,
}

/// `[notifications] terminal-protocol`: the OSC dialect `terminal` mode
/// writes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NotifyProtocol {
    /// Pick from the environment: OSC 99 for kitty, OSC 777 for Ghostty,
    /// `WezTerm`, and foot, OSC 9 otherwise.
    #[default]
    Auto,
    /// `OSC 9 ; message` (iTerm2, `WezTerm`, Ghostty, Windows Terminal).
    Osc9,
    /// `OSC 777 ; notify ; title ; body` (Ghostty, `WezTerm`, foot).
    Osc777,
    /// `OSC 99` desktop notifications (kitty).
    Osc99,
}

/// A status-bar widget: a bare kind (`"session"`) or an inline table with
/// `kind` plus options (`docs/consumers/tui.md` §8.1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Widget {
    /// Shorthand for `{ kind = "..." }`.
    Bare(String),
    /// `kind` plus options.
    Spec(WidgetSpec),
}

impl Widget {
    /// The long form of this entry (a bare kind has no options).
    #[must_use]
    pub(crate) fn to_spec(&self) -> WidgetSpec {
        match self {
            Self::Bare(kind) => WidgetSpec {
                kind: kind.clone(),
                opts: BTreeMap::new(),
            },
            Self::Spec(spec) => spec.clone(),
        }
    }
}

/// Long-form widget spec; options are validated per kind by the registry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WidgetSpec {
    /// Widget kind (see `docs/reference/widgets.md`).
    pub kind: String,
    /// Remaining inline-table fields.
    #[serde(flatten)]
    pub opts: BTreeMap<String, toml::Value>,
}

/// One `[[hooks.<event>]]` entry (`docs/consumers/tui.md` §9).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HookEntry {
    /// Match clauses; first match wins per event.
    #[serde(default)]
    pub when: BTreeMap<String, toml::Value>,
    /// Action to fire on match.
    pub action: Action,
}

// ---------------------------------------------------------------------------
// [experimental] / [theme] / [policy] / [voice]
// ---------------------------------------------------------------------------

/// `[experimental]`: opt-in flags that may change without a `SemVer` bump.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ExperimentalCfg {
    /// Mosh-class predictive local echo (ADR-0090). Unset lets the transport
    /// decide (see [`Self::predictive_echo_for`]); an explicit value wins.
    #[serde(rename = "predictive-echo")]
    pub predictive_echo: Option<bool>,
}

impl ExperimentalCfg {
    /// Resolve predictive echo for one attach: explicit config wins, else on
    /// only when the dial crosses a network. Local echo is fast enough that
    /// prediction could only cost its two undetectable flicker cases (vi
    /// mode at a readline prompt, no-echo password prompts).
    #[must_use]
    pub const fn predictive_echo_for(&self, remote_dial: bool) -> bool {
        match self.predictive_echo {
            Some(explicit) => explicit,
            None => remote_dial,
        }
    }
}

/// `[theme]`: free-form `slot -> color` map; the renderer interprets it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct ThemeCfg {
    /// Slot to color string (e.g. `"fg" -> "#cdd6f4"`).
    pub slots: BTreeMap<String, String>,
}

/// `[policy]`: the server's authorization posture
/// (`docs/spec/workload-auth.md` §8, ADR-0116). Read once at start.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct PolicyCfg {
    /// Unset is `local`, and a configured remote entry point or workload
    /// authority location then refuses to start (ADR-0179).
    pub mode: Option<PolicyMode>,
}

/// The two closed policy modes (`docs/spec/workload-auth.md` §8).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum PolicyMode {
    /// Owner's Unix socket only; a configured remote listener refuses start.
    Local,
    /// Every TLS connection must present an enrolled workload certificate.
    Paired,
}

/// `[voice]`: the server-side transcriber behind `TRANSCRIBE`. The server
/// runs the configured command rather than embedding a model.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct VoiceCfg {
    /// Transcriber argv. `{path}` is replaced by the clip's path (appended
    /// when absent); trimmed stdout is the transcript.
    pub transcriber: Option<Vec<String>>,
    /// Seconds before the transcriber is killed. Unset means 30.
    pub timeout_secs: Option<u64>,
}

impl VoiceCfg {
    /// Default transcriber deadline when `timeout-secs` is unset.
    pub const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    /// The configured deadline, or [`Self::DEFAULT_TIMEOUT`].
    #[must_use]
    pub fn timeout(&self) -> std::time::Duration {
        self.timeout_secs
            .map_or(Self::DEFAULT_TIMEOUT, std::time::Duration::from_secs)
    }

    /// Is a transcriber configured at all?
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.transcriber
            .as_ref()
            .is_some_and(|argv| !argv.is_empty())
    }

    /// The transcriber argv with `{path}` resolved to `clip`, or `None` when
    /// nothing is configured.
    #[must_use]
    pub fn transcriber_argv(&self, clip: &std::path::Path) -> Option<Vec<String>> {
        let argv = self.transcriber.as_ref().filter(|argv| !argv.is_empty())?;
        let clip = clip.to_string_lossy();
        let mut resolved: Vec<String> = argv
            .iter()
            .map(|arg| arg.replace("{path}", &clip))
            .collect();
        if !argv.iter().any(|arg| arg.contains("{path}")) {
            resolved.push(clip.into_owned());
        }
        Some(resolved)
    }
}

#[cfg(test)]
mod voice_tests {
    use super::VoiceCfg;

    #[test]
    fn path_token_is_substituted_or_appended() {
        let cfg = VoiceCfg {
            transcriber: Some(vec!["curl".into(), "-F".into(), "file=@{path}".into()]),
            timeout_secs: None,
        };
        let argv = cfg
            .transcriber_argv(std::path::Path::new("/tmp/clip.wav"))
            .unwrap_or_default();
        assert_eq!(argv, vec!["curl", "-F", "file=@/tmp/clip.wav"]);
        let cfg = VoiceCfg {
            transcriber: Some(vec!["transcribe".into()]),
            timeout_secs: Some(5),
        };
        let argv = cfg
            .transcriber_argv(std::path::Path::new("/tmp/clip.wav"))
            .unwrap_or_default();
        assert_eq!(argv, vec!["transcribe", "/tmp/clip.wav"]);
        assert_eq!(cfg.timeout(), std::time::Duration::from_secs(5));
    }

    #[test]
    fn unset_is_not_configured() {
        let cfg = VoiceCfg::default();
        assert!(!cfg.is_configured());
        assert!(cfg.transcriber_argv(std::path::Path::new("/x")).is_none());
        assert_eq!(cfg.timeout(), VoiceCfg::DEFAULT_TIMEOUT);
    }
}
