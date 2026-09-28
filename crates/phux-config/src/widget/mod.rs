//! Status-bar widget trait, registry, and built-in widgets.
//!
//! [`crate::Widget`] is the parsed TOML entry; the runtime trait is
//! [`StatusWidget`]. The bar carries its own lightweight [`Cell`] because the
//! status bar never reaches the wire (ADR-0013).

use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, SystemTime};

use smallvec::SmallVec;

use crate::schema::WidgetSpec;
use crate::vocab;

mod status_bar;
mod widgets;

pub use status_bar::{StatusBar, merge_widget_contributions, row_to_string};
pub use widgets::cwd::CwdWidget;
pub use widgets::exec::{ExecFeed, ExecWidget};
pub use widgets::exit_status::ExitWidget;
pub use widgets::help_hints::HelpHintsWidget;
pub use widgets::session_name::SessionNameWidget;
pub use widgets::spacer::SpacerWidget;
pub use widgets::switch::SwitchWidget;
pub use widgets::text::TextWidget;
pub use widgets::time::TimeWidget;
pub use widgets::windows::WindowsWidget;

/// Visual style for a status-bar [`Cell`] as plain data. Colors are strings
/// (`"red"`, `"#cdd6f4"`, `"12"`) the render layer interprets (ADR-0020).
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent SGR attribute toggle"
)]
pub struct CellStyle {
    /// Foreground color, or `None` for the terminal default.
    pub fg: Option<String>,
    /// Background color, or `None` for the terminal default.
    pub bg: Option<String>,
    /// Bold.
    pub bold: bool,
    /// Dim / faint.
    pub dim: bool,
    /// Italic.
    pub italic: bool,
    /// Underline.
    pub underline: bool,
    /// Reverse video.
    pub reverse: bool,
}

impl CellStyle {
    /// `true` when every field is at its default (no styling).
    #[must_use]
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }

    /// `over` layered onto `self`: colours `over` sets replace, and each
    /// attribute is on when either side turns it on.
    #[must_use]
    pub fn layered(&self, over: &Self) -> Self {
        Self {
            fg: over.fg.clone().or_else(|| self.fg.clone()),
            bg: over.bg.clone().or_else(|| self.bg.clone()),
            bold: self.bold || over.bold,
            dim: self.dim || over.dim,
            italic: self.italic || over.italic,
            underline: self.underline || over.underline,
            reverse: self.reverse || over.reverse,
        }
    }
}

/// The click target a composed cell carries. Widgets stamp it at render time
/// and the composer copies it through untouched, so paint and hit-testing
/// derive from one model and cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellHit {
    /// Selects window `i` (the `select-window` index).
    Window(usize),
    /// Opens the fleet switcher.
    Switch,
    /// Invokes a named, argument-free TUI action through the ordinary
    /// action dispatcher.
    Action(&'static str),
}

/// A single status-bar cell.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cell {
    /// Grapheme cluster: base codepoint, then combining codepoints. Empty for
    /// a blank cell or the claimed second column of a double-width character.
    pub text: SmallVec<[char; 2]>,
    /// Per-cell style; `None` is the terminal default.
    pub style: Option<CellStyle>,
    /// Click target; `None` is inert chrome.
    pub hit: Option<CellHit>,
}

/// A window as the `windows` widget and the sidebar see it. Its index in the
/// slice is its `select-window` index.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WindowInfo {
    /// Window display name.
    pub name: String,
    /// The client's active window.
    pub active: bool,
    /// A pane in this (active) window is zoomed; marked `Z`.
    pub zoomed: bool,
    /// A pane is waiting on a human answer (ADR-0035); marked `!`.
    pub attention: bool,
    /// VCS branch of the focused pane's cwd (shown by the sidebar only).
    pub branch: Option<String>,
    /// Compact exit status of a retained pane (`"3"`, `"sig9"`, or `""`),
    /// ADR-0124; `None` while every pane is live.
    pub exited: Option<String>,
    /// Pre-resolved agent badge of the focused pane, painted before the name.
    pub badge: Option<WindowBadge>,
}

