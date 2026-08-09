//! Typed schema for `config.toml`.
//!
//! Field and section names track `docs/consumers/tui.md` §4 verbatim. The TOML side
//! uses kebab-case (`history-limit`); the Rust side uses `snake_case`
//! and `#[serde(rename = ...)]` bridges the two.
//!
//! `Eq` is intentionally not derived: several structs carry
//! `toml::Value` (which is not `Eq` because of `f64`).

#![allow(clippy::derive_partial_eq_without_eq)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    connector::ConnectorConfigEntry, plugin::PluginConfigEntry, remote::RemoteConfigEntry,
    satellite::SatelliteConfigEntry,
};

/// Top-level config. See `docs/consumers/tui.md` §4.2.
///
/// Sections are all optional; an empty config file parses to
/// [`Config::default`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Server-wide defaults — shell, `TERM`, scrollback, spawn policy.
    #[serde(default)]
    pub defaults: DefaultsCfg,

    /// Prefix + prefix-table + global keybindings.
    #[serde(default)]
    pub keybindings: KeybindingsCfg,

    /// Status-bar slot composition.
    #[serde(default)]
    pub status: StatusCfg,

    /// Window sidebar (`[sidebar]`). Off by default.
    #[serde(default)]
    pub sidebar: SidebarCfg,

    /// Event hooks (`[[hooks.<name>]]`).
    ///
    /// Keyed by hook name (e.g. `pane-exit`, `after-new-pane`); each
    /// entry is the array-of-tables under that name.
    #[serde(default)]
    pub hooks: BTreeMap<String, Vec<HookEntry>>,

    /// Declarative plugin manifests composed into this config.
    #[serde(default)]
    pub plugins: Vec<PluginConfigEntry>,

    /// Hub-and-spoke federation satellites declared for this host.
    #[serde(default)]
    pub satellites: Vec<SatelliteConfigEntry>,

    /// Outbound relay links this server supervises (ADR-0052).
    #[serde(default)]
    pub connector: Vec<ConnectorConfigEntry>,

    /// Remote phux servers this machine attaches to (ADR-0055). Written by
    /// `phux host enroll` and `phux host add`; read by `phux attach <name>`.
    #[serde(default)]
    pub remote: Vec<RemoteConfigEntry>,

    /// Color slots (theme). Free-form key/value of color strings.
    #[serde(default)]
    pub theme: ThemeCfg,

    /// Experimental knobs gated behind `[experimental]`.
    ///
    /// Everything under this section is subject to change without notice.
    /// See `docs/consumers/tui.md` §4.2 for the user-facing caveat.
    #[serde(default)]
    pub experimental: ExperimentalCfg,
}

// ---------------------------------------------------------------------------
// [defaults]
// ---------------------------------------------------------------------------

