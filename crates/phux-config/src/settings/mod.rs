//! Schema-pinned catalogue of scalar settings, with provenance (phux-u1tq.3).
//!
//! Three things live here, all in service of a settings editor that must
//! round-trip through the user's `config.toml` rather than a side channel
//! (ADR-0023: pure-config, defaults as a live base layer):
//!
//! 1. [`CATALOG`]: one [`SettingSpec`] per scalar key of the schema, with
//!    its editing [`SettingKind`], a summary, a detail paragraph sourced
//!    from the schema and `default.toml` comments, and when a change takes
//!    effect ([`Applies`]). Composite keys (widget lists, binding tables,
//!    the free-form `[theme]` map, registry arrays) are deliberately
//!    absent: they are composition, not knobs. A test walks the schema and
//!    fails when a scalar field has no row here or a row names no field,
//!    so the catalogue cannot drift from the schema.
//! 2. [`SettingsSnapshot`]: the merged layer stack resolved once, so an
//!    editor can ask each setting for its effective value, its shipped
//!    default, and which layer set it ([`SettingEntry`]).
//! 3. The writer ([`apply_edit`] / [`write_edit`]): sets or unsets one
//!    dotted key in the user's file, preserving every other byte, and
//!    validates the result before anything touches disk.

mod write;

use std::path::Path;

pub use write::{Edit, EditOutcome, apply_edit, write_edit};

use crate::{ConfigError, ConfigProvenance, LayerSource, MAX_HISTORY_BYTES};

/// A top-level `config.toml` table that holds scalar settings.
///
/// The composite-only top-level keys (`hooks`, `plugins`, `satellites`,
/// `connector`, `remote`) are not sections: they carry no scalar knobs.
/// `Theme` is a section for the editor's benefit even though it has no
/// [`CATALOG`] rows — its slots are a free-form map the renderer owns, and
/// an editor reads them through [`SettingsSnapshot::value_at`].
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
        }
    }

    /// One-sentence summary of what the section configures.
    ///
    /// The wording tracks the section index of the generated config
    /// reference (`crates/phux/src/refdocs/config.rs`).
    #[must_use]
    pub const fn summary(self) -> &'static str {
        match self {
            Self::Defaults => {
                "Server-wide defaults: shell, TERM, scrollback depth, mouse tracking, \
                 spawn-time cwd policy, session naming, multi-view window sizing."
            }
            Self::Keybindings => {
                "Prefix chord, the prefix-table and global binding maps, and the \
                 which-key popup knobs."
            }
            Self::Status => {
                "Status-bar composition: widget lists for the left, center, and right \
                 slots, plus which outer-terminal row the bar reserves."
            }
            Self::Sidebar => {
                "The window sidebar: whether it shows, its width in columns, and the \
                 edge it docks to."
            }
            Self::Chrome => {
                "Responsive-chrome breakpoints: the column and row counts at which \
                 overlays go full-bleed and the sidebar yields its columns back to the \
                 panes."
            }
            Self::Theme => "Free-form color slots (slot = \"color\") consumed by the renderer.",
            Self::Experimental => {
                "Opt-in unstable knobs; anything here may change or disappear without \
                 notice."
            }
            Self::Voice => {
                "The server-side transcriber behind TRANSCRIBE: an argv that turns an \
                 uploaded clip into text for a paste."
            }
        }
    }
}

/// How a setting's value is shaped, for an editor choosing a control.
///
/// The `Optional*` kinds and [`Argv`](Self::Argv) may be *unset*: the key
/// is absent from every layer and the consumer applies its own documented
/// fallback. The other kinds always have a value, because the embedded
/// `default.toml` sets one.
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
    /// One of a fixed set of kebab-case variant names, spelled exactly as
    /// the schema's serde enum accepts them.
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
    /// A theme color string (named, `#rrggbb`, or an ANSI index). This
    /// crate does not validate colors — the renderer owns the color parser
    /// — so the kind is a label for editors, not a validation promise.
    Color,
}