/// A pre-resolved agent badge for a [`WindowInfo`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WindowBadge {
    /// The single-cell glyph (`●`, `◆`, `◐`, `○`).
    pub glyph: String,
    /// Its style, layered over the tab segment's own style.
    pub style: CellStyle,
}

impl WindowInfo {
    /// Compact chrome suffix for a retained pane: ` x`, ` x3`, or ` xsig9`.
    #[must_use]
    pub fn exited_marker(&self) -> Option<String> {
        let status = self.exited.as_ref()?;
        Some(if status.is_empty() {
            " x".to_owned()
        } else {
            format!(" x{status}")
        })
    }
}

/// Context passed to a [`StatusWidget`] at render time, so render is a pure
/// function of it.
#[derive(Debug, Clone, Copy)]
pub struct WidgetContext<'a> {
    /// Wall-clock time the status bar is rendering at.
    pub now: SystemTime,
    /// Current session name (`""` if not in a session).
    pub session_name: &'a str,
    /// Configured TUI prefix chord.
    pub prefix: &'a str,
    /// Windows in display order.
    pub windows: &'a [WindowInfo],
    /// The focused pane's working directory, or `""` when unknown.
    pub cwd: &'a str,
    /// Exit code of the focused pane's last finished command (OSC 133 `D`).
    pub last_exit: Option<i32>,
    /// Width of the whole row (not the widget's budget), read by the
    /// `min-cols` / `max-cols` options. Set by [`StatusBar::render`].
    pub cols: u16,
}

impl<'a> WidgetContext<'a> {
    /// Build a context with `cwd` and `last_exit` unknown.
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
            cols: 0,
        }
    }
}

/// A horizontal strip of cells produced by a widget for one render pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetCells {
    /// Cells in left-to-right display order.
    pub cells: Vec<Cell>,
}

/// The advance width of `ch` in terminal cells, or `None` for a character
/// that must never reach a VT emitter.
///
/// Status-bar text (window names, cwd, `exec` output) is untrusted input on a
/// path that writes escape sequences, so control characters are refused. Bidi
/// formatting characters are refused too: zero-width, but they reorder
/// everything drawn after them, and real RTL text does not need them.
#[must_use]
pub fn cell_width(ch: char) -> Option<usize> {
    if ch.is_control() || is_bidi_control(ch) {
        return None;
    }
    unicode_width::UnicodeWidthChar::width(ch)
}

