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

pub(crate) use write::replace_atomically;
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
    /// `[theme]`.
    Theme,
    /// `[experimental]`.
    Experimental,
    /// `[voice]`.
    Voice,
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
        Self::Theme,
        Self::Experimental,
        Self::Voice,
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
            Self::Theme => "Theme",
            Self::Experimental => "Experimental",
            Self::Voice => "Voice",
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
            Self::Theme => "theme",
            Self::Experimental => "experimental",
            Self::Voice => "voice",
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
const CWD_INHERITANCE: &[&str] = &[
    "inherit-focused",
    "home",
    "session-root",
    "last-cwd-per-window",
];
const WINDOW_SIZE: &[&str] = &["smallest", "largest", "latest", "manual"];
const STATUS_POSITION: &[&str] = &["bottom", "top"];
const SIDEBAR_POSITION: &[&str] = &["left", "right"];

/// Every scalar setting of the schema, in section then field order.
pub const CATALOG: &[SettingSpec] = &[
    // -- [defaults] ---------------------------------------------------------
    SettingSpec {
        key: "defaults.shell",
        section: SettingSection::Defaults,
        kind: SettingKind::OptionalText,
        summary: "Shell for server-spawned panes; unset honors $SHELL",
        detail: "The program server-spawned panes run when nothing names a command: the \
                 seed session, attach-time session creation, and a SPAWN_RESOURCE whose \
                 wire frame carries no command. Unset resolves $SHELL at server startup, \
                 falling back to /bin/sh. A wire command always wins over this default.",
        applies: Applies::NextSpawn,
    },
    SettingSpec {
        key: "defaults.term",
        section: SettingSection::Defaults,
        kind: SettingKind::Text,
        summary: "TERM advertised to the inner program of every spawned pane",
        detail: "xterm-256color is the safe universal baseline: 256 colors and the standard \
                 xterm keys with no kitty-keyboard advertisement, so ncurses TUIs like htop \
                 keep working. Set \"ghostty\" to opt into ghostty's extended terminfo once \
                 your apps are known to round-trip the kitty keyboard protocol. A per-spawn \
                 TERM in the wire frame's env always wins.",
        applies: Applies::NextSpawn,
    },
    SettingSpec {
        key: "defaults.history-limit",
        section: SettingSection::Defaults,
        kind: SettingKind::Integer {
            min: 0,
            max: U32_MAX,
        },
        summary: "Lines of scrollback retained per pane",
        detail: "An upper bound, not a reservation, and not the only one: libghostty \
                 prunes on whichever of this line limit and history-bytes is reached \
                 first. On anything but a narrow grid the byte limit binds, so raising \
                 this alone buys no depth.",
        applies: Applies::NextSpawn,
    },
    SettingSpec {
        key: "defaults.history-bytes",
        section: SettingSection::Defaults,
        kind: SettingKind::Integer {
            min: 0,
            max: HISTORY_BYTES_MAX,
        },
        summary: "Bytes of scrollback retained per pane; costs resident memory",
        detail: "The bound that actually limits a pane's memory. Raising it buys depth and \
                 costs resident memory, roughly this many bytes per pane for the life of \
                 the session; attach is unaffected, because retained pages are leased \
                 rather than re-encoded. The 10 MiB default keeps about 14,400 rows at 80 \
                 columns; a pane that never fills it never pays for it. 67108864 (64 MiB) is the accepted maximum; phux config check \
                 rejects more.",
        applies: Applies::NextSpawn,
    },
    SettingSpec {
        key: "defaults.agent-log-bytes",
        section: SettingSection::Defaults,
        kind: SettingKind::Integer {
            min: 0,
            max: AGENT_LOG_BYTES_MAX,
        },
        summary: "Bytes of agent-session records retained per session stream",
        detail: "An agent session keeps a bounded ring of the JSONL records its producer \
                 appends and replays that ring when a client attaches to the stream \
                 (ADR-0103). An append past the ceiling evicts the oldest records and \
                 counts a tombstone the bootstrap reports; it is never an error. Records \
                 are small, so the 4 MiB default is tens of thousands of them. 67108864 \
                 (64 MiB) is the accepted maximum; phux config check rejects more.",
        applies: Applies::NextSpawn,
    },
    SettingSpec {
        key: "defaults.event-journal-entries",
        section: SettingSection::Defaults,
        kind: SettingKind::Integer {
            min: 0,
            max: EVENT_JOURNAL_ENTRIES_MAX,
        },
        summary: "Recent events the server keeps for cursor replay",
        detail: "The server stamps every event with one sequence and keeps the most recent \
                 ones (ADR-0123), so a watcher or waiter that reconnects with a cursor is \
                 replayed what it missed. A cursor older than the ring is told it missed \
                 events and re-reads state; nothing is lost silently. The oldest events go \
                 first when this or event-journal-bytes is reached. 1048576 is the \
                 accepted maximum; phux config check rejects more.",
        applies: Applies::NextSpawn,
    },
    SettingSpec {
        key: "defaults.event-journal-bytes",
        section: SettingSection::Defaults,
        kind: SettingKind::Integer {
            min: 0,
            max: EVENT_JOURNAL_BYTES_MAX,
        },
        summary: "Estimated bytes of recent events the server keeps",
        detail: "The memory bound beside event-journal-entries: most events are tiny, but \
                 titles, working directories, and agent questions are not. The 1 MiB \
                 default holds thousands of ordinary events. 67108864 (64 MiB) is the \
                 accepted maximum; phux config check rejects more.",
        applies: Applies::NextSpawn,
    },
    SettingSpec {
        key: "defaults.retain-on-exit",
        section: SettingSection::Defaults,
        kind: SettingKind::Bool,
        summary: "Keep every pane inspectable after its process exits",
        detail: "A retained pane stays listed as exited, with its exit status, last screen, \
                 and history, until retain-on-exit-secs pass, retain-on-exit-max evicts it, \
                 or it is killed (ADR-0124). false retains only panes whose spawner asked; \
                 true retains every pane that does not say, seed panes included. The reference TUI marks \
                 a retained pane in the sidebar and window tabs, shows its last screen with an exited \
                 badge while focused, and refuses input to it.",
        applies: Applies::NextSpawn,
    },
    SettingSpec {
        key: "defaults.retain-on-exit-secs",
        section: SettingSection::Defaults,
        kind: SettingKind::Integer {
            min: 0,
            max: U32_MAX,
        },
        summary: "Seconds a retained pane stays after its process exits",
        detail: "What a spawn that asked for the server default, or one retained by \
                 retain-on-exit, gets. Capped by retain-on-exit-max-secs.",
        applies: Applies::NextSpawn,
    },
    SettingSpec {
        key: "defaults.retain-on-exit-max-secs",
        section: SettingSection::Defaults,
        kind: SettingKind::Integer {
            min: 0,
            max: U32_MAX,
        },
        summary: "The longest any pane is retained after its process exits",
        detail: "A spawner may ask for any retention; the server caps it here.",
        applies: Applies::NextSpawn,
    },
    SettingSpec {
        key: "defaults.retain-on-exit-max",
        section: SettingSection::Defaults,
        kind: SettingKind::Integer {
            min: 0,
            max: RETAIN_ON_EXIT_MAX_MAX,
        },
        summary: "How many exited panes the server retains at once",
        detail: "Retaining one more closes the oldest. Each retained pane holds its grid \
                 and history until it is purged, up to history-bytes each: about 2.5 GiB \
                 at the default 256 with the 10 MiB default history. It holds no PTY or \
                 descriptor. 0 retains none; 4096 is the accepted maximum; phux config \
                 check rejects more and the server clamps to it.",
        applies: Applies::NextSpawn,
    },
    SettingSpec {
        key: "defaults.approval-ttl-secs",
        section: SettingSection::Defaults,
        kind: SettingKind::Integer {
            min: 1,
            max: APPROVAL_TTL_MAX,
        },
        summary: "Seconds a held action waits for a decision before it expires",
        detail: "Only a workload grant spelled ?signal holds anything, so this is inert \
                 until one exists. A held command that nobody approves or denies in time \
                 is refused as expired and never runs. 86400 (one day) is the accepted \
                 maximum; phux config check rejects 0 and anything larger.",
        applies: Applies::NextSpawn,
    },
    SettingSpec {
        key: "defaults.approval-max-pending",
        section: SettingSection::Defaults,
        kind: SettingKind::Integer {
            min: 0,
            max: APPROVAL_MAX_PENDING_MAX,
        },
        summary: "How many actions one connection may hold for approval at once",
        detail: "One more is refused as resource exhausted rather than held. 0 refuses \
                 every hold. 1024 is the accepted maximum; phux config check rejects \
                 more and the server clamps to it.",
        applies: Applies::NextSpawn,
    },
    SettingSpec {
        key: "defaults.approval-max-pending-total",
        section: SettingSection::Defaults,
        kind: SettingKind::Integer {
            min: 0,
            max: APPROVAL_MAX_PENDING_TOTAL_MAX,
        },
        summary: "How many actions the whole server may hold for approval at once",
        detail: "The server-wide bound beside the per-connection one: one more hold is \
                 refused as resource exhausted rather than held. 65536 is the accepted \
                 maximum; phux config check rejects more and the server clamps to it.",
        applies: Applies::NextSpawn,
    },
    SettingSpec {
        key: "defaults.mouse",
        section: SettingSection::Defaults,
        kind: SettingKind::Bool,
        summary: "Enable outer-terminal mouse tracking on attach",
        detail: "true emits DECSET ?1002h?1006h on attach so divider drag-to-resize and \
                 click-to-focus work without an inner program turning mouse mode on, and \
                 restores the host terminal's mouse state on detach. false is the \
                 pass-through-only escape hatch: the host's native click-drag selection \
                 is left untouched.",
        applies: Applies::NextAttach,
    },
    SettingSpec {
        key: "defaults.cwd-inheritance",
        section: SettingSection::Defaults,
        kind: SettingKind::Choice(CWD_INHERITANCE),
        summary: "How a new pane picks its working directory",
        detail: "Applies when a SPAWN_RESOURCE leaves cwd unset; an explicit cwd always \
                 wins. inherit-focused reads the focused pane's live PTY working directory \
                 (tmux behavior); home uses $HOME. session-root and last-cwd-per-window are \
                 accepted but not yet resolved server-side.",
        applies: Applies::NextSpawn,
    },
    SettingSpec {
        key: "defaults.spawn-on-attach",
        section: SettingSection::Defaults,
        kind: SettingKind::OptionalText,
        summary: "Command an auto-created session starts with; unset uses the shell",
        detail: "What naked phux (or phux attach with no name) runs, via $SHELL -c, when it \
                 auto-creates its default session. phux new ignores this and gives an \
                 explicitly created session a plain shell. Unset means use defaults.shell.",
        applies: Applies::NextSpawn,
    },
    SettingSpec {
        key: "defaults.session-name-template",
        section: SettingSection::Defaults,
        kind: SettingKind::Text,
        summary: "Name template for auto-created sessions",
        detail: "${cwd-basename} expands to the basename of the client's working directory \
                 at session-create time; ${random-name} expands to a generated \
                 adjective-noun pair such as drifting-cedar, and phux new redraws a \
                 taken pick before adding a numeric suffix; unknown placeholders pass \
                 through verbatim. The result is made selector-safe: a colon or `/@` \
                 becomes an underscore, as does a leading @, #, or % (a directory named \
                 @proj names the session _proj). Set \"default\" for a fixed name.",
        applies: Applies::NextSpawn,
    },
    SettingSpec {
        key: "defaults.window-size",
        section: SettingSection::Defaults,
        kind: SettingKind::Choice(WINDOW_SIZE),
        summary: "Which view's size wins when views of one terminal disagree",
        detail: "A Terminal is one PTY and one grid, so it renders one size; a view wanting \
                 another letterboxes rather than reflowing. smallest never crops (tmux's \
                 default); largest lets smaller views clamp; latest tracks the most recent \
                 resize; manual holds a size set only by phux resize.",
        applies: Applies::NextSpawn,
    },
    // -- [keybindings] ------------------------------------------------------
    SettingSpec {
        key: "keybindings.prefix",
        section: SettingSection::Keybindings,
        kind: SettingKind::Chord,
        summary: "Prefix chord captured before everything else",
        detail: "After the prefix, the next keystroke is matched against the prefix-table. \
                 Chord syntax: modifier letters C, M, A, S joined to the key by a dash, \
                 e.g. C-a or M-Space. C-a is the default because some emulators swallow \
                 C-Space before it reaches the client.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "keybindings.which-key",
        section: SettingSection::Keybindings,
        kind: SettingKind::Bool,
        summary: "Show the which-key popup after the prefix",
        detail: "Press the prefix and hesitate for which-key-delay-ms and a panel lists \
                 every prefix-table continuation, built from your bindings. It is \
                 display-only: any key dismisses it and executes normally; Esc cancels the \
                 prefix.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "keybindings.which-key-delay-ms",
        section: SettingSection::Keybindings,
        kind: SettingKind::Integer {
            min: 0,
            max: WHICH_KEY_DELAY_MAX_MS,
        },
        summary: "Milliseconds of hesitation before the which-key popup",
        detail: "400 is deliberately snappier than tmux-ish 600: the popup is the primary \
                 discovery surface, so it should feel like a hint that arrives while you \
                 hesitate, not a timeout you wait out. A fast continuation chord always \
                 suppresses it. Capped here at 60000 (one minute): a hint that arrives \
                 after a minute of hesitation is no hint, and the cap keeps an editor from \
                 stepping into the full u64 range.",
        applies: Applies::LiveReload,
    },
    // -- [status] -----------------------------------------------------------
    SettingSpec {
        key: "status.position",
        section: SettingSection::Status,
        kind: SettingKind::Choice(STATUS_POSITION),
        summary: "Which outer-terminal row the status bar reserves",
        detail: "top (the default) or bottom. The bar's widget lists are composition, not \
                 knobs, and are edited in the file directly.",
        applies: Applies::LiveReload,
    },
    // -- [sidebar] ----------------------------------------------------------
    SettingSpec {
        key: "sidebar.enabled",
        section: SettingSection::Sidebar,
        kind: SettingKind::Bool,
        summary: "Show the window sidebar",
        detail: "On by default because the strip is the product's answer to which agent \
                 needs you. prefix-b toggles it for the life of the attach without \
                 touching this key. Below sidebar.width + chrome.min-pane-cols columns the \
                 strip is not reserved at all.",
        applies: Applies::NextAttach,
    },
    SettingSpec {
        key: "sidebar.width",
        section: SettingSection::Sidebar,
        kind: SettingKind::Integer {
            min: 0,
            max: U16_MAX,
        },
        summary: "Sidebar width in columns; 0 sizes it automatically",
        detail: "0, the default, sizes the strip to one quarter of the viewport bounded to \
                 28..40 columns, so names get room on a wide terminal while the classic \
                 80-column layout stays compact. A positive value fixes the width. The panes \
                 tile into the remaining columns.",
        applies: Applies::NextAttach,
    },
    SettingSpec {
        key: "sidebar.position",
        section: SettingSection::Sidebar,
        kind: SettingKind::Choice(SIDEBAR_POSITION),
        summary: "Which edge the sidebar docks to",
        detail: "left (the default) or right.",
        applies: Applies::NextAttach,
    },
    SettingSpec {
        key: "sidebar.hosts",
        section: SettingSection::Sidebar,
        kind: SettingKind::Bool,
        summary: "Show your other machines in the sidebar",
        detail: "On (the default), the Sessions area is segmented by machine: the one this \
                 terminal is attached to, then this machine and every registered host \
                 (`phux host ls`), each with its sessions. Clicking a session on another \
                 machine re-attaches there. Off lists only the attached server's sessions.",
        applies: Applies::NextAttach,
    },
    SettingSpec {
        key: "sidebar.hosts-refresh-secs",
        section: SettingSection::Sidebar,
        kind: SettingKind::Integer {
            min: 2,
            max: U32_MAX,
        },
        summary: "Seconds between refreshes of other machines' sessions",
        detail: "How often the hosts provider re-lists every machine. Each run dials every \
                 registered host at once with a short deadline, so a host that is down \
                 costs one deadline, not a hang. The provider command itself is \
                 `sidebar.hosts-provider` (a list, edited in the file).",
        applies: Applies::NextAttach,
    },
    // -- [chrome] -----------------------------------------------------------
    SettingSpec {
        key: "chrome.compact-cols",
        section: SettingSection::Chrome,
        kind: SettingKind::Integer {
            min: 0,
            max: U16_MAX,
        },
        summary: "Viewport width at or below which overlays go full-bleed",
        detail: "At or below this many columns the viewport is column-starved and overlays \
                 go full-bleed horizontally instead of floating. 0 disables the threshold; \
                 a very large value pins everything compact. Both are legitimate.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "chrome.compact-rows",
        section: SettingSection::Chrome,
        kind: SettingKind::Integer {
            min: 0,
            max: U16_MAX,
        },
        summary: "Viewport height at or below which overlays go full-bleed",
        detail: "At or below this many rows the viewport is row-starved and overlays go \
                 full-bleed vertically. Judged independently of compact-cols, because a \
                 short wide terminal and a narrow tall one want opposite things. 0 \
                 disables the threshold.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "chrome.min-pane-cols",
        section: SettingSection::Chrome,
        kind: SettingKind::Integer {
            min: 0,
            max: U16_MAX,
        },
        summary: "Narrowest pane area worth tiling into, in columns",
        detail: "The sidebar strip is not reserved below sidebar.width + this many columns, \
                 so lowering it keeps the strip on a narrower terminal. 0 means the sidebar \
                 never yields.",
        applies: Applies::LiveReload,
    },
    // -- [experimental] -----------------------------------------------------
    SettingSpec {
        key: "experimental.predictive-echo",
        section: SettingSection::Experimental,
        kind: SettingKind::OptionalBool,
        summary: "Mosh-class predictive local echo; unset lets the dial decide",
        detail: "Unset means the transport decides: on over a remote dial, off over the \
                 local Unix socket (a loopback QUIC or WebSocket dial counts as local). true \
                 engages prediction everywhere including UDS; false disables it everywhere \
                 including remote dials. Experimental: may change without notice.",
        applies: Applies::NextAttach,
    },
    // -- [voice] ------------------------------------------------------------
    SettingSpec {
        key: "voice.transcriber",
        section: SettingSection::Voice,
        kind: SettingKind::Argv,
        summary: "Transcriber command behind TRANSCRIBE; unset refuses the request",
        detail: "An argv the server runs on an uploaded clip. The token {path} is replaced \
                 by the clip's absolute path (appended as a final argument when no argument \
                 contains it); the command's stdout, trimmed, is the transcript. Unset means \
                 TRANSCRIBE is refused with a remedy.",
        applies: Applies::NextSpawn,
    },
    SettingSpec {
        key: "voice.timeout-secs",
        section: SettingSection::Voice,
        kind: SettingKind::OptionalInteger {
            min: 1,
            max: VOICE_TIMEOUT_MAX_SECS,
        },
        summary: "Seconds before the transcriber is killed; unset means 30",
        detail: "The server waits this long for the transcriber before refusing the \
                 request and killing the process. Capped here at 3600 (one hour): the \
                 request is held open the whole time, and an hour is already far past any \
                 useful clip. 0 would refuse every request, so the editor floor is 1.",
        applies: Applies::NextSpawn,
    },
    // -- [limits] -------------------------------------------------------
    SettingSpec {
        key: "limits.metadata-value-bytes",
        section: SettingSection::Limits,
        kind: SettingKind::Integer {
            min: METADATA_VALUE_BYTES_MIN,
            max: U32_MAX,
        },
        summary: "Largest L3 metadata value stored at one key",
        detail: "A SET_METADATA write over this cap is silently refused — the frame has no \
                 reply, so the writer is not told; nothing is stored (ADR-0129). The one \
                 shared metadata store backs every consumer's convention — session names, \
                 tags, and any named layout projection alike — so the cap is global, not per \
                 key family. Floored at 4096 bytes: the built-in agent-session record write \
                 is checked against its own 4096-byte limit only after this cap, so a lower \
                 cap would silently break session-create, rename, and keep-empty metadata too.",
        applies: Applies::NextSpawn,
    },
];

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
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;

    /// Catalogue keys whose shipped state is unset.
    const OPTIONAL_KEYS: &[&str] = &[
        "defaults.shell",
        "defaults.spawn-on-attach",
        "experimental.predictive-echo",
        "voice.transcriber",
        "voice.timeout-secs",
    ];

    /// Composite subtrees under the section tables: composition, not knobs.
    const COMPOSITE_KEYS: &[&str] = &[
        "status.left",
        "status.center",
        "status.right",
        "keybindings.prefix-table",
        "keybindings.global",
        "theme",
    ];

    fn scalar_leaves(value: &toml::Value, path: &str, out: &mut BTreeSet<String>) {
        if COMPOSITE_KEYS.contains(&path) {
            return;
        }
        match value {
            toml::Value::Table(table) => {
                for (key, child) in table {
                    scalar_leaves(child, &format!("{path}.{key}"), out);
                }
            }
            toml::Value::Array(_) => {}
            _ => {
                out.insert(path.to_owned());
            }
        }
    }

    /// The coverage gate, both directions: every scalar leaf under the
    /// section tables has exactly one row, and every row names such a leaf
    /// or a documented unset key with an optional kind.
    #[test]
    fn catalog_covers_every_scalar_leaf_of_the_schema() {
        let cfg = crate::parse_with_defaults("", Path::new("test.toml")).expect("defaults parse");
        let defaults = toml::Table::try_from(cfg).expect("Config serializes");
        let mut expected = BTreeSet::new();
        for section in SettingSection::ALL {
            if let Some(table) = defaults.get(section.table()) {
                scalar_leaves(table, section.table(), &mut expected);
            }
        }
        for key in OPTIONAL_KEYS {
            assert!(
                expected.insert((*key).to_owned()),
                "{key} now has a default"
            );
        }

        let mut catalog_keys = BTreeSet::new();
        let mut last_section = 0;
        for spec in CATALOG {
            assert!(
                catalog_keys.insert(spec.key.to_owned()),
                "duplicate {}",
                spec.key
            );
            let optional = matches!(
                spec.kind,
                SettingKind::OptionalBool
                    | SettingKind::OptionalText
                    | SettingKind::OptionalInteger { .. }
                    | SettingKind::Argv
            );
            assert_eq!(optional, OPTIONAL_KEYS.contains(&spec.key), "{}", spec.key);
            let index = SettingSection::ALL
                .iter()
                .position(|s| *s == spec.section)
                .expect("section is in ALL");
            assert!(
                index >= last_section,
                "{} is out of section order",
                spec.key
            );
            last_section = index;
            assert_eq!(spec.table(), spec.section.table(), "{}", spec.key);
            assert!(spec.summary.chars().count() <= 72, "{}", spec.key);
            assert!(
                !spec.summary.ends_with('.') && spec.detail.ends_with('.'),
                "{}",
                spec.key
            );
        }
        assert_eq!(catalog_keys, expected, "CATALOG drifted from the schema");
    }

    /// Every `Choice` variant is one the schema accepts, and a bogus one is
    /// rejected.
    #[test]
    fn choice_variants_match_the_schema_enums() {
        let mut seen = 0;
        for spec in CATALOG {
            let SettingKind::Choice(variants) = spec.kind else {
                continue;
            };
            seen += 1;
            let parse = |variant: &str| {
                let text = format!("[{}]\n{} = \"{variant}\"\n", spec.table(), spec.leaf());
                crate::parse_with_defaults(&text, Path::new("t.toml"))
            };
            for variant in variants {
                assert!(parse(variant).is_ok(), "{}: `{variant}` rejected", spec.key);
            }
            assert!(parse("not-a-real-variant").is_err(), "{}", spec.key);
        }
        assert_eq!(seen, 4);
    }

    #[test]
    fn every_integer_default_lies_within_its_bounds() {
        let snapshot = SettingsSnapshot::load("", Path::new("t.toml")).expect("load");
        for spec in CATALOG {
            let SettingKind::Integer { min, max } = spec.kind else {
                continue;
            };
            let default = snapshot
                .default_at(spec.key)
                .and_then(toml::Value::as_integer)
                .expect("integer default");
            assert!((min..=max).contains(&default), "{}: {default}", spec.key);
        }
    }

    #[test]
    fn origins_attribute_user_and_extends_layers() {
        let snapshot = SettingsSnapshot::load("", Path::new("t.toml")).expect("load");
        for spec in CATALOG {
            assert!(snapshot.origin_at(spec.key).is_none(), "{}", spec.key);
            assert_eq!(snapshot.value_at(spec.key), snapshot.default_at(spec.key));
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let layer_path = dir.path().join("layers").join("team.toml");
        std::fs::create_dir_all(layer_path.parent().unwrap()).unwrap();
        std::fs::write(&layer_path, "[chrome]\ncompact-cols = 80\n").unwrap();
        let user_path = dir.path().join("config.toml");
        let user = "extends = [\"team\"]\n\n[sidebar]\nwidth = 32\n[theme]\naccent = \"#f00\"\n";
        let snapshot = SettingsSnapshot::load(user, &user_path).expect("load");
        assert_eq!(
            snapshot.origin_at("chrome.compact-cols"),
            Some(&LayerSource::Extended(layer_path))
        );
        assert_eq!(snapshot.value_at("chrome.compact-cols"), Some(&80.into()));
        assert_eq!(
            snapshot.origin_at("sidebar.width"),
            Some(&LayerSource::User(user_path))
        );
        assert_eq!(snapshot.value_at("sidebar.width"), Some(&32.into()));
        assert_eq!(snapshot.default_at("sidebar.width"), Some(&0.into()));
        assert!(snapshot.origin_at("sidebar.enabled").is_none());
        assert_eq!(snapshot.value_at("theme.accent"), Some(&"#f00".into()));
        assert!(snapshot.default_at("theme.accent").is_none());
        assert!(snapshot.value_at("theme.nonexistent").is_none());
        assert!(snapshot.value_at("").is_none());
    }

    #[test]
    fn render_value_shapes() {
        assert_eq!(render_value(None), "unset");
        assert_eq!(
            render_value(Some(&"xterm-256color".into())),
            "xterm-256color"
        );
        assert_eq!(render_value(Some(&false.into())), "false");
        assert_eq!(render_value(Some(&400.into())), "400");
        let argv = vec!["curl", "-sf", "-F", "file=@{path}", "a b", "it's"].into();
        assert_eq!(
            render_value(Some(&argv)),
            "curl -sf -F file=@{path} 'a b' 'it'\\''s'"
        );
    }
}