/// `[defaults]` table. See `docs/consumers/tui.md` §12 for shipped values.
///
/// This struct is intentionally NOT `#[non_exhaustive]`: it is constructed
/// only via `Default` + struct-update syntax (`..DefaultsCfg::default()`)
/// in tests and via serde everywhere else, so adding fields is
/// source-compatible for all in-tree consumers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DefaultsCfg {
    /// Shell to spawn in new panes. `None` ⇒ honor `$SHELL` (fallback
    /// `/bin/sh`) at runtime.
    ///
    /// Consumed by the server at startup (phux-i0e8.4.1): the binary
    /// resolves `configured → $SHELL → /bin/sh` once from its single
    /// config load and threads the result into every server-owned spawn
    /// path — the pre-seeded session, `--seed-command`,
    /// attach-time `CreateIfMissing`, `SESSION_CREATE_KEY`, and a
    /// `SPAWN_TERMINAL` whose wire frame carries no `command`. A wire
    /// `command` always wins over this default, mirroring the
    /// `defaults.term` precedent.
    #[serde(default)]
    pub shell: Option<String>,

    /// `TERM` advertised to the inner program of every server-spawned pane
    /// (the seed session, attach-time `CreateIfMissing`, and a
    /// `SPAWN_TERMINAL` whose wire `env` does not itself carry `TERM`).
    ///
    /// Default: `xterm-256color`. The baseline is the
    /// universally-recognised safe value — 256 colours and the standard
    /// xterm key vocabulary, no kitty-keyboard advertisement (phux-7vx /
    /// phux-ign). Set explicitly to e.g. `"ghostty"` to opt into ghostty's
    /// extended terminfo (sixel, kitty-graphics advertisement, the ghostty
    /// SGR extensions) once the host's apps are known to round-trip the
    /// kitty keyboard protocol. The phux-0o8 harness
    /// (`crates/phux-server/tests/kip_roundtrip.rs`) records the evidence
    /// per app: nvim's CSI-u opt-in round-trips end-to-end and
    /// fzf/less/vim/btop are regression-free under `TERM=ghostty`, but
    /// htop — the phux-7vx regression app — is unproven, so the shipped
    /// default stays conservative.
    ///
    /// A per-spawn `SPAWN_TERMINAL.env` entry for `TERM` always wins over
    /// this default — the wire frame is authoritative for the Terminal it
    /// creates; this is only the fallback when the frame is silent.
    #[serde(default = "default_term")]
    pub term: String,

    /// Lines of scrollback retained per pane.
    ///
    /// The TOML key is `history-limit` — the tmux-shaped name kept since
    /// `phux-config` first shipped. `scrollback-lines` was proposed in
    /// phux-4li.1 but consciously folded into this field rather than
    /// duplicated: they describe the same per-Terminal scrollback cap.
    #[serde(default = "default_history_limit", rename = "history-limit")]
    pub history_limit: u32,

    /// Whether the client enables its own outer-terminal mouse tracking
    /// on attach (ADR-0048). `true` (default) emits DECSET
    /// `?1002h?1006h` so divider drag-to-resize and click-to-focus work
    /// without an inner program turning mouse mode on, and restores the
    /// host terminal's mouse state on detach. `false` is the
    /// pass-through-only escape hatch: no DECSET, the host's native
    /// click-drag selection is left untouched.
    #[serde(default = "default_true")]
    pub mouse: bool,

    /// How a freshly-spawned pane chooses its working directory.
    ///
    /// Default: [`CwdInheritance::InheritFocused`], matching tmux. See
    /// the enum docs for the full set.
    ///
    /// Wired server-side in `phux-server` (phux-cs6): `SPAWN_TERMINAL`
    /// reads this policy when the wire frame leaves `cwd` unset.
    /// `inherit-focused` resolves the focused pane's live PTY working
    /// directory via a kernel query on the PTY child; `home` uses
    /// `$HOME`. `session-root` and `last-cwd-per-window` are accepted but
    /// not yet resolved server-side (follow-ups).
    #[serde(default, rename = "cwd-inheritance")]
    pub cwd_inheritance: CwdInheritance,

    /// Command to spawn when `phux` auto-creates a session on attach.
    ///
    /// `None` (default) ⇒ honor [`DefaultsCfg::shell`] (which in turn
    /// honors `$SHELL`). Set explicitly to launch e.g. a TUI dashboard
    /// or a specific REPL as the initial program.
    #[serde(default, rename = "spawn-on-attach")]
    pub spawn_on_attach: Option<String>,

    /// Naming template for auto-created sessions.
    ///
    /// Default: `"${cwd-basename}"` — the basename of the client's
    /// working directory, resolved at session-creation time. A bench of
    /// project checkouts then reads as its directories in the session
    /// picker, which is the only thing that makes the picker useful with
    /// more than one session open. Set `"default"` for a fixed name.
    /// Other placeholders may be added later; unknown placeholders are
    /// passed through verbatim.
    #[serde(
        default = "default_session_name_template",
        rename = "session-name-template"
    )]
    pub session_name_template: String,

    /// Policy for choosing one geometry when concurrent views of a single
    /// Terminal disagree on size.
    ///
    /// A Terminal is one PTY + one libghostty grid, so it has exactly one
    /// authoritative `(cols, rows)`; concurrent views (mirrored panes,
    /// multiple attached clients) share it. When they disagree, this key
    /// picks which size wins; a view larger than the chosen size
    /// letterboxes rather than reflowing the shared grid. The vocabulary
    /// mirrors tmux's `window-size` option.
    ///
    /// Default: [`WindowSize::Smallest`] — nothing is ever cropped. See
    /// [ADR-0027](../../ADR/0027-terminal-references-and-l3-links.md) and
    /// [`WindowSize`].
    ///
    /// Consumed at the size-decision point: the server resolves a
    /// Terminal's geometry by folding this policy across every subscriber's
    /// viewport, on ATTACH and on every `VIEWPORT_RESIZE` (phux-nk07).
    ///
    /// It governs *views* only. An explicit `TERMINAL_RESIZE` — the frame
    /// `phux resize` sends — names a size no viewport reported and applies
    /// regardless; under the three view-derived values the next view event
    /// recomputes over it, and under [`WindowSize::Manual`] nothing ever
    /// does. See ADR-0062.
    #[serde(default, rename = "window-size")]
    pub window_size: WindowSize,
}

