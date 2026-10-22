//! `[defaults]` and `[limits]`: pane defaults, history and journal budgets,
//! and the ceilings the server enforces.

use serde::{Deserialize, Serialize};

/// `[defaults]` table. See `docs/reference/config.md` for shipped values.
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