/// The explicit bidi formatting characters: embeddings/overrides, isolates,
/// and the standalone marks.
#[must_use]
pub const fn is_bidi_control(ch: char) -> bool {
    matches!(
        ch,
        '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// The columns one composed [`Cell`] advances the terminal; the claimed
/// second cell of a double-width character reports `0`.
#[must_use]
pub fn cell_columns(cell: &Cell) -> usize {
    cell.text.iter().copied().filter_map(cell_width).sum()
}

/// Drop a trailing double-width base whose claimed cell was cut away: the
/// terminal would advance two columns for it and wrap the bar into the pane
/// grid. The strip may come back one cell short; it must never overrun.
pub(crate) fn drop_orphan_base(cells: &mut Vec<Cell>) {
    while cells.last().is_some_and(|c| cell_columns(c) > 1) {
        cells.pop();
    }
}

/// The width of `s` in terminal cells, ignoring anything unprintable.
#[must_use]
pub fn display_width(s: &str) -> usize {
    s.chars().filter_map(cell_width).sum()
}

impl WidgetCells {
    /// Build unstyled cells from a string.
    #[must_use]
    pub fn from_text(s: &str) -> Self {
        Self::from_styled(s, None)
    }

    /// Build cells with one style on every cell. Unprintable characters are
    /// dropped, zero-width marks join the previous cell, and a double-width
    /// character claims two cells: `cells.len()` is the bar's width contract.
    #[must_use]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "style is cloned into each cell; by-value keeps call sites ergonomic"
    )]
    pub fn from_styled(s: &str, style: Option<CellStyle>) -> Self {
        let mut cells: Vec<Cell> = Vec::with_capacity(s.len());
        for ch in s.chars() {
            let Some(width) = cell_width(ch) else {
                continue;
            };
            if width == 0 {
                if let Some(last) = cells.last_mut() {
                    last.text.push(ch);
                }
                continue;
            }
            cells.push(Cell {
                text: smallvec::smallvec![ch],
                style: style.clone(),
                hit: None,
            });
            for _ in 1..width {
                cells.push(Cell {
                    text: SmallVec::new(),
                    style: style.clone(),
                    hit: None,
                });
            }
        }
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

    /// Cut the strip to at most `budget` cells, replacing the last survivor
    /// with an ellipsis (keeping its style and hit target) so a shortened
    /// strip never masquerades as complete. A double-width character is
    /// never split, and the ellipsis always lands on a drawn base cell.
    /// Strips that already fit are untouched.
    pub fn clip(&mut self, budget: usize) {
        if self.cells.len() <= budget {
            return;
        }
        let carried = self.cells.first().map(|c| (c.style.clone(), c.hit));
        self.cells.truncate(budget);
        drop_orphan_base(&mut self.cells);
        if self.cells.last().is_some_and(|c| cell_columns(c) == 0) {
            self.cells.pop();
        }
        if let Some(last) = self.cells.last_mut() {
            last.text = smallvec::smallvec![ELLIPSIS];
            return;
        }
        // Nothing survived (a one-column budget against a wide first
        // character): the cut must still be marked.
        if budget > 0
            && let Some((style, hit)) = carried
        {
            self.cells.push(Cell {
                text: smallvec::smallvec![ELLIPSIS],
                style,
                hit,
            });
        }
    }

    /// A copy of this strip cut to `budget` cells. See [`Self::clip`].
    #[must_use]
    pub fn clipped(mut self, budget: usize) -> Self {
        self.clip(budget);
        self
    }
}

/// The single-cell mark for "there is more here than fits".
pub const ELLIPSIS: char = '…';

/// A status-bar widget.
pub trait StatusWidget: Send + Sync + fmt::Debug + 'static {
    /// Render the widget for the current [`WidgetContext`].
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells;

    /// Render into at most `budget` cells. The default clips with an
    /// ellipsis; structured widgets (the `windows` tab bar) override it to
    /// degrade more gracefully.
    fn render_within(&self, ctx: &WidgetContext<'_>, budget: usize) -> WidgetCells {
        self.render(ctx).clipped(budget)
    }

    /// `true` for a widget that absorbs the row's leftover columns instead of
    /// having content (only `spacer`). Takes `ctx` because a widget gated out
    /// by `min-cols` / `max-cols` is absent, not elastic.
    fn elastic(&self, _ctx: &WidgetContext<'_>) -> bool {
        false
    }

    /// Repaint cadence for time-based widgets; `None` repaints only when the
    /// bar does.
    fn poll_interval(&self) -> Option<Duration> {
        None
    }

    /// The async feed the host must drive (the `exec` kind): the host runs the
    /// command and pushes output through [`ExecFeed::apply_output`], and
    /// `render` reads the cached cells without blocking.
    fn exec_feed(&self) -> Option<ExecFeed> {
        None
    }
}

/// Builds a widget from its kind-specific options (the registry strips the
/// universal ones first).
pub type WidgetFactory =
    fn(&BTreeMap<String, toml::Value>) -> Result<Box<dyn StatusWidget>, WidgetError>;

/// Universal widget-level [`CellStyle`] table.
const STYLE_OPT: &str = "style";
/// Universal visibility bounds on the *bar* width: outside them the widget
/// renders nothing, so one lineup can adapt to the terminal.
const MIN_COLS_OPT: &str = "min-cols";
const MAX_COLS_OPT: &str = "max-cols";
const UNIVERSAL_OPTS: [&str; 3] = [STYLE_OPT, MIN_COLS_OPT, MAX_COLS_OPT];