impl Default for DefaultsCfg {
    fn default() -> Self {
        Self {
            shell: None,
            term: default_term(),
            history_limit: default_history_limit(),
            mouse: true,
            cwd_inheritance: CwdInheritance::default(),
            spawn_on_attach: None,
            session_name_template: default_session_name_template(),
            window_size: WindowSize::default(),
        }
    }
}

/// Default `TERM` for server-spawned panes: the safe xterm baseline
/// (phux-7vx / phux-ign). See [`DefaultsCfg::term`].
fn default_term() -> String {
    "xterm-256color".to_owned()
}
const fn default_history_limit() -> u32 {
    50_000
}
const fn default_true() -> bool {
    true
}
fn default_session_name_template() -> String {
    "${cwd-basename}".to_owned()
}

/// How a newly-spawned pane chooses its working directory.
///
/// Selected by the `defaults.cwd-inheritance` TOML key. Values use
/// kebab-case on the wire and `PascalCase` in Rust.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum CwdInheritance {
    /// Inherit the focused pane's current working directory. Default —
    /// matches tmux's default split behavior. Resolved server-side from
    /// the focused pane's live PTY working directory via a kernel query;
    /// see [`DefaultsCfg::cwd_inheritance`].
    #[default]
    InheritFocused,
    /// Always spawn in `$HOME`.
    Home,
    /// Spawn in the directory the session was created in.
    SessionRoot,
    /// Remember the last CWD per window and reuse it for new panes in
    /// that window.
    LastCwdPerWindow,
}

/// Policy for picking one Terminal geometry when concurrent views
/// disagree on size (ADR-0027).
///
/// Selected by the `defaults.window-size` TOML key. Values use kebab-case
/// on the wire and `PascalCase` in Rust. The vocabulary tracks tmux's
/// `window-size` option, since the one-PTY-one-grid constraint is the same
/// one tmux faces: a Terminal cannot render two sizes at once, so a view
/// that wants a different size letterboxes (larger) or clamps (smaller)
/// rather than reflowing the shared grid.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum WindowSize {
    /// Use the smallest view's size. Default — nothing is ever cropped;
    /// larger views letterbox. Matches tmux's default.
    #[default]
    Smallest,
    /// Use the largest view's size. Smaller views clamp (the grid may
    /// exceed their viewport, so content can be cut off).
    Largest,
    /// Track the most recently resized view's size.
    Latest,
    /// Hold a fixed size, ignoring view geometry. The size is set by an
    /// explicit `TERMINAL_RESIZE` (`phux resize TARGET COLSxROWS`), which
    /// is the only thing that moves a Terminal's grid under this value —
    /// attaches, detaches, and window resizes never recompute over it.
    /// This is the setting under which a scripted geometry holds
    /// (ADR-0027 named the value; ADR-0062 shipped the verb).
    Manual,
}

// ---------------------------------------------------------------------------
// [keybindings]
// ---------------------------------------------------------------------------

/// `[keybindings]` table: prefix key, prefix-table, and global table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct KeybindingsCfg {
    /// Prefix key chord (e.g. `"C-a"`). Defaults to `C-a` — chosen for
    /// portability across host terminals (some emulators silently
    /// swallow `C-Space` before it reaches the client). Chord syntax
    /// per `crate::keybind`: modifier letters (`C` / `M` / `A` / `S`)
    /// joined to the key by `-`.
    #[serde(default = "default_prefix")]
    pub prefix: String,

    /// Bindings that fire after the prefix.
    #[serde(default, rename = "prefix-table")]
    pub prefix_table: BTreeMap<String, Action>,

    /// Bindings that fire any time (typically `super`/`hyper` chords).
    #[serde(default)]
    pub global: BTreeMap<String, Action>,

    /// Show the which-key popup: after the prefix is pressed, if no
    /// continuation chord arrives within [`Self::which_key_delay_ms`],
    /// the client pops a panel listing the available prefix-table
    /// bindings. Any key dismisses it and executes normally; Esc
    /// cancels the prefix. Default `true`.
    #[serde(default = "default_true", rename = "which-key")]
    pub which_key: bool,

    /// Milliseconds to wait after the prefix before showing the
    /// which-key popup. Default `600`. A fast continuation chord always
    /// suppresses the popup entirely.
    #[serde(default = "default_which_key_delay_ms", rename = "which-key-delay-ms")]
    pub which_key_delay_ms: u64,
}

