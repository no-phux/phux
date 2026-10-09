//! Typed schema for `config.toml` (`docs/consumers/tui.md` §4).
//!
//! TOML keys are kebab-case; serde renames bridge them to `snake_case`.
//! Every table is `#[serde(default)]`, so a missing key takes the value from
//! the table's `Default` impl, and an empty file parses to [`Config::default`].

#![allow(clippy::derive_partial_eq_without_eq)] // `toml::Value` is not `Eq`

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    connector::ConnectorConfigEntry, plugin::PluginConfigEntry, project::ProjectConfigEntry,
    remote::RemoteConfigEntry, satellite::SatelliteConfigEntry,
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
    /// Event hooks (`[[hooks.<event>]]`), keyed by event name.
    pub hooks: BTreeMap<String, Vec<HookEntry>>,
    /// Declarative plugin manifests composed into this config.
    pub plugins: Vec<PluginConfigEntry>,
    /// Hub-and-spoke federation satellites declared for this host.
    pub satellites: Vec<SatelliteConfigEntry>,
    /// Outbound relay links this server supervises (ADR-0052).
    pub connector: Vec<ConnectorConfigEntry>,
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
    /// `[limits]`: server-enforced ceilings that are not per-pane defaults.
    pub limits: LimitsCfg,
}

// ---------------------------------------------------------------------------
// [defaults]
// ---------------------------------------------------------------------------

/// `[defaults]` table. See `docs/consumers/tui.md` §12 for shipped values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DefaultsCfg {
    /// Shell for new panes. `None` resolves `$SHELL`, then `/bin/sh`, once at
    /// server start; a wire `command` always wins.
    pub shell: Option<String>,

    /// `TERM` for server-spawned panes when the spawn's own env has none.
    /// Default `xterm-256color`: `ghostty` is opt-in because not every app
    /// (htop) is proven to round-trip the kitty keyboard protocol.
    pub term: String,

    /// Lines of scrollback per pane. libghostty prunes on this or
    /// [`Self::history_bytes`], whichever is reached first.
    #[serde(rename = "history-limit")]
    pub history_limit: u32,

    /// Bytes of scrollback per pane (ADR-0094): the bound that actually caps
    /// a pane's resident memory, and usually the one that binds on wide
    /// grids. Pruning is page-granular. Ceiling: [`MAX_HISTORY_BYTES`].
    #[serde(rename = "history-bytes")]
    pub history_bytes: u32,

    /// Bytes of records an `AgentSession` retains and replays as its
    /// bootstrap (ADR-0103); older records are evicted with a tombstone
    /// count. Ceiling: [`MAX_AGENT_LOG_BYTES`].
    #[serde(rename = "agent-log-bytes")]
    pub agent_log_bytes: u32,

    /// Events the server's event journal retains for cursor replay
    /// (ADR-0123). A cursor older than the ring gets a journal gap.
    /// Ceiling: [`MAX_EVENT_JOURNAL_ENTRIES`].
    #[serde(rename = "event-journal-entries")]
    pub event_journal_entries: u32,

    /// Estimated encoded bytes the event journal retains, beside
    /// [`Self::event_journal_entries`]. Ceiling: [`MAX_EVENT_JOURNAL_BYTES`].
    #[serde(rename = "event-journal-bytes")]
    pub event_journal_bytes: u32,

    /// Whether a Terminal spawned without `retain_secs` stays in the
    /// inventory, exited, after its process exits (ADR-0124).
    #[serde(rename = "retain-on-exit")]
    pub retain_on_exit: bool,

    /// Seconds a retained Terminal stays after exit when the server default
    /// applies. Capped by [`Self::retain_on_exit_max_secs`].
    #[serde(rename = "retain-on-exit-secs")]
    pub retain_on_exit_secs: u32,

    /// The longest any Terminal is retained after exit.
    #[serde(rename = "retain-on-exit-max-secs")]
    pub retain_on_exit_max_secs: u32,

    /// How many exited Terminals are retained at once; one more closes the
    /// oldest. `0` retains none.
    #[serde(rename = "retain-on-exit-max")]
    pub retain_on_exit_max: u32,

    /// Seconds a held `SIGNAL` action waits for a decision before its
    /// requester is denied (ADR-0128). Read at server start.
    #[serde(rename = "approval-ttl-secs")]
    pub approval_ttl_secs: u32,

    /// Actions one connection may hold for approval at once (ADR-0128).
    #[serde(rename = "approval-max-pending")]
    pub approval_max_pending: u32,

    /// Actions the whole server may hold for approval at once (ADR-0128).
    #[serde(rename = "approval-max-pending-total")]
    pub approval_max_pending_total: u32,

    /// Whether the client enables outer-terminal mouse tracking on attach
    /// (ADR-0048). `false` leaves the host's native selection untouched.
    pub mouse: bool,

    /// What the client does with an OSC 52 clipboard write from the pane it
    /// is focused on (ADR-0158).
    #[serde(rename = "clipboard-write")]
    pub clipboard_write: ClipboardWrite,

    /// How a freshly-spawned pane chooses its working directory.
    #[serde(rename = "cwd-inheritance")]
    pub cwd_inheritance: CwdInheritance,

    /// Command to spawn when `phux` auto-creates a session on attach.
    /// `None` uses [`Self::shell`].
    #[serde(rename = "spawn-on-attach")]
    pub spawn_on_attach: Option<String>,

    /// Naming template for auto-created sessions. Placeholders:
    /// `${cwd-basename}` (default) and `${random-name}`; unknown ones pass
    /// through verbatim.
    #[serde(rename = "session-name-template")]
    pub session_name_template: String,

    /// Which size wins when concurrent views of one Terminal disagree
    /// (ADR-0027). Governs views only: an explicit `RESIZE_TERMINAL` always
    /// applies (ADR-0062).
    #[serde(rename = "window-size")]
    pub window_size: WindowSize,
}

