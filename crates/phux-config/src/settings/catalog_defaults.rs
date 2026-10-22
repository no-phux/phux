//! [`CATALOG`](super::CATALOG) rows for `[defaults]`: what a new pane and
//! the server start with.

use super::{
    AGENT_LOG_BYTES_MAX, APPROVAL_MAX_PENDING_MAX, APPROVAL_MAX_PENDING_TOTAL_MAX,
    APPROVAL_TTL_MAX, Applies, CLIPBOARD_WRITE, CWD_INHERITANCE, EVENT_JOURNAL_BYTES_MAX,
    EVENT_JOURNAL_ENTRIES_MAX, HISTORY_BYTES_MAX, RETAIN_ON_EXIT_MAX_MAX, SettingKind,
    SettingSection, SettingSpec, U32_MAX, WINDOW_SIZE,
};

/// The `[defaults]` rows, in field order.
pub(super) const ROWS: &[SettingSpec] = &[
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
        key: "defaults.clipboard-write",
        section: SettingSection::Defaults,
        kind: SettingKind::Choice(CLIPBOARD_WRITE),
        summary: "What a program's OSC 52 clipboard write does",
        detail: "allow sets the host clipboard (Ghostty and tmux behavior); ask shows a \
                 prompt first; deny drops it. Only the pane this client is focused on can \
                 write, only from live output, and at most 1 MiB. Clipboard reads are \
                 never answered (ADR-0158).",
        applies: Applies::LiveReload,
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
];