impl Default for KeybindingsCfg {
    fn default() -> Self {
        Self {
            prefix: default_prefix(),
            prefix_table: BTreeMap::new(),
            global: BTreeMap::new(),
            which_key: true,
            which_key_delay_ms: default_which_key_delay_ms(),
        }
    }
}

fn default_prefix() -> String {
    "C-a".to_owned()
}

/// Serde default for [`KeybindingsCfg::which_key_delay_ms`].
///
/// Deliberately snappier than tmux-ish 600: the popup is phux's primary
/// discovery surface, so it should feel like a hint that arrives while
/// you hesitate, not a timeout you wait out.
const fn default_which_key_delay_ms() -> u64 {
    400
}

/// An action attached to a binding, hook, or status slot.
///
/// Per `docs/consumers/tui.md` §4.2, this is either a bare string (no parameters,
/// e.g. `"kill-pane"`) or an inline table whose `action` field names
/// the action and whose remaining fields supply parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Action {
    /// `"detach"` — bare action name, no parameters.
    Bare(String),
    /// `{ action = "new-pane", direction = "vertical" }` — parameterized.
    Parameterized(ParamAction),
}

/// Parameterized action: `action` plus arbitrary `kind`-specific args.
///
/// `args` collects every remaining key in the inline table. This mirrors
/// the design's "we don't centrally enumerate action parameters in the
/// loader" stance — schema validation per action lives in the
/// dispatcher, not here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParamAction {
    /// The action name (e.g. `new-pane`, `run`, `focus-pane`).
    ///
    /// `docs/consumers/tui.md` uses both `action = "..."` (in keybindings, §4.2)
    /// and `kind = "..."` (in hooks, §9) to name the action. We accept
    /// either spelling on input and canonicalize to `action` on output.
    #[serde(alias = "kind")]
    pub action: String,
    /// Remaining inline-table fields, passed through as TOML values.
    #[serde(flatten)]
    pub args: BTreeMap<String, toml::Value>,
}

// ---------------------------------------------------------------------------
// [status]
// ---------------------------------------------------------------------------

/// `[status]` table: three slots, each a list of widgets, plus the row
/// the bar reserves (`position`, phux-foz.8).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StatusCfg {
    /// Left slot. Default: empty (callers may substitute defaults).
    #[serde(default)]
    pub left: Vec<Widget>,
    /// Center slot.
    #[serde(default)]
    pub center: Vec<Widget>,
    /// Right slot.
    #[serde(default)]
    pub right: Vec<Widget>,
    /// Which outer-terminal row the bar reserves. Default `bottom`
    /// (per `docs/consumers/tui.md` section 8).
    #[serde(default)]
    pub position: StatusPosition,
}

/// Which row the [`StatusCfg`] bar occupies (phux-foz.8).
///
/// Consumed by the TUI's chrome layer, which maps it onto its own
/// `Position` render enum; kept as plain config data here (ADR-0020:
/// phux-config carries no render types).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum StatusPosition {
    /// One row at the very bottom of the outer terminal (default).
    #[default]
    Bottom,
    /// One row at the very top of the outer terminal.
    Top,
}

/// `[sidebar]` — the Warp-style window sidebar (phux-4h5a).
///
/// A vertical strip listing the session's windows as tabs, each labelled by
/// its OSC title (falling back to the window name), the focused one
/// highlighted. Off by default; when `enabled`, it reserves `width` columns
/// on `position`, and the panes tile into the remaining area.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SidebarCfg {
    /// Show the sidebar. Default `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Width in columns when shown. Default `20`.
    #[serde(default = "default_sidebar_width")]
    pub width: u16,
    /// Which edge the sidebar docks to. Default `left`.
    #[serde(default)]
    pub position: SidebarPosition,
}

impl Default for SidebarCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            width: default_sidebar_width(),
            position: SidebarPosition::default(),
        }
    }
}

const fn default_sidebar_width() -> u16 {
    20
}

/// Which edge the [`SidebarCfg`] docks to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SidebarPosition {
    /// Dock on the left (default).
    #[default]
    Left,
    /// Dock on the right.
    Right,
}