impl SettingKind {
    /// Whether the setting may legitimately be unset.
    #[must_use]
    pub const fn is_optional(self) -> bool {
        matches!(
            self,
            Self::OptionalBool | Self::OptionalText | Self::OptionalInteger { .. } | Self::Argv
        )
    }

    /// The inclusive `(min, max)` bounds of an integer kind; `None` for
    /// every other kind.
    #[must_use]
    pub const fn bounds(self) -> Option<(i64, i64)> {
        match self {
            Self::Integer { min, max } | Self::OptionalInteger { min, max } => Some((min, max)),
            _ => None,
        }
    }
}

/// When a change to a setting takes effect.
///
/// Derived from `docs/consumers/tui.md` section 4.3 ("Reloading") and the
/// schema's field docs; each variant's doc names the paragraph relied on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Applies {
    /// Takes effect on the `reload-config` action or `phux config reload`.
    ///
    /// Section 4.3 lists what a reload rebuilds atomically: "keybindings
    /// (prefix, both tables, plugin-contributed chords, the which-key
    /// knobs), the theme, the status-bar composition (widgets, plugin
    /// `[[widgets]]` contributions, and `[status] position`)". The chrome
    /// breakpoints are not named there but are rebuilt on the same path
    /// (`crates/phux-tui/src/settings.rs` folds `cfg.chrome` into the
    /// reloaded `TuiSettings`), so `[chrome]` rides along.
    LiveReload,
    /// Read once when a client attaches; restart the client, or detach and
    /// re-attach, to pick it up.
    ///
    /// Section 4.3: "Not covered by a reload (restart the client, or detach
    /// and re-attach): pane-behavior settings read once at attach, such as
    /// `[predict]`, `[sidebar]` geometry". `defaults.mouse` is the client's
    /// outer-terminal mouse tracking "on attach" (ADR-0048), and
    /// `experimental.predictive-echo` engages "in `phux attach`", so both
    /// sit here too.
    NextAttach,
    /// Server-owned: applies to panes the server spawns after it has read
    /// the change.
    ///
    /// Section 4.3 excludes `[defaults]` from a reload, "which the server
    /// owns anyway". The server reads `[defaults]` and `[voice]` from its
    /// single config load at startup (`crates/phux/src/commands/server.rs`,
    /// `load_config`) and mirrors them into shared state, so a running
    /// server keeps the values it started with; the change reaches panes
    /// spawned by a server started after the edit.
    NextSpawn,
}

impl Applies {
    /// Short label for an editor's "takes effect" column.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::LiveReload => "on reload",
            Self::NextAttach => "next attach",
            Self::NextSpawn => "next server start",
        }
    }
}

/// One scalar setting of the schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingSpec {
    /// Dotted TOML path, e.g. `defaults.history-limit`. Every segment is a
    /// bare TOML key, so the path is also how provenance and `phux config
    /// check` spell it.
    pub key: &'static str,
    /// The section the key lives in.
    pub section: SettingSection,
    /// The value's shape.
    pub kind: SettingKind,
    /// One line, at most 72 characters, no trailing period.
    pub summary: &'static str,
    /// One to three sentences: what it does, the units, the gotcha. Sourced
    /// from the schema docs and `default.toml` comments.
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

/// `u16::MAX` as an `i64` bound, for the u16 fields.
const U16_MAX: i64 = u16::MAX as i64;
/// `u32::MAX` as an `i64` bound, for the u32 fields.
const U32_MAX: i64 = u32::MAX as i64;
/// [`MAX_HISTORY_BYTES`] as an `i64` bound.
const HISTORY_BYTES_MAX: i64 = MAX_HISTORY_BYTES as i64;
/// Cap on `keybindings.which-key-delay-ms`: one minute. See the row's
/// detail text.
const WHICH_KEY_DELAY_MAX_MS: i64 = 60_000;
/// Cap on `voice.timeout-secs`: one hour. See the row's detail text.
const VOICE_TIMEOUT_MAX_SECS: i64 = 3_600;