/// Documentation spec for one built-in widget kind. The factory validates its
/// options against the same const, so the documented surface
/// (`docs/reference/widgets.md`) and the enforced one cannot drift.
#[derive(Debug, Clone, Copy)]
pub struct WidgetKindSpec {
    /// Registered kind string (the `kind = "..."` value).
    pub kind: &'static str,
    /// One-paragraph human description of what the widget renders.
    pub summary: &'static str,
    /// The kind-specific options the factory accepts.
    pub options: &'static [WidgetOptSpec],
}

/// One accepted option of a widget kind.
#[derive(Debug, Clone, Copy)]
pub struct WidgetOptSpec {
    /// Canonical option key (kebab-case).
    pub name: &'static str,
    /// Alternative accepted spellings (e.g. `max_len` for `max-len`).
    pub aliases: &'static [&'static str],
    /// Type, default, and behavior in one line, rendered verbatim.
    pub doc: &'static str,
}

impl WidgetKindSpec {
    fn accepts(&self, key: &str) -> bool {
        self.options
            .iter()
            .any(|opt| opt.name == key || opt.aliases.contains(&key))
    }

    /// Did-you-mean candidates: every option spelling plus the universals.
    fn candidate_keys(&self) -> Vec<&'static str> {
        let mut keys: Vec<&'static str> = self
            .options
            .iter()
            .flat_map(|opt| std::iter::once(opt.name).chain(opt.aliases.iter().copied()))
            .collect();
        keys.extend_from_slice(&UNIVERSAL_OPTS);
        keys
    }
}

/// Doc specs of every built-in widget kind, in ASCII order of kind.
pub const BUILTIN_WIDGET_SPECS: &[&WidgetKindSpec] = &[
    &widgets::cwd::SPEC,
    &widgets::exec::SPEC,
    &widgets::exit_status::SPEC,
    &widgets::help_hints::SPEC,
    &widgets::session_name::SPEC,
    &widgets::spacer::SPEC,
    &widgets::switch::SPEC,
    &widgets::text::SPEC,
    &widgets::time::SPEC,
    &widgets::windows::SPEC,
];

pub(crate) fn invalid(kind: &str, message: String) -> WidgetError {
    WidgetError::InvalidOption {
        kind: kind.to_owned(),
        message,
    }
}

/// Parse an optional [`CellStyle`] from an inline-table option.
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
            .map_err(|e| invalid(kind, format!("`{key}` must be a style table: {e}")))
    })
}

/// Parse an optional string option.
pub(crate) fn string_opt(
    kind: &str,
    opts: &BTreeMap<String, toml::Value>,
    key: &str,
) -> Result<Option<String>, WidgetError> {
    match opts.get(key) {
        None => Ok(None),
        Some(toml::Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(invalid(
            kind,
            format!("`{key}` must be a string, got {}", other.type_str()),
        )),
    }
}

/// Parse an optional integer option that must be `> 0`, spelled `key` or
/// `alias`.
pub(crate) fn positive_opt(
    kind: &str,
    opts: &BTreeMap<String, toml::Value>,
    key: &str,
    alias: Option<&str>,
) -> Result<Option<usize>, WidgetError> {
    match opts.get(key).or_else(|| alias.and_then(|a| opts.get(a))) {
        None => Ok(None),
        Some(toml::Value::Integer(n)) if *n > 0 => usize::try_from(*n)
            .map(Some)
            .map_err(|_| invalid(kind, format!("`{key}` does not fit in usize: {n}"))),
        Some(toml::Value::Integer(n)) => {
            Err(invalid(kind, format!("`{key}` must be > 0, got {n}")))
        }
        Some(other) => Err(invalid(
            kind,
            format!("`{key}` must be an integer, got {}", other.type_str()),
        )),
    }
}

/// Reject any option key outside the kind's [`WidgetKindSpec`], suggesting
/// the nearest valid one. Every factory calls this first.
pub(crate) fn reject_unknown_opts(
    spec: &WidgetKindSpec,
    opts: &BTreeMap<String, toml::Value>,
) -> Result<(), WidgetError> {
    for key in opts.keys() {
        if UNIVERSAL_OPTS.contains(&key.as_str()) || spec.accepts(key) {
            continue;
        }
        let suggestion = vocab::did_you_mean(key, &spec.candidate_keys())
            .map(|hit| format!(" (did you mean `{hit}`?)"))
            .unwrap_or_default();
        return Err(invalid(
            spec.kind,
            format!("unknown option `{key}`{suggestion}"),
        ));
    }
    Ok(())
}

/// Decorator applying a widget-level [`CellStyle`] to the wrapped widget's
/// *unstyled* cells; a cell the widget styled itself keeps its style.
#[derive(Debug)]
struct Styled {
    inner: Box<dyn StatusWidget>,
    style: CellStyle,
}

impl Styled {
    fn apply(&self, mut cells: WidgetCells) -> WidgetCells {
        for cell in &mut cells.cells {
            if cell.style.is_none() {
                cell.style = Some(self.style.clone());
            }
        }
        cells
    }
}

impl StatusWidget for Styled {
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        self.apply(self.inner.render(ctx))
    }

    fn render_within(&self, ctx: &WidgetContext<'_>, budget: usize) -> WidgetCells {
        self.apply(self.inner.render_within(ctx, budget))
    }

    fn elastic(&self, ctx: &WidgetContext<'_>) -> bool {
        self.inner.elastic(ctx)
    }

    fn poll_interval(&self) -> Option<Duration> {
        self.inner.poll_interval()
    }

    fn exec_feed(&self) -> Option<ExecFeed> {
        self.inner.exec_feed()
    }
}

