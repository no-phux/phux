//! Status-bar widget trait + registry + in-tree widget implementations.
//!
//! Owned by `phux-nz4.4`. Note that [`crate::Widget`] is the *schema* enum
//! (a parsed `[[status.widgets]]` entry from TOML); the runtime trait here
//! is called [`StatusWidget`] to avoid the name collision.
//!
//! ## Why a local `Cell` here
//!
//! The status bar is a *frontend* concern: phux-config composes cells, the
//! TUI client lays them out, the wire never sees them. Under ADR-0013 the
//! protocol crate no longer exposes a `Cell` type (pane content moved to
//! VT bytes on the wire); the status bar accordingly carries its own
//! lightweight cell shape — just a grapheme cluster — and renders it via
//! the same SGR emission code path the renderer uses for live panes. If a
//! richer widget style surface ever lands (foreground color, bold flag,
//! etc.), grow this struct here — the wire doesn't care.

use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, SystemTime};

use smallvec::SmallVec;

use crate::schema::WidgetSpec;
use crate::vocab;

mod status_bar;
mod widgets;

pub use status_bar::{StatusBar, row_to_string};
pub use widgets::cwd::CwdWidget;
pub use widgets::exec::{ExecFeed, ExecWidget};
pub use widgets::exit_status::ExitWidget;
pub use widgets::help_hints::HelpHintsWidget;
pub use widgets::session_name::SessionNameWidget;
pub use widgets::time::TimeWidget;
pub use widgets::windows::WindowsWidget;

/// Visual style for a status-bar [`Cell`], expressed as plain data.
///
/// Colors are strings (`"red"`, `"#cdd6f4"`, `"12"`) interpreted by the
/// render layer — phux-config never imports ratatui (ADR-0020), so the
/// translation to `ratatui::style::Color` happens in
/// `phux-client`'s chrome module. This mirrors the existing
/// `[theme].slots` color-as-string convention.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent SGR attribute toggle; a bitflags enum would only obscure the TOML-facing shape"
)]
pub struct CellStyle {
    /// Foreground color string, or `None` for the terminal default.
    pub fg: Option<String>,
    /// Background color string, or `None` for the terminal default.
    pub bg: Option<String>,
    /// Bold.
    pub bold: bool,
    /// Dim / faint.
    pub dim: bool,
    /// Italic.
    pub italic: bool,
    /// Underline.
    pub underline: bool,
    /// Reverse video (swap fg/bg).
    pub reverse: bool,
}

impl CellStyle {
    /// `true` when every field is at its default (no styling). Used to
    /// store `None` rather than an all-default style on a [`Cell`].
    #[must_use]
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }
}

/// phux-foz.12: the interactive target a composed cell carries.
///
/// Lets a host that routes mouse input (the TUI client) hit-test a click
/// against the exact strip it painted. Widgets stamp this on their cells
/// at render time; the composer copies it through slot placement and
/// truncation untouched. Paint and hit targets therefore derive from one
/// model and cannot drift — the same discipline as the sidebar's
/// `row_model`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellHit {
    /// Clicking this cell selects window `i` (the `select-window` index).
    /// Stamped by the `windows` widget on tab cells (separators excluded).
    Window(usize),
}

/// A single status-bar cell.
///
/// Local to phux-config — see the module-level doc for rationale. Grapheme
/// storage matches the inline-then-spill pattern phux-protocol once used
/// for its (now-deleted) `Cell::text` field.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cell {
    /// Grapheme cluster occupying this cell. May be empty for a blank cell.
    /// First element is the base codepoint; remaining elements are combining
    /// codepoints in source order.
    pub text: SmallVec<[char; 2]>,
    /// Optional per-cell style. `None` ⇒ inherit the terminal default
    /// (plain). The render layer (phux-client) translates this to SGR.
    pub style: Option<CellStyle>,
    /// phux-foz.12: optional interactive hit target. `None` ⇒ the cell is
    /// inert (clicks over it are consumed as chrome, committing nothing).
    pub hit: Option<CellHit>,
}