impl Default for DefaultsCfg {
    fn default() -> Self {
        Self {
            shell: None,
            term: "xterm-256color".to_owned(),
            history_limit: DEFAULT_HISTORY_LINES,
            history_bytes: DEFAULT_HISTORY_BYTES,
            agent_log_bytes: DEFAULT_AGENT_LOG_BYTES,
            event_journal_entries: DEFAULT_EVENT_JOURNAL_ENTRIES,
            event_journal_bytes: DEFAULT_EVENT_JOURNAL_BYTES,
            retain_on_exit: false,
            retain_on_exit_secs: DEFAULT_RETAIN_ON_EXIT_SECS,
            retain_on_exit_max_secs: DEFAULT_RETAIN_ON_EXIT_MAX_SECS,
            retain_on_exit_max: DEFAULT_RETAIN_ON_EXIT_MAX,
            approval_ttl_secs: DEFAULT_APPROVAL_TTL_SECS,
            approval_max_pending: DEFAULT_APPROVAL_MAX_PENDING,
            approval_max_pending_total: DEFAULT_APPROVAL_MAX_PENDING_TOTAL,
            mouse: true,
            clipboard_write: ClipboardWrite::default(),
            cwd_inheritance: CwdInheritance::default(),
            spawn_on_attach: None,
            session_name_template: "${cwd-basename}".to_owned(),
            window_size: WindowSize::default(),
        }
    }
}

impl DefaultsCfg {
    /// The configured scrollback bounds as one value.
    #[must_use]
    pub const fn scrollback_limits(&self) -> ScrollbackLimits {
        ScrollbackLimits::new(self.history_limit, self.history_bytes)
    }
}

/// The pair of per-pane scrollback bounds libghostty prunes on together;
/// carried as one value so no plumbing site threads them independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbackLimits {
    /// Rows of history (`defaults.history-limit`).
    pub lines: u32,
    /// Bytes of history (`defaults.history-bytes`).
    pub bytes: u32,
}