/// Decorator gating a widget on the bar's width; outside the range it
/// renders zero cells and costs no natural width.
#[derive(Debug)]
struct ColRange {
    inner: Box<dyn StatusWidget>,
    min: Option<u16>,
    max: Option<u16>,
}

impl ColRange {
    fn visible(&self, ctx: &WidgetContext<'_>) -> bool {
        self.min.is_none_or(|m| ctx.cols >= m) && self.max.is_none_or(|m| ctx.cols <= m)
    }
}

impl StatusWidget for ColRange {
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        if self.visible(ctx) {
            self.inner.render(ctx)
        } else {
            WidgetCells { cells: Vec::new() }
        }
    }

    fn render_within(&self, ctx: &WidgetContext<'_>, budget: usize) -> WidgetCells {
        if self.visible(ctx) {
            self.inner.render_within(ctx, budget)
        } else {
            WidgetCells { cells: Vec::new() }
        }
    }

    fn elastic(&self, ctx: &WidgetContext<'_>) -> bool {
        self.visible(ctx) && self.inner.elastic(ctx)
    }

    fn poll_interval(&self) -> Option<Duration> {
        self.inner.poll_interval()
    }

    fn exec_feed(&self) -> Option<ExecFeed> {
        self.inner.exec_feed()
    }
}

/// Parse an optional `u16` column-count option.
fn cols_opt(
    kind: &str,
    opts: &BTreeMap<String, toml::Value>,
    key: &str,
) -> Result<Option<u16>, WidgetError> {
    match opts.get(key) {
        None => Ok(None),
        Some(toml::Value::Integer(n)) => u16::try_from(*n).map(Some).map_err(|_| {
            invalid(
                kind,
                format!("`{key}` must be a column count in 0..=65535, got {n}"),
            )
        }),
        Some(other) => Err(invalid(
            kind,
            format!("`{key}` must be an integer, got {}", other.type_str()),
        )),
    }
}

