//! Status-bar widget trait, registry, composer, and built-in renderers.
//!
//! [`phux_config::Widget`] is the parsed TOML entry, and each kind's options
//! are validated by its `phux_config::widget::kinds` module; the runtime trait
//! is [`StatusWidget`]. The bar carries its own lightweight [`Cell`] because
//! the status bar never reaches the wire (ADR-0013).

use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, SystemTime};

use phux_config::WidgetSpec;
pub use phux_config::widget::{CellStyle, WidgetError};
use phux_config::widget::{UniversalOpts, split_universal};
use smallvec::SmallVec;

mod bar;
mod button;
mod cwd;
mod exec;
mod exit_status;
mod help_hints;
mod session_name;
mod spacer;
mod switch;
mod text;
mod time;
mod windows;

pub use bar::{StatusBar, row_to_string};
pub use button::ButtonWidget;
pub use cwd::CwdWidget;
pub use exec::{ExecFeed, ExecWidget};
pub use exit_status::ExitWidget;
pub use help_hints::HelpHintsWidget;
pub use session_name::SessionNameWidget;
pub use spacer::SpacerWidget;
pub use switch::SwitchWidget;
pub use text::TextWidget;
pub use time::TimeWidget;
pub use windows::WindowsWidget;

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
    /// Terminal width the `min-cols` / `max-cols` gates read;
    /// [`StatusBar::render`] raises it to the row's width at least.
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
/// Chrome text (window names, pane titles set over OSC 2, cwd, `exec` output)
/// is untrusted input on a path that writes escape sequences, so control
/// characters are refused. Bidi formatting characters are refused too:
/// zero-width, but they reorder everything drawn after them, so a pane could
/// read as another's, and real RTL text does not need them.
#[must_use]
pub fn cell_width(ch: char) -> Option<usize> {
    if ch.is_control() || is_bidi_control(ch) {
        return None;
    }
    unicode_width::UnicodeWidthChar::width(ch)
}

/// The explicit bidi formatting characters: the embedding/override set
/// (`U+202A`-`U+202E`), the isolates (`U+2066`-`U+2069`), and the
/// standalone marks (`U+061C`, `U+200E`, `U+200F`).
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
fn drop_orphan_base(cells: &mut Vec<Cell>) {
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
        r.register("button", button::factory);
        r.register("cwd", cwd::factory);
        r.register("exec", exec::factory);
        r.register("exit", exit_status::factory);
        r.register("help-hints", help_hints::factory);
        r.register("time", time::factory);
        r.register("session-name", session_name::factory);
        r.register("spacer", spacer::factory);
        r.register("switch", switch::factory);
        r.register("text", text::factory);
        r.register("windows", windows::factory);
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
        let (universal, opts) = split_universal(spec)?;
        let widget = factory(&opts)?;

        let UniversalOpts {
            style,
            min_cols: min,
            max_cols: max,
        } = universal;
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

#[cfg(test)]
mod tests {
    use phux_config::widget::BUILTIN_WIDGET_SPECS;

    use super::{WidgetCells, WidgetRegistry, display_width};

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

    /// The documented, validated kinds are exactly the registered builtins
    /// (`docs/reference/widgets.md`), so the specs and the registry cannot
    /// drift.
    #[test]
    fn builtin_specs_match_the_registry() {
        let documented: Vec<&str> = BUILTIN_WIDGET_SPECS.iter().map(|s| s.kind).collect();
        assert_eq!(documented, WidgetRegistry::with_builtins().kinds());
    }
}