/// `defaults.cwd-inheritance` variants, as `CwdInheritance` serializes them.
const CWD_INHERITANCE: &[&str] = &[
    "inherit-focused",
    "home",
    "session-root",
    "last-cwd-per-window",
];
/// `defaults.window-size` variants, as `WindowSize` serializes them.
const WINDOW_SIZE: &[&str] = &["smallest", "largest", "latest", "manual"];
/// `status.position` variants, as `StatusPosition` serializes them.
const STATUS_POSITION: &[&str] = &["bottom", "top"];
/// `sidebar.position` variants, as `SidebarPosition` serializes them.
const SIDEBAR_POSITION: &[&str] = &["left", "right"];

/// Every scalar setting of the schema, in section order then schema field
/// order.
///
/// Pinned to the schema by `catalog_covers_every_scalar_leaf_of_the_schema`:
/// a scalar field added without a row here fails CI, and so does a row that
/// names no field.
pub const CATALOG: &[SettingSpec] = &[
    // -- [defaults] ---------------------------------------------------------
    SettingSpec {
        key: "defaults.shell",
        section: SettingSection::Defaults,
        kind: SettingKind::OptionalText,
        summary: "Shell for server-spawned panes; unset honors $SHELL",
        detail: "The program server-spawned panes run when nothing names a command: the \
                 seed session, attach-time session creation, and a SPAWN_TERMINAL whose \
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
        summary: "Bytes of scrollback retained per pane; costs attach latency",
        detail: "The bound that actually limits a pane's memory. Raising it buys depth and \
                 costs attach latency, because every retained page is re-encoded per pane \
                 when a client attaches, on the single server thread: roughly 8 ms at the \
                 2 MiB default, 65 ms at 10 MiB, 222 ms at 32 MiB. 67108864 (64 MiB) is \
                 the accepted maximum; phux config check rejects more.",
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
        detail: "Applies when a SPAWN_TERMINAL leaves cwd unset; an explicit cwd always \
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
                 at session-create time (a colon in it becomes an underscore so the name \
                 stays selector-safe); unknown placeholders pass through verbatim. Set \
                 \"default\" for a fixed name.",
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
        detail: "bottom (the default) or top. The bar's widget lists are composition, not \
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
];

/// The catalogue rows of one section, in schema field order.
pub fn catalog_for(section: SettingSection) -> impl Iterator<Item = &'static SettingSpec> {
    CATALOG.iter().filter(move |spec| spec.section == section)
}

/// Look up a catalogue row by its dotted key.
#[must_use]
pub fn find(key: &str) -> Option<&'static SettingSpec> {
    CATALOG.iter().find(|spec| spec.key == key)
}

/// One setting resolved against a [`SettingsSnapshot`].
#[derive(Debug, Clone, PartialEq)]
pub struct SettingEntry<'a> {
    /// The catalogue row.
    pub spec: &'a SettingSpec,
    /// The effective merged value; `None` when the key is unset in every
    /// layer (only possible for optional kinds).
    pub value: Option<toml::Value>,
    /// The shipped default: the embedded `default.toml` as the schema
    /// resolves it (see [`SettingsSnapshot`]); `None` when the default is
    /// unset.
    pub default: Option<toml::Value>,
    /// The layer that set the effective value. `None` means the value is
    /// the shipped default (or the key is unset everywhere).
    pub origin: Option<LayerSource>,
}

impl SettingEntry<'_> {
    /// Whether a layer above the embedded defaults set this key.
    #[must_use]
    pub const fn is_overridden(&self) -> bool {
        self.origin.is_some()
    }

    /// The effective value for display; see [`render_value`].
    #[must_use]
    pub fn render_value(&self) -> String {
        render_value(self.value.as_ref())
    }

    /// The shipped default for display; see [`render_value`].
    #[must_use]
    pub fn render_default(&self) -> String {
        render_value(self.default.as_ref())
    }
}