impl ScrollbackLimits {
    /// Pair an explicit line and byte bound.
    #[must_use]
    pub const fn new(lines: u32, bytes: u32) -> Self {
        Self { lines, bytes }
    }
}

impl Default for ScrollbackLimits {
    fn default() -> Self {
        Self::new(DEFAULT_HISTORY_LINES, DEFAULT_HISTORY_BYTES)
    }
}

const DEFAULT_HISTORY_LINES: u32 = 50_000;

/// Shipped `defaults.history-bytes`: 10 MiB per pane (ADR-0143). Attach
/// leases history rather than encoding it (ADR-0119), so this prices only
/// the resident memory of a pane that has filled it.
pub const DEFAULT_HISTORY_BYTES: u32 = 10 * 1024 * 1024;
/// Largest accepted `defaults.history-bytes` (64 MiB): a resident-memory
/// bound, held for the life of the session.
pub const MAX_HISTORY_BYTES: u32 = 64 * 1024 * 1024;

/// Shipped `defaults.agent-log-bytes`: 4 MiB per agent session.
pub const DEFAULT_AGENT_LOG_BYTES: u32 = 4 * 1024 * 1024;
/// Largest accepted `defaults.agent-log-bytes` (64 MiB): the ring is
/// replayed on every attach, so this is a latency bound.
pub const MAX_AGENT_LOG_BYTES: u32 = 64 * 1024 * 1024;

/// Shipped `defaults.event-journal-entries` (ADR-0123).
pub const DEFAULT_EVENT_JOURNAL_ENTRIES: u32 = 4096;
/// Largest accepted `defaults.event-journal-entries`.
pub const MAX_EVENT_JOURNAL_ENTRIES: u32 = 1024 * 1024;

/// Shipped `defaults.event-journal-bytes`: 1 MiB (ADR-0123).
pub const DEFAULT_EVENT_JOURNAL_BYTES: u32 = 1024 * 1024;
/// Largest accepted `defaults.event-journal-bytes` (64 MiB).
pub const MAX_EVENT_JOURNAL_BYTES: u32 = 64 * 1024 * 1024;

/// Shipped `defaults.retain-on-exit-secs`: ten minutes (ADR-0124).
pub const DEFAULT_RETAIN_ON_EXIT_SECS: u32 = 600;
/// Shipped `defaults.retain-on-exit-max-secs`: one day (ADR-0124).
pub const DEFAULT_RETAIN_ON_EXIT_MAX_SECS: u32 = 86_400;
/// Shipped `defaults.retain-on-exit-max` (ADR-0124).
pub const DEFAULT_RETAIN_ON_EXIT_MAX: u32 = 256;
/// Largest accepted `defaults.retain-on-exit-max` (a memory bound).
pub const MAX_RETAIN_ON_EXIT_MAX: u32 = 4096;

/// Shipped `defaults.approval-ttl-secs`: two minutes (ADR-0128).
pub const DEFAULT_APPROVAL_TTL_SECS: u32 = 120;
/// Largest accepted `defaults.approval-ttl-secs`: one day.
pub const MAX_APPROVAL_TTL_SECS: u32 = 86_400;
/// Shipped `defaults.approval-max-pending` per connection.
pub const DEFAULT_APPROVAL_MAX_PENDING: u32 = 64;
/// Largest accepted `defaults.approval-max-pending`.
pub const MAX_APPROVAL_MAX_PENDING: u32 = 1024;
/// Shipped `defaults.approval-max-pending-total` server-wide.
pub const DEFAULT_APPROVAL_MAX_PENDING_TOTAL: u32 = 1024;
/// Largest accepted `defaults.approval-max-pending-total`.
pub const MAX_APPROVAL_MAX_PENDING_TOTAL: u32 = 65_536;

/// `[limits]` table: server-enforced ceilings on shared resources.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct LimitsCfg {
    /// Largest L3 metadata value stored at one key (`docs/spec/L3.md` §2),
    /// in bytes; a larger write is refused (ADR-0129).
    pub metadata_value_bytes: u32,
}