/// A window as the `windows` widget sees it: a display name and whether
/// it is the client's active window. Positional index in the slice is the
/// window's selector (matches `select-window index=N`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    /// Window display name (the editable label).
    pub name: String,
    /// `true` for the active window (rendered with the active style).
    pub active: bool,
    /// phux-x2hm: `true` when a pane in this window is zoomed to fill it.
    /// Only ever set on the active window; the `windows` widget appends a
    /// `Z` marker to its tab (tmux's zoomed-pane indicator).
    pub zoomed: bool,
    /// phux-foz.1: `true` when a pane in this window is waiting on a human
    /// answer (an ADR-0035 `AgentEvent::Asked` landed and has not been
    /// cleared by user input). The `windows` widget appends a `!` marker to
    /// the tab so the asking window is findable from any window.
    pub attention: bool,
    /// phux-p4vp: the VCS branch of the window's focused pane, when its
    /// working directory sits inside a git repository (e.g. `main`, or a
    /// short commit hash for a detached HEAD). `None` when the cwd is
    /// unknown or not under version control. The sidebar renders this as a
    /// dim branch line under the window label (herdr-style); the status-bar
    /// `windows` widget ignores it.
    pub branch: Option<String>,
}

/// Context passed to a [`StatusWidget`] at render time.
///
/// Kept intentionally narrow: every field is real data the host already
/// tracks, passed in (rather than read inside the widget) so render is a
/// pure function of context — that's what makes deterministic snapshot
/// tests possible.
#[derive(Debug, Clone, Copy)]
pub struct WidgetContext<'a> {
    /// Wall-clock time the status bar is rendering at.
    pub now: SystemTime,
    /// Current session name (`""` if not in a session).
    pub session_name: &'a str,
    /// Configured TUI prefix chord, used by discoverability widgets.
    pub prefix: &'a str,
    /// The TUI's windows in display order, with the active one flagged.
    /// Consumed by the `windows` (tab-bar) widget; empty for consumers
    /// that don't present windows.
    pub windows: &'a [WindowInfo],
    /// phux-foz.4: the focused pane's live working directory, or `""`
    /// when unknown. Fed by the server's `cwd_changed` agent events
    /// (kernel-queried PTY-child cwd); consumed by the `cwd` widget.
    pub cwd: &'a str,
    /// phux-foz.4: exit code of the last command that finished in the
    /// focused pane, when the shell's OSC-133 `D` mark reported one
    /// (`command_finished.exit_code`). `None` until a command finishes
    /// (or when the shell integration omits the code); consumed by the
    /// `exit` widget.
    pub last_exit: Option<i32>,
}

impl<'a> WidgetContext<'a> {
    /// Build a context carrying the universally-available fields, with the
    /// focused-pane data feeds (`cwd`, `last_exit`) at their unknown
    /// defaults. Hosts that track those feeds override the fields via
    /// struct-update syntax.
    #[must_use]
    pub const fn new(
        now: SystemTime,
        session_name: &'a str,
        prefix: &'a str,
        windows: &'a [WindowInfo],
    ) -> Self {
        Self {
            now,
            session_name,
            prefix,
            windows,
            cwd: "",
            last_exit: None,
        }
    }
}

/// A horizontal strip of cells produced by a widget for one render pass.
///
/// `Cell` is the local widget cell type — see the module-level doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetCells {
    /// Cells in left-to-right display order.
    pub cells: Vec<Cell>,
}

impl WidgetCells {
    /// Build a [`WidgetCells`] from a plain string. Each `char` becomes a
    /// single-cell grapheme; styling is left at default. This is the
    /// simplest possible cell-builder and is what both built-in widgets
    /// use today.
    #[must_use]
    pub fn from_text(s: &str) -> Self {
        Self::from_styled(s, None)
    }

    /// Build a [`WidgetCells`] from a string with one style applied to
    /// every cell. `None` ⇒ plain (terminal default).
    #[must_use]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "style is cloned into each cell; by-value keeps the builder call sites ergonomic"
    )]
    pub fn from_styled(s: &str, style: Option<CellStyle>) -> Self {
        let cells = s
            .chars()
            .map(|c| Cell {
                text: smallvec::smallvec![c],
                style: style.clone(),
                hit: None,
            })
            .collect();
        Self { cells }
    }

    /// True if this strip carries no cells.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Number of cells in the strip.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.cells.len()
    }
}