/// A status-bar widget.
///
/// Per `docs/consumers/tui.md` §8.1, this is either a bare string (`"session"` is
/// shorthand for `{ kind = "session" }`) or an inline table with `kind`
/// plus widget-specific options.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Widget {
    /// `"session"` — shorthand for `{ kind = "session" }`.
    Bare(String),
    /// Full widget spec: `kind` plus arbitrary options.
    Spec(WidgetSpec),
}

/// Long-form widget spec.
///
/// `opts` carries every field except `kind` so that future widget
/// parameters don't require schema churn here; the renderer in
/// `phux-client` validates per-`kind`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WidgetSpec {
    /// Widget kind (`clock`, `session`, `exec`, ...). See
    /// `docs/reference/widgets.md` for the built-in catalog.
    pub kind: String,
    /// Remaining inline-table fields.
    #[serde(flatten)]
    pub opts: BTreeMap<String, toml::Value>,
}

// ---------------------------------------------------------------------------
// [[hooks.<name>]]
// ---------------------------------------------------------------------------

/// One entry under `[[hooks.<name>]]`: a `when` predicate plus an
/// `action` to run on match. See `docs/consumers/tui.md` §9.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HookEntry {
    /// Match clauses (`exit-code = 0`, `cwd-startswith = "..."`, etc.).
    /// First-match-wins per hook event.
    #[serde(default)]
    pub when: BTreeMap<String, toml::Value>,
    /// Action to fire on match — same shape as a keybind action.
    pub action: Action,
}

// ---------------------------------------------------------------------------
// [experimental]
// ---------------------------------------------------------------------------

/// `[experimental]` table — opt-in flags for unstable features.
///
/// Anything here may be renamed, repurposed, or removed without a
/// `SemVer` bump. Set explicitly only if you accept that contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ExperimentalCfg {
    /// Engage Mosh-class predictive local echo in `phux attach`.
    ///
    /// When `true`, the attach loop dispatches to
    /// `phux_client::attach::run_with_predict_dial` with a
    /// `PredictiveConfig { enabled: true, .. }`. See `phux-9gw.1` for
    /// the algorithm and `crates/phux-client/src/predict/` for the
    /// implementation.
    ///
    /// Default `false` (phux-pxaj, re-evaluated phux-51n6.1): predictive
    /// echo is a shell-prompt latency win and inert in full-screen app mode.
    /// The client now gates it proactively on the alternate screen — a pane
    /// running vim/nvim, a pager, or an agent TUI predicts nothing, since a
    /// keystroke there is a command the shell never echoes (see
    /// `phux_client::attach`'s `terminal_in_alt_screen`). Two real cases the
    /// client still cannot gate keep the *default* off, though the opt-in is
    /// safe to use:
    ///
    /// 1. readline vi command-mode at the prompt (`set -o vi`) is on the main
    ///    screen with no DEC mode bit to detect it, so normal-mode keys are
    ///    mispredicted as inserts until the reactive auto-back-off suspends
    ///    (a brief underlined flicker, then quiet);
    /// 2. no-echo prompts (`sudo`/`ssh` password) suppress echo via the PTY's
    ///    termios, which the client mirror never sees — so a predicted insert
    ///    would momentarily render the typed characters locally.
    ///
    /// Making on-by-default safe needs the mosh mechanisms not yet ported: an
    /// RTT-adaptive gate (predict only when the round trip is worth hiding —
    /// over local UDS it is not) and a prediction display-timeout that expires
    /// unconfirmed ghosts. Until then it is opt-in.
    ///
    /// Set `true` to engage Mosh-class local echo (the predicted classes are
    /// the conservative mosh-proven subset; a wrong guess is stomped by the
    /// next authoritative frame, so the failure mode is a brief underlined
    /// flicker in exchange for typing that doesn't wait a round trip per key).
    #[serde(default = "default_predictive_echo", rename = "predictive-echo")]
    pub predictive_echo: bool,
}

impl Default for ExperimentalCfg {
    fn default() -> Self {
        Self {
            predictive_echo: default_predictive_echo(),
        }
    }
}

/// Serde default for [`ExperimentalCfg::predictive_echo`].
const fn default_predictive_echo() -> bool {
    false
}

// ---------------------------------------------------------------------------
// [theme]
// ---------------------------------------------------------------------------

/// `[theme]` table. Free-form `slot -> color-string` map; the renderer
/// owns interpretation. We deliberately do not type-check colors here.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct ThemeCfg {
    /// Slot → color string (e.g. `"fg" -> "#cdd6f4"`).
    pub slots: BTreeMap<String, String>,
}
