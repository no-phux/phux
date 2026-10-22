//! [`CATALOG`](super::CATALOG) rows after `[defaults]`: keybindings,
//! chrome, notifications, and the remaining scalar tables.

use super::{
    Applies, METADATA_VALUE_BYTES_MIN, NOTIFY_MODE, NOTIFY_PROTOCOL, SIDEBAR_POSITION,
    STATUS_POSITION, SettingKind, SettingSection, SettingSpec, U16_MAX, U32_MAX,
    VOICE_TIMEOUT_MAX_SECS, WHICH_KEY_DELAY_MAX_MS,
};

/// The rows from `[keybindings]` on, in section then field order.
pub(super) const ROWS: &[SettingSpec] = &[
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
        detail: "20, the default, fits a badge, a name, and a right-hand tag while leaving \
                 the terminal its columns. 0 sizes the strip to one quarter of the viewport \
                 bounded to 28..40 columns, for longer names on a wide terminal. The panes \
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
        key: "sidebar.agents-share",
        section: SettingSection::Sidebar,
        kind: SettingKind::Integer { min: 0, max: 100 },
        summary: "Percent of the sidebar body the Agents panel keeps",
        detail: "50 (half) by default. The Agents panel on top and the Sessions \
                 panel below share the body at a fixed line whatever the agent \
                 population, so gaining or resolving an agent never moves Sessions \
                 (ADR-0112). An empty Agents panel keeps its header and a quiet \
                 placeholder. Set 0 to drop the panel and give Sessions the whole \
                 strip.",
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
    // -- [notifications] ----------------------------------------------------
    SettingSpec {
        key: "notifications.mode",
        section: SettingSection::Notifications,
        kind: SettingKind::Choice(NOTIFY_MODE),
        summary: "Where agent notifications go",
        detail: "terminal writes an OSC notification to the outer terminal, so it works \
                 over SSH. system uses the desktop notifier of the machine the TUI runs on \
                 (osascript on macOS, notify-send elsewhere). bell rings the terminal bell. \
                 off posts nothing.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "notifications.terminal-protocol",
        section: SettingSection::Notifications,
        kind: SettingKind::Choice(NOTIFY_PROTOCOL),
        summary: "The OSC dialect terminal mode writes",
        detail: "auto picks OSC 99 for kitty, OSC 777 for Ghostty, WezTerm, and foot, \
                 and OSC 9 otherwise. Over SSH the environment may not name the \
                 terminal; pin the dialect your terminal speaks.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "notifications.blocked",
        section: SettingSection::Notifications,
        kind: SettingKind::Bool,
        summary: "Notify when an agent is blocked or asks for you",
        detail: "Fires on the edge into blocked, or when an agent raises an ask. An agent \
                 already blocked when you attach does not fire.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "notifications.done",
        section: SettingSection::Notifications,
        kind: SettingKind::Bool,
        summary: "Notify when a working agent finishes",
        detail: "Fires once per working-to-done or working-to-idle edge. Looking at the \
                 pane before the notification is posted cancels it.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "notifications.suppress-focused",
        section: SettingSection::Notifications,
        kind: SettingKind::Bool,
        summary: "Stay quiet about the pane you are looking at",
        detail: "When true, an agent in the focused pane does not notify while the \
                 terminal window has focus. A terminal that never reports focus counts as \
                 focused.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "notifications.sound",
        section: SettingSection::Notifications,
        kind: SettingKind::Bool,
        summary: "Add a sound to each notification",
        detail: "terminal mode also rings the bell; system mode asks the desktop notifier \
                 for its default sound. bell mode always rings.",
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
    // -- [organize] ---------------------------------------------------------
    SettingSpec {
        key: "organize.jev-key-command",
        section: SettingSection::Organize,
        kind: SettingKind::Argv,
        summary: "Command that prints the TypeSafe API key for organize's Jev judge",
        detail: "Run when TYPESAFE_API_KEY is unset; its trimmed stdout is the key. Unset \
                 means the environment only, and without a key organize decides by \
                 repository matching.",
        applies: Applies::NextAttach,
    },
    SettingSpec {
        key: "organize.offline",
        section: SettingSection::Organize,
        kind: SettingKind::Bool,
        summary: "Organize by repository matching only; send nothing off the machine",
        detail: "true never asks Jev, even with a key: organize places panes by the \
                 repository their directory is in. Only pane names, titles, directories, \
                 and repositories are ever sent, never screen contents.",
        applies: Applies::NextAttach,
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