/// A status-bar widget.
///
/// The trait is **not** named `Widget` because [`crate::schema::Widget`]
/// already occupies that name on the parsed-TOML side.
pub trait StatusWidget: Send + Sync + fmt::Debug + 'static {
    /// Render the widget for the current [`WidgetContext`]. Returns a
    /// horizontal cell strip.
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells;

    /// Optional poll interval. `None` ⇒ this widget needs no time-based
    /// repaint and is redrawn only when the status bar repaints for
    /// other reasons (session-name change, layout change, …). `Some(d)`
    /// ⇒ the status bar schedules a repaint at this cadence.
    fn poll_interval(&self) -> Option<Duration> {
        None
    }

    /// phux-r82.6: for widgets backed by an external asynchronous data
    /// feed (the `exec` kind), the shared feed handle the host must
    /// drive. The host runs the feed's command on its interval and
    /// pushes output through [`ExecFeed::apply_output`]; `render` then
    /// reads the cached cells without ever blocking. `None` for pure
    /// widgets (the default).
    fn exec_feed(&self) -> Option<ExecFeed> {
        None
    }
}

/// Factory function: builds a widget from a TOML `opts` map.
///
/// The universal `style` option is *not* part of a factory's surface:
/// [`WidgetRegistry::build`] extracts and applies it before the factory
/// runs, so factories only ever see their own kind-specific options.
pub type WidgetFactory =
    fn(&BTreeMap<String, toml::Value>) -> Result<Box<dyn StatusWidget>, WidgetError>;

/// The universal widget option every kind accepts (phux-i0e8.4.2):
/// a [`CellStyle`] table applied by the registry, not by factories.
const STYLE_OPT: &str = "style";

/// Documentation spec for one built-in widget kind (phux-i0e8.11.3).
///
/// Each widget file defines a `SPEC` const next to its factory, and the
/// factory validates its options against that same const (via
/// [`reject_unknown_opts`]), so the documented option surface and the
/// enforced one are one object and cannot drift. [`BUILTIN_WIDGET_SPECS`]
/// aggregates them; a unit test pins that list to
/// [`WidgetRegistry::with_builtins`], and the generated reference page
/// `docs/reference/widgets.md` (see `phux::refdocs::widgets`) renders
/// from it.
///
/// Named `WidgetKindSpec` because [`crate::schema::WidgetSpec`] already
/// names the parsed `[[status.widgets]]` TOML entry.
#[derive(Debug, Clone, Copy)]
pub struct WidgetKindSpec {
    /// Registered kind string (the `kind = "..."` value).
    pub kind: &'static str,
    /// One-paragraph human description of what the widget renders.
    pub summary: &'static str,
    /// The kind-specific options the factory accepts. The universal
    /// `style` table is not listed per kind — it is registry-applied and
    /// documented once on the generated page.
    pub options: &'static [WidgetOptSpec],
}

/// One accepted option of a widget kind.
#[derive(Debug, Clone, Copy)]
pub struct WidgetOptSpec {
    /// Canonical option key (kebab-case).
    pub name: &'static str,
    /// Alternative accepted spellings (e.g. `max_len` for `max-len`).
    pub aliases: &'static [&'static str],
    /// Type, default, and behavior in one line, rendered verbatim into
    /// the generated reference.
    pub doc: &'static str,
}

impl WidgetKindSpec {
    /// `true` when `key` is an accepted spelling of one of this kind's
    /// options.
    fn accepts(&self, key: &str) -> bool {
        self.options
            .iter()
            .any(|opt| opt.name == key || opt.aliases.contains(&key))
    }

    /// Every accepted option spelling plus the universal `style` key —
    /// the did-you-mean candidate set for an unknown-option diagnostic.
    fn candidate_keys(&self) -> Vec<&'static str> {
        let mut keys: Vec<&'static str> = self
            .options
            .iter()
            .flat_map(|opt| std::iter::once(opt.name).chain(opt.aliases.iter().copied()))
            .collect();
        keys.push(STYLE_OPT);
        keys
    }
}

/// The doc specs of every built-in widget kind, in ASCII order of kind
/// (matching [`WidgetRegistry::kinds`] on a builtins registry). Pinned to
/// [`WidgetRegistry::with_builtins`] by a unit test below.
pub const BUILTIN_WIDGET_SPECS: &[&WidgetKindSpec] = &[
    &widgets::cwd::SPEC,
    &widgets::exec::SPEC,
    &widgets::exit_status::SPEC,
    &widgets::help_hints::SPEC,
    &widgets::session_name::SPEC,
    &widgets::time::SPEC,
    &widgets::windows::SPEC,
];