/// Render a value the way a settings page shows it.
///
/// Strings are bare (no quotes), integers and floats plain, booleans
/// `true` / `false`, an absent value `unset`, and an array shell-joined
/// words (each word single-quoted only when it needs to be). Tables fall
/// back to their inline TOML form.
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

/// Serialize a typed config back to a table, so every key the schema
/// defaults is present alongside what the layers set.
fn schema_table(config: crate::Config, path: &Path) -> Result<toml::Table, ConfigError> {
    let spanless = |message: String| ConfigError::Parse {
        path: path.to_path_buf(),
        position: None,
        message,
    };
    match toml::Value::try_from(config).map_err(|err| spanless(err.to_string()))? {
        toml::Value::Table(table) => Ok(table),
        other => Err(spanless(format!(
            "config serialized to a {} rather than a table",
            other.type_str()
        ))),
    }
}

/// Walk a dotted path of bare segments into `table`.
///
/// Every catalogue key and every theme slot is made of bare segments, so
/// quoted segments (`keybindings.prefix-table."%"`) are out of scope here.
fn value_at<'t>(table: &'t toml::Table, dotted: &str) -> Option<&'t toml::Value> {
    let mut segments = dotted.split('.');
    let mut current = table.get(segments.next()?)?;
    for segment in segments {
        current = current.as_table()?.get(segment)?;
    }
    Some(current)
}

/// The resolved layer stack of one config file, ready to answer "what is
/// this setting's effective value, what is its default, who set it".
///
/// Built once per page load rather than per setting: the merge and the
/// provenance fold happen in [`SettingsSnapshot::load`], and every
/// [`entry`](Self::entry) after that is a map lookup.
///
/// Both tables are the layer stack *as the schema resolves it*, not the
/// raw TOML merge. The embedded `default.toml` ships `[sidebar]`,
/// `[chrome]`, and `status.position` commented out and relies on the
/// schema's serde defaults for them, so the raw defaults layer would call
/// `sidebar.width` unset when its shipped default is 28. Round-tripping
/// through [`crate::Config`] puts every schema-defaulted key in place;
/// keys that are genuinely optional stay absent.
#[derive(Debug, Clone)]
pub struct SettingsSnapshot {
    /// The effective config, all layers applied and schema defaults filled.
    effective: toml::Table,
    /// The embedded defaults alone, schema defaults filled.
    defaults: toml::Table,
    /// Which layer set each leaf that some layer actually set.
    provenance: ConfigProvenance,
}

impl SettingsSnapshot {
    /// Resolve the user's config text at `path` with its full layer stack
    /// ([`crate::merged_config_with_provenance`]) and the embedded defaults
    /// beside it.
    ///
    /// `path` reports errors and anchors relative `extends` entries; layer
    /// files are read from disk, the user's own text is not.
    ///
    /// # Errors
    ///
    /// Whatever [`crate::parse_with_defaults`] returns: a parse failure in
    /// the user's text or a layer, a layer that cannot be read or is
    /// cyclic, or a merged document that does not fit the schema (an
    /// unknown key, a wrong type). A config the client would refuse to
    /// load has no effective values to show.
    pub fn load(user_input: &str, path: &Path) -> Result<Self, ConfigError> {
        let (merged, provenance) = crate::merged_config_with_provenance(user_input, path)?;
        let effective = schema_table(crate::deserialize_merged(merged, user_input, path)?, path)?;
        // An empty overlay resolves to the defaults alone and declares no
        // `extends`, so this touches no file.
        let defaults = schema_table(crate::parse_with_defaults("", path)?, path)?;
        Ok(Self {
            effective,
            defaults,
            provenance,
        })
    }