impl Default for LimitsCfg {
    fn default() -> Self {
        Self {
            metadata_value_bytes: DEFAULT_METADATA_VALUE_BYTES,
        }
    }
}

/// Shipped `limits.metadata-value-bytes`: 256 KiB, `docs/spec/L3.md` §2's
/// recommended cap.
pub const DEFAULT_METADATA_VALUE_BYTES: u32 = 256 * 1024;

/// What a client does with an OSC 52 clipboard write from a pane
/// (`defaults.clipboard-write`, ADR-0158). Ghostty's `clipboard-write`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum ClipboardWrite {
    /// Set the host clipboard (default, as Ghostty and tmux).
    #[default]
    Allow,
    /// Ask before setting it.
    Ask,
    /// Drop the write.
    Deny,
}

/// How a newly-spawned pane chooses its working directory
/// (`defaults.cwd-inheritance`).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum CwdInheritance {
    /// The focused pane's live working directory (default, as tmux).
    #[default]
    InheritFocused,
    /// Always `$HOME`.
    Home,
    /// The directory the session was created in.
    SessionRoot,
    /// The last working directory used in the window.
    LastCwdPerWindow,
}

/// Which geometry wins when concurrent views of one Terminal disagree on
/// size (`defaults.window-size`, ADR-0027). Mirrors tmux's `window-size`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum WindowSize {
    /// The smallest view's size; larger views letterbox (default).
    #[default]
    Smallest,
    /// The largest view's size; smaller views clamp.
    Largest,
    /// The most recently resized view's size.
    Latest,
    /// A fixed size set only by an explicit `RESIZE_TERMINAL` (ADR-0062).
    Manual,
}

// ---------------------------------------------------------------------------
// [keybindings]
// ---------------------------------------------------------------------------

/// `[keybindings]` table: prefix key, prefix-table, and global table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct KeybindingsCfg {
    /// Prefix chord (default `C-a`; some emulators swallow `C-Space`).
    pub prefix: String,
    /// Bindings that fire after the prefix.
    #[serde(rename = "prefix-table")]
    pub prefix_table: BTreeMap<String, Action>,
    /// Bindings that fire any time.
    pub global: BTreeMap<String, Action>,
    /// Show the which-key popup when no chord follows the prefix within
    /// [`Self::which_key_delay_ms`].
    #[serde(rename = "which-key")]
    pub which_key: bool,
    /// Milliseconds after the prefix before the which-key popup shows.
    #[serde(rename = "which-key-delay-ms")]
    pub which_key_delay_ms: u64,
}

impl Default for KeybindingsCfg {
    fn default() -> Self {
        Self {
            prefix: "C-a".to_owned(),
            prefix_table: BTreeMap::new(),
            global: BTreeMap::new(),
            which_key: true,
            which_key_delay_ms: 400,
        }
    }
}

/// An action attached to a binding, hook, or status slot: a bare name or an
/// inline table (`docs/consumers/tui.md` §4.2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Action {
    /// `"detach"`.
    Bare(String),
    /// `{ action = "new-pane", direction = "vertical" }`.
    Parameterized(ParamAction),
}

/// Parameterized action. Per-action argument validation lives in the
/// dispatcher, not here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParamAction {
    /// The action name. Hooks spell it `kind`; both are accepted.
    #[serde(alias = "kind")]
    pub action: String,
    /// Remaining inline-table fields.
    #[serde(flatten)]
    pub args: BTreeMap<String, toml::Value>,
}

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
    /// Width in columns. `0` (default) sizes automatically to a quarter of
    /// the viewport, bounded to 28-40 columns.
    pub width: u16,
    /// Which edge the sidebar docks to.
    pub position: SidebarPosition,
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
            width: 0,
            position: SidebarPosition::default(),
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
    /// Unset keeps the transitional posture: every admitted connection holds
    /// the owner's grant, and a remote listener logs a warning.
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