/// Parse an optional [`CellStyle`] from an inline-table option.
///
/// Shared by the registry (the universal `style` option) and by widgets
/// with their own style-table options (`windows`' `active`/`inactive`).
pub(crate) fn style_opt(
    kind: &str,
    opts: &BTreeMap<String, toml::Value>,
    key: &str,
) -> Result<Option<CellStyle>, WidgetError> {
    opts.get(key).map_or(Ok(None), |value| {
        value
            .clone()
            .try_into::<CellStyle>()
            .map(Some)
            .map_err(|e| WidgetError::InvalidOption {
                kind: kind.to_owned(),
                message: format!("`{key}` must be a style table: {e}"),
            })
    })
}

/// Reject any option key outside the kind's [`WidgetKindSpec`], naming
/// the widget and suggesting the nearest valid option (phux-i0e8.4.2).
///
/// Every factory calls this first with its own `SPEC` const, so a typo'd
/// option fails the build loudly instead of parsing into an open
/// `BTreeMap` and doing nothing — and the enforced surface is the very
/// object the generated reference documents (phux-i0e8.11.3). The
/// universal `style` key is always accepted: [`WidgetRegistry::build`]
/// extracts and applies it before the factory runs, so factories never
/// see it — but it is a valid spelling and belongs in the suggestions.
pub(crate) fn reject_unknown_opts(
    spec: &WidgetKindSpec,
    opts: &BTreeMap<String, toml::Value>,
) -> Result<(), WidgetError> {
    for key in opts.keys() {
        if key == STYLE_OPT || spec.accepts(key) {
            continue;
        }
        let suggestion = vocab::did_you_mean(key, &spec.candidate_keys())
            .map(|hit| format!(" (did you mean `{hit}`?)"))
            .unwrap_or_default();
        return Err(WidgetError::InvalidOption {
            kind: spec.kind.to_owned(),
            message: format!("unknown option `{key}`{suggestion}"),
        });
    }
    Ok(())
}

/// Decorator applying a widget-level [`CellStyle`] to the wrapped
/// widget's *unstyled* cells (phux-i0e8.4.2).
///
/// Precedence: a cell that already carries a style keeps it — the
/// `windows` widget's `active`/`inactive` segments, `exec`'s SGR-parsed
/// output, and `help-hints`' dim base all win over the widget-level
/// `style` table. Only cells the widget left plain inherit it.
#[derive(Debug)]
struct Styled {
    inner: Box<dyn StatusWidget>,
    style: CellStyle,
}

impl StatusWidget for Styled {
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        let mut cells = self.inner.render(ctx);
        for cell in &mut cells.cells {
            if cell.style.is_none() {
                cell.style = Some(self.style.clone());
            }
        }
        cells
    }

    fn poll_interval(&self) -> Option<Duration> {
        self.inner.poll_interval()
    }

    fn exec_feed(&self) -> Option<ExecFeed> {
        self.inner.exec_feed()
    }
}

/// Registry of widget kinds → factories.
///
/// [`WidgetRegistry::with_builtins`] pre-populates `time` and `session-name`;
/// callers may register additional kinds via [`Self::register`] before
/// the first [`Self::build`] call.
pub struct WidgetRegistry {
    factories: BTreeMap<&'static str, WidgetFactory>,
}