    /// The layer stack in merge order: `Defaults` first, the user's file
    /// last, `Extended` layers in between.
    #[must_use]
    pub fn layers(&self) -> &[LayerSource] {
        &self.provenance.layers
    }

    /// Resolve one catalogue row.
    #[must_use]
    pub fn entry<'a>(&self, spec: &'a SettingSpec) -> SettingEntry<'a> {
        SettingEntry {
            spec,
            value: self.value_at(spec.key).cloned(),
            default: self.default_at(spec.key).cloned(),
            origin: self.origin_at(spec.key).cloned(),
        }
    }

    /// The effective merged value at a dotted key of bare segments, or
    /// `None` when unset. Works for keys outside the catalogue too, which
    /// is how an editor reads `theme.<slot>`.
    #[must_use]
    pub fn value_at(&self, dotted_key: &str) -> Option<&toml::Value> {
        value_at(&self.effective, dotted_key)
    }

    /// The layer above the embedded defaults that set `dotted_key`.
    ///
    /// `None` when the effective value is the shipped default or the key is
    /// unset everywhere — the two cases an editor shows as "not
    /// overridden".
    #[must_use]
    pub fn origin_at(&self, dotted_key: &str) -> Option<&LayerSource> {
        let origin = self.provenance.keys.get(dotted_key)?;
        self.provenance
            .layers
            .get(origin.layer)
            .filter(|layer| **layer != LayerSource::Defaults)
    }

    /// The shipped default at a dotted key of bare segments, or `None` when
    /// the embedded `default.toml` leaves it unset.
    #[must_use]
    pub fn default_at(&self, dotted_key: &str) -> Option<&toml::Value> {
        value_at(&self.defaults, dotted_key)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use super::*;

    /// Catalogue keys whose shipped state is unset: they never appear in
    /// the serialized defaults, so the coverage walk cannot find them.
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

    fn schema_defaults_table() -> toml::Table {
        let cfg = crate::parse_with_defaults("", Path::new("test.toml")).expect("defaults parse");
        toml::Value::try_from(cfg)
            .expect("Config serializes")
            .as_table()
            .cloned()
            .expect("Config is a table")
    }

    fn scalar_leaves(value: &toml::Value, path: &str, out: &mut BTreeSet<String>) {
        if COMPOSITE_KEYS.contains(&path) {
            return;
        }
        match value {
            toml::Value::Table(table) => {
                for (key, child) in table {
                    let child_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    scalar_leaves(child, &child_path, out);
                }
            }
            toml::Value::Array(_) => {}
            _ => {
                out.insert(path.to_owned());
            }
        }
    }

    /// THE coverage gate, both directions: every scalar leaf under the
    /// section tables has exactly one row, and every row names such a leaf
    /// or one of the documented unset keys.
    #[test]
    fn catalog_covers_every_scalar_leaf_of_the_schema() {
        let defaults = schema_defaults_table();
        let mut leaves = BTreeSet::new();
        for section in SettingSection::ALL {
            if let Some(table) = defaults.get(section.table()) {
                scalar_leaves(table, section.table(), &mut leaves);
            }
        }
        let mut expected = leaves;
        for key in OPTIONAL_KEYS {
            assert!(
                !expected.contains(*key),
                "{key} is listed as unset but the schema now serializes a default for it"
            );
            expected.insert((*key).to_owned());
        }

        let mut catalog_keys = BTreeSet::new();
        for spec in CATALOG {
            assert!(
                catalog_keys.insert(spec.key.to_owned()),
                "duplicate CATALOG row for {}",
                spec.key
            );
        }
        assert_eq!(
            catalog_keys, expected,
            "CATALOG drifted from the schema's scalar leaves; add or remove rows in \
             crates/phux-config/src/settings/mod.rs"
        );
    }

    #[test]
    fn optional_keys_have_optional_kinds_and_nothing_else_does() {
        for spec in CATALOG {
            let listed = OPTIONAL_KEYS.contains(&spec.key);
            assert_eq!(
                spec.kind.is_optional(),
                listed,
                "{}: kind {:?} disagrees with the OPTIONAL_KEYS list",
                spec.key,
                spec.kind
            );
        }
    }

    #[test]
    fn catalog_is_in_section_order_and_every_section_table_matches() {
        let mut last_section = 0;
        for spec in CATALOG {
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
            assert!(!spec.leaf().is_empty());
            assert!(find(spec.key).is_some());
        }
        assert_eq!(catalog_for(SettingSection::Theme).count(), 0);
        assert_eq!(catalog_for(SettingSection::Chrome).count(), 3);
    }

    #[test]
    fn summaries_are_one_short_line_and_details_are_prose() {
        for spec in CATALOG {
            assert!(
                spec.summary.chars().count() <= 72,
                "{}: summary is {} chars",
                spec.key,
                spec.summary.chars().count()
            );
            assert!(
                !spec.summary.ends_with('.'),
                "{}: trailing period",
                spec.key
            );
            assert!(
                !spec.summary.contains('\n'),
                "{}: multi-line summary",
                spec.key
            );
            assert!(
                spec.detail.ends_with('.'),
                "{}: detail is not a sentence",
                spec.key
            );
        }
    }

    /// Every `Choice` variant is one the schema deserializes at that key,
    /// and a bogus variant is rejected.
    #[test]
    fn choice_variants_match_the_schema_enums() {
        let mut seen = 0;
        for spec in CATALOG {
            let SettingKind::Choice(variants) = spec.kind else {
                continue;
            };
            seen += 1;
            for variant in variants {
                let text = format!("[{}]\n{} = \"{variant}\"\n", spec.table(), spec.leaf());
                crate::parse_with_defaults(&text, Path::new("t.toml"))
                    .unwrap_or_else(|err| panic!("{}: `{variant}` rejected: {err}", spec.key));
            }
            let bogus = format!(
                "[{}]\n{} = \"not-a-real-variant\"\n",
                spec.table(),
                spec.leaf()
            );
            assert!(
                crate::parse_with_defaults(&bogus, Path::new("t.toml")).is_err(),
                "{}: bogus variant accepted",
                spec.key
            );
        }
        assert_eq!(seen, 4, "expected the four schema enums to be Choice rows");
    }

    #[test]
    fn every_integer_default_lies_within_its_bounds() {
        let snapshot = SettingsSnapshot::load("", Path::new("t.toml")).expect("load");
        for spec in CATALOG {
            let SettingKind::Integer { min, max } = spec.kind else {
                continue;
            };
            assert!(min <= max, "{}: inverted bounds", spec.key);
            let default = snapshot
                .default_at(spec.key)
                .and_then(toml::Value::as_integer)
                .unwrap_or_else(|| panic!("{}: no integer default", spec.key));
            assert!(
                (min..=max).contains(&default),
                "{}: default {default} outside [{min}, {max}]",
                spec.key
            );
        }
        let (min, max) = find("defaults.history-bytes")
            .unwrap()
            .kind
            .bounds()
            .unwrap();
        assert_eq!((min, max), (0, i64::from(MAX_HISTORY_BYTES)));
    }

    #[test]
    fn empty_user_file_yields_defaults_with_no_origin() {
        let snapshot = SettingsSnapshot::load("", Path::new("t.toml")).expect("load");
        assert_eq!(snapshot.layers().len(), 2);
        for spec in CATALOG {
            let entry = snapshot.entry(spec);
            assert!(entry.origin.is_none(), "{}: {:?}", spec.key, entry.origin);
            assert!(!entry.is_overridden());
            assert_eq!(entry.value, entry.default, "{}", spec.key);
            if spec.kind.is_optional() {
                assert_eq!(entry.render_value(), "unset", "{}", spec.key);
            }
        }
    }

    #[test]
    fn a_user_override_is_attributed_to_the_user_layer() {
        let path = PathBuf::from("/nonexistent/config.toml");
        let snapshot = SettingsSnapshot::load("[sidebar]\nwidth = 32\n", &path).expect("load");
        let spec = find("sidebar.width").unwrap();
        let entry = snapshot.entry(spec);
        assert_eq!(entry.value, Some(toml::Value::Integer(32)));
        assert_eq!(
            entry.default,
            Some(toml::Value::Integer(0)),
            "the shipped default: 0 selects automatic sizing"
        );
        assert_eq!(entry.origin, Some(LayerSource::User(path)));
        assert!(entry.is_overridden());
        assert_eq!(entry.render_value(), "32");
        assert_eq!(entry.render_default(), "0");
        // Untouched siblings stay attributed to the defaults.
        assert!(snapshot.origin_at("sidebar.enabled").is_none());
    }

    #[test]
    fn an_extends_layer_is_attributed_to_the_extended_layer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layer_path = dir.path().join("layers").join("team.toml");
        std::fs::create_dir_all(layer_path.parent().unwrap()).unwrap();
        std::fs::write(&layer_path, "[chrome]\ncompact-cols = 80\n").unwrap();
        let user_path = dir.path().join("config.toml");
        let user = "extends = [\"team\"]\n\n[sidebar]\nwidth = 40\n";
        std::fs::write(&user_path, user).unwrap();

        let snapshot = SettingsSnapshot::load(user, &user_path).expect("load");
        assert_eq!(snapshot.layers().len(), 3);
        assert_eq!(
            snapshot.origin_at("chrome.compact-cols"),
            Some(&LayerSource::Extended(layer_path))
        );
        assert_eq!(
            snapshot.value_at("chrome.compact-cols"),
            Some(&toml::Value::Integer(80))
        );
        assert_eq!(
            snapshot.origin_at("sidebar.width"),
            Some(&LayerSource::User(user_path))
        );
    }

    #[test]
    fn theme_slots_are_readable_through_value_at() {
        let snapshot =
            SettingsSnapshot::load("[theme]\naccent = \"#ff0000\"\n", Path::new("t.toml"))
                .expect("load");
        assert_eq!(
            snapshot.value_at("theme.accent"),
            Some(&toml::Value::String("#ff0000".to_owned()))
        );
        assert!(snapshot.default_at("theme.accent").is_none());
        assert!(snapshot.origin_at("theme.accent").is_some());
        assert!(snapshot.value_at("theme.nonexistent").is_none());
        assert!(snapshot.value_at("").is_none());
    }

    #[test]
    fn render_value_shapes() {
        assert_eq!(render_value(None), "unset");
        assert_eq!(
            render_value(Some(&toml::Value::String("xterm-256color".to_owned()))),
            "xterm-256color"
        );
        assert_eq!(render_value(Some(&toml::Value::Boolean(false))), "false");
        assert_eq!(render_value(Some(&toml::Value::Integer(400))), "400");
        let argv = toml::Value::Array(
            ["curl", "-sf", "-F", "file=@{path}", "a b", "it's"]
                .into_iter()
                .map(|s| toml::Value::String(s.to_owned()))
                .collect(),
        );
        assert_eq!(
            render_value(Some(&argv)),
            "curl -sf -F file=@{path} 'a b' 'it'\\''s'"
        );
    }

    #[test]
    fn section_metadata_is_consistent() {
        assert_eq!(SettingSection::ALL.len(), 8);
        for section in SettingSection::ALL {
            assert!(!section.title().is_empty());
            assert!(section.summary().ends_with('.'));
            assert!(section.table().chars().all(|c| c.is_ascii_lowercase()));
        }
        assert_eq!(SettingSection::Status.title(), "Status bar");
        assert_eq!(Applies::LiveReload.label(), "on reload");
    }
}