/// Registry of widget kinds to factories.
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
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            factories: BTreeMap::new(),
        }
    }

    /// Registry pre-populated with the built-in widgets.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register("cwd", widgets::cwd::factory);
        r.register("exec", widgets::exec::factory);
        r.register("exit", widgets::exit_status::factory);
        r.register("help-hints", widgets::help_hints::factory);
        r.register("time", widgets::time::factory);
        r.register("session-name", widgets::session_name::factory);
        r.register("spacer", widgets::spacer::factory);
        r.register("switch", widgets::switch::factory);
        r.register("text", widgets::text::factory);
        r.register("windows", widgets::windows::factory);
        r
    }

    /// Register a factory under `kind`, replacing any earlier one.
    pub fn register(&mut self, kind: &'static str, factory: WidgetFactory) {
        self.factories.insert(kind, factory);
    }

    /// Look up a kind and invoke its factory. The universal options are
    /// stripped before the factory runs and applied as decorators.
    ///
    /// # Errors
    ///
    /// [`WidgetError::UnknownKind`] for an unregistered kind, or
    /// [`WidgetError::InvalidOption`] from a universal option or the factory.
    pub fn build(&self, spec: &WidgetSpec) -> Result<Box<dyn StatusWidget>, WidgetError> {
        let factory = self
            .factories
            .get(spec.kind.as_str())
            .ok_or_else(|| WidgetError::UnknownKind(spec.kind.clone()))?;
        let style = style_opt(&spec.kind, &spec.opts, STYLE_OPT)?;
        let min = cols_opt(&spec.kind, &spec.opts, MIN_COLS_OPT)?;
        let max = cols_opt(&spec.kind, &spec.opts, MAX_COLS_OPT)?;
        if let (Some(min), Some(max)) = (min, max)
            && min > max
        {
            return Err(invalid(
                &spec.kind,
                format!(
                    "`{MIN_COLS_OPT}` ({min}) is above `{MAX_COLS_OPT}` ({max}), so this widget \
                     could never render"
                ),
            ));
        }

        let mut opts = spec.opts.clone();
        for key in UNIVERSAL_OPTS {
            opts.remove(key);
        }
        let widget = factory(&opts)?;

        let widget = match style.filter(|s| !s.is_plain()) {
            Some(style) => Box::new(Styled {
                inner: widget,
                style,
            }),
            None => widget,
        };
        // Gate outermost so a hidden widget also costs no style pass.
        Ok(if min.is_some() || max.is_some() {
            Box::new(ColRange {
                inner: widget,
                min,
                max,
            })
        } else {
            widget
        })
    }

    /// Registered widget kinds, in ASCII order.
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
mod tests {
    use std::collections::BTreeSet;

    use super::{BUILTIN_WIDGET_SPECS, WidgetCells, WidgetRegistry, display_width};

    /// `cells.len()` is the bar's width contract: a double-width character
    /// claims two cells (one CJK name once overflowed the row into the pane
    /// grid), a combining mark joins its base, and control characters never
    /// become cells.
    #[test]
    fn cells_count_terminal_columns() {
        let cells = WidgetCells::from_text("日本");
        assert_eq!(cells.cells.len(), 4);
        assert_eq!(cells.cells.len(), display_width("日本"));
        assert!(cells.cells[1].text.is_empty());
        assert_eq!(cells.cells[2].text.as_slice(), ['本']);

        let cells = WidgetCells::from_text("cafe\u{301}");
        assert_eq!(cells.cells.len(), 4);
        assert_eq!(cells.cells[3].text.as_slice(), ['e', '\u{301}']);

        let cells = WidgetCells::from_text("a\u{1b}[31mb\u{7}");
        let text: String = cells
            .cells
            .iter()
            .flat_map(|c| c.text.iter().copied())
            .collect();
        assert_eq!(text, "a[31mb");
    }

    /// The documented kinds are exactly the registered builtins, and each
    /// spec is renderable and unambiguous (`docs/reference/widgets.md`).
    #[test]
    fn builtin_specs_match_the_registry_and_are_renderable() {
        let documented: Vec<&str> = BUILTIN_WIDGET_SPECS.iter().map(|s| s.kind).collect();
        assert_eq!(documented, WidgetRegistry::with_builtins().kinds());
        for spec in BUILTIN_WIDGET_SPECS {
            assert!(!spec.summary.trim().is_empty(), "`{}` summary", spec.kind);
            let mut seen = BTreeSet::new();
            for opt in spec.options {
                assert!(
                    !opt.doc.trim().is_empty(),
                    "`{}.{}` doc",
                    spec.kind,
                    opt.name
                );
                for key in std::iter::once(opt.name).chain(opt.aliases.iter().copied()) {
                    assert!(seen.insert(key), "`{}` spells `{key}` twice", spec.kind);
                }
            }
        }
    }
}
