//! `[defaults]` and `[limits]`: pane defaults, history and journal budgets,
//! and the ceilings the server enforces.

use phux_config_core::limits::DEFAULT_HISTORY_LINES;
pub use phux_config_core::limits::{DEFAULT_HISTORY_BYTES, MAX_HISTORY_BYTES, ScrollbackLimits};
pub use phux_config_core::{ClipboardRead, ClipboardWrite};
use serde::{Deserialize, Serialize};

/// `[defaults]` table. See `docs/reference/config.md` for shipped values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent config switches, one key each"
)]
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

    /// Make the default shell of new panes mark its prompts and commands
    /// (OSC 133) without the person editing a startup file. Read at server
    /// start.
    #[serde(rename = "shell-integration")]
    pub shell_integration: bool,

    /// Give panes a stand-in `BROWSER` that prints a link and copies it to
    /// the attached person's clipboard, for a machine with no screen. Read
    /// at server start.
    #[serde(rename = "browser-shim")]
    pub browser_shim: bool,

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

    /// What the client does with an OSC 52 clipboard read from the pane it
    /// is focused on (`docs/program-sequences.md#clipboard-osc-52`).
    #[serde(rename = "clipboard-read")]
    pub clipboard_read: ClipboardRead,

    /// How a freshly-spawned pane chooses its working directory.
    #[serde(rename = "cwd-inheritance")]
    pub cwd_inheritance: CwdInheritance,

    /// The directory `cwd-inheritance = "fixed"` starts new panes and
    /// sessions in; `~/` expands. Unset is `$HOME`.
    #[serde(rename = "fixed-cwd")]
    pub fixed_cwd: Option<String>,

    /// Command to spawn when `phux` auto-creates a session on attach.
    /// `None` uses [`Self::shell`].
    #[serde(rename = "spawn-on-attach")]
    pub spawn_on_attach: Option<String>,

    /// Naming template for auto-created sessions. Placeholders:
    /// `${cwd-basename}` (default) and `${random-name}`; unknown ones pass
    /// through verbatim.
    #[serde(rename = "session-name-template")]
    pub session_name_template: String,

    /// The session a bare `phux` or `phux attach` and a new Rune window
    /// open, created when missing. `--session` and links still win. Unset
    /// opens the session used last.
    #[serde(rename = "attach-session")]
    pub attach_session: Option<String>,

    /// Which size wins when concurrent views of one Terminal disagree
    /// (ADR-0027). Governs views only: an explicit `RESIZE_TERMINAL` always
    /// applies (ADR-0062).
    #[serde(rename = "window-size")]
    pub window_size: WindowSize,

    /// Whether the server holds an idle-sleep inhibitor (`caffeinate`,
    /// `systemd-inhibit`) so a working agent is not put to sleep.
    #[serde(rename = "keep-awake")]
    pub keep_awake: KeepAwake,
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
            shell_integration: false,
            browser_shim: false,
            retain_on_exit: false,
            retain_on_exit_secs: DEFAULT_RETAIN_ON_EXIT_SECS,
            retain_on_exit_max_secs: DEFAULT_RETAIN_ON_EXIT_MAX_SECS,
            retain_on_exit_max: DEFAULT_RETAIN_ON_EXIT_MAX,
            approval_ttl_secs: DEFAULT_APPROVAL_TTL_SECS,
            approval_max_pending: DEFAULT_APPROVAL_MAX_PENDING,
            approval_max_pending_total: DEFAULT_APPROVAL_MAX_PENDING_TOTAL,
            mouse: true,
            clipboard_write: ClipboardWrite::default(),
            clipboard_read: ClipboardRead::default(),
            cwd_inheritance: CwdInheritance::default(),
            fixed_cwd: None,
            spawn_on_attach: None,
            session_name_template: "${cwd-basename}".to_owned(),
            attach_session: None,
            window_size: WindowSize::default(),
            keep_awake: KeepAwake::default(),
        }
    }
}

impl DefaultsCfg {
    /// The configured scrollback bounds as one value.
    #[must_use]
    pub const fn scrollback_limits(&self) -> ScrollbackLimits {
        ScrollbackLimits::new(self.history_limit, self.history_bytes)
    }

    /// Where new panes and sessions start under `cwd-inheritance = "fixed"`
    /// (`fixed-cwd`, else `$HOME`); `None` under any other policy.
    #[must_use]
    pub fn fixed_start_dir(&self) -> Option<std::path::PathBuf> {
        (self.cwd_inheritance == CwdInheritance::Fixed)
            .then(|| crate::theme::expand_home(self.fixed_cwd.as_deref().unwrap_or("~/")))
    }
}

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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct LimitsCfg {
    /// Largest L3 metadata value stored at one key (`docs/protocol.md#commands-events-resources`),
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

/// Shipped `limits.metadata-value-bytes`: 256 KiB, `docs/protocol.md#commands-events-resources`'s
/// recommended cap.
pub const DEFAULT_METADATA_VALUE_BYTES: u32 = 256 * 1024;

/// How a newly-spawned pane chooses its working directory
/// (`defaults.cwd-inheritance`).
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash, schemars::JsonSchema,
)]
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
    /// Always `defaults.fixed-cwd` (else `$HOME`), for new sessions too.
    Fixed,
}

/// Which geometry wins when concurrent views of one Terminal disagree on
/// size (`defaults.window-size`, ADR-0027). Mirrors tmux's `window-size`.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum WindowSize {
    /// The smallest view's size; larger views letterbox (default).
    #[default]
    Smallest,
    /// The largest view's size; smaller views clamp.
    Largest,
    /// The most recently resized view's size.
    Latest,
    /// The size of the view last used: the one that most recently typed,
    /// clicked, pasted, gained focus, attached or resized. Lets a phone and
    /// a desktop share a pane without the idle one shrinking it.
    Active,
    /// A fixed size set only by an explicit `RESIZE_TERMINAL` (ADR-0062).
    Manual,
}

/// When the server keeps the machine from idle-sleeping
/// (`defaults.keep-awake`).
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum KeepAwake {
    /// Never (default).
    #[default]
    Off,
    /// While at least one agent's state is `working`.
    WhileAgentsWork,
    /// For as long as the server runs.
    Always,
}

impl KeepAwake {
    /// Global metadata key a client writes to switch the running server's
    /// mode until it restarts; the value is the mode as a JSON string
    /// (`"always"`). Unset, the server follows `defaults.keep-awake`.
    pub const MODE_KEY: &str = "phux.keep-awake.mode/v1";

    /// Global metadata key of the server's keep-awake state, published while
    /// the mode is not `off`: `{"state": "on" | "idle" | "unavailable",
    /// "working": N, "mode": "while-agents-work" | "always"}`;
    /// `unavailable` means the inhibitor is wanted but could not be held.
    pub const STATUS_KEY: &str = "phux.keep-awake/v1";
}