impl fmt::Debug for WidgetRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WidgetRegistry")
            .field("kinds", &self.factories.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl WidgetRegistry {
    /// Empty registry — register kinds explicitly via [`Self::register`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            factories: BTreeMap::new(),
        }
    }

    /// Registry pre-populated with the in-tree widgets: `cwd`, `exec`,
    /// `exit`, `help-hints`, `session-name`, `time`, and `windows`.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register("cwd", widgets::cwd::factory);
        r.register("exec", widgets::exec::factory);
        r.register("exit", widgets::exit_status::factory);
        r.register("help-hints", widgets::help_hints::factory);
        r.register("time", widgets::time::factory);
        r.register("session-name", widgets::session_name::factory);
        r.register("windows", widgets::windows::factory);
        r
    }

    /// Register a factory under `kind`. Later registrations overwrite
    /// earlier ones for the same `kind`.
    pub fn register(&mut self, kind: &'static str, factory: WidgetFactory) {
        self.factories.insert(kind, factory);
    }

    /// Look up a kind and invoke its factory.
    ///
    /// The universal `style` option (phux-i0e8.4.2) is handled here, not
    /// per factory: it is parsed and removed from the opts before the
    /// factory runs, and a non-plain style wraps the built widget in a
    /// decorator that styles its unstyled cells (per-cell styles win —
    /// see `docs/reference/widgets.md` for the precedence contract).
    ///
    /// # Errors
    ///
    /// Returns [`WidgetError::UnknownKind`] if `spec.kind` is not
    /// registered, [`WidgetError::InvalidOption`] if `style` is not a
    /// valid style table, or forwards [`WidgetError::InvalidOption`]
    /// from the factory.
    pub fn build(&self, spec: &WidgetSpec) -> Result<Box<dyn StatusWidget>, WidgetError> {
        let factory = self
            .factories
            .get(spec.kind.as_str())
            .ok_or_else(|| WidgetError::UnknownKind(spec.kind.clone()))?;
        let style = style_opt(&spec.kind, &spec.opts, STYLE_OPT)?;
        let widget = if spec.opts.contains_key(STYLE_OPT) {
            let mut opts = spec.opts.clone();
            opts.remove(STYLE_OPT);
            factory(&opts)?
        } else {
            factory(&spec.opts)?
        };
        Ok(match style.filter(|s| !s.is_plain()) {
            Some(style) => Box::new(Styled {
                inner: widget,
                style,
            }),
            None => widget,
        })
    }

    /// Registered widget kinds, in ASCII order (handy for tests).
    #[must_use]
    pub fn kinds(&self) -> Vec<&'static str> {
        self.factories.keys().copied().collect()
    }
}

impl Default for WidgetRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

/// Failures from widget construction.
#[derive(Debug, thiserror::Error)]
pub enum WidgetError {
    /// `spec.kind` was not in the registry.
    #[error("unknown widget kind: {0}")]
    UnknownKind(String),
    /// A factory rejected one of its options.
    #[error("invalid option for widget {kind}: {message}")]
    InvalidOption {
        /// The widget kind that rejected the option.
        kind: String,
        /// Human-readable explanation.
        message: String,
    },
}

#[cfg(test)]
mod spec_tests {
    use std::collections::BTreeSet;

    use super::{BUILTIN_WIDGET_SPECS, WidgetRegistry};

    /// The registry-vs-spec pin (phux-i0e8.11.3): the documented kinds are
    /// exactly the kinds [`WidgetRegistry::with_builtins`] registers, in
    /// the same (ASCII) order. Registering a new builtin without a doc
    /// spec — or documenting a kind that is not registered — fails here,
    /// which is what keeps the generated `docs/reference/widgets.md`
    /// covering exactly the real widget surface.
    #[test]
    fn builtin_specs_match_the_builtin_registry() {
        let documented: Vec<&str> = BUILTIN_WIDGET_SPECS.iter().map(|s| s.kind).collect();
        assert_eq!(
            documented,
            WidgetRegistry::with_builtins().kinds(),
            "widget doc specs and with_builtins() drifted \
             (add/remove the SPEC const next to the factory)",
        );
    }

    /// Doc specs exist to be rendered: every summary and every option doc
    /// must carry text, and option spellings must not collide within or
    /// across name/alias lists of a kind.
    #[test]
    fn builtin_specs_are_renderable_and_unambiguous() {
        for spec in BUILTIN_WIDGET_SPECS {
            assert!(
                !spec.summary.trim().is_empty(),
                "`{}` has an empty summary",
                spec.kind
            );
            let mut seen = BTreeSet::new();
            for opt in spec.options {
                assert!(
                    !opt.doc.trim().is_empty(),
                    "`{}` option `{}` has an empty doc",
                    spec.kind,
                    opt.name
                );
                for key in std::iter::once(opt.name).chain(opt.aliases.iter().copied()) {
                    assert!(
                        seen.insert(key),
                        "`{}` documents option spelling `{key}` twice",
                        spec.kind
                    );
                }
            }
        }
    }
}
