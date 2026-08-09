//! Status-bar chrome layer (phux-5ke.2).
//!
//! Replaces the hand-painted cell positioning from
//! `attach::status_bar::StatusBarPainter` with a ratatui-based composer
//! for the reserved bottom (or top) row of the outer terminal. The
//! composer lives in the chrome layer, the only place that imports
//! `ratatui`; the pane-interior substrate is in the `phux-client-core`
//! crate, which has no `ratatui` dependency (ADR-0020).
//!
//! Pipeline:
//!
//! 1. Higher layer ([`phux_config::widget::StatusBar`]) composes the
//!    widget row into a `Vec<WidgetCell>` of caller-supplied width.
//! 2. [`render_status_bar`] copies those cells into a ratatui
//!    [`ratatui::buffer::Buffer`] of shape `cols × 1`. Layout splits
//!    are available via ratatui's [`ratatui::layout::Layout`] if a
//!    consumer ever wants per-segment styling — today we mirror the
//!    composer's output 1:1.
//! 3. [`render_status_bar`] emits raw VT bytes (CUP + per-cell SGR +
//!    grapheme) to the writer. We do **not** route through crossterm;
//!    the rest of `phux-client` writes raw VT to stdout and the
//!    boundary stays clean.
//!
//! ### SGR + cursor invariants (per ADR-0020 §Decision)
//!
//! - The painter emits a hard SGR reset (`\x1b[0m`) after the row so
//!   subsequent paints don't inherit our attributes.
//! - The painter does **not** restore the cursor itself — the caller
//!   (`crate::attach::paint::paint_bar_after_pane`) restores the
//!   focused pane's logical cursor after the bar paints (this matches
//!   the post-34bfc07 paint order).
//!
//! ### Placement
//!
//! Defaults to [`Position::Bottom`] per `docs/consumers/tui.md` §8.5.
//! `Position::Top` is surfaced by the `[status] position = "top"` config
//! key (phux-foz.8); the pane content rect shifts down one row to match
//! (see `crate::attach::paint::content_rect`).

use std::io::{self, Write};
use std::time::{Duration, SystemTime};

use std::str::FromStr;

use phux_config::widget::{Cell as WidgetCell, CellStyle, StatusBar, WidgetContext, WindowInfo};
use ratatui::buffer::{Buffer, Cell as RatatuiCell};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

/// Where the status bar lives in the outer terminal. Defaults to
/// [`Self::Bottom`] per `docs/consumers/tui.md` §8.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Position {
    /// One row at the very bottom of the outer terminal.
    #[default]
    Bottom,
    /// One row at the very top of the outer terminal. Surfaced by the
    /// `[status] position = "top"` config key (phux-foz.8).
    Top,
}

impl From<phux_config::StatusPosition> for Position {
    /// Map the `[status] position` config value onto the render enum
    /// (phux-foz.8). The mapping lives at this boundary so `phux-config`
    /// stays free of render types (ADR-0020).
    fn from(pos: phux_config::StatusPosition) -> Self {
        match pos {
            phux_config::StatusPosition::Bottom => Self::Bottom,
            phux_config::StatusPosition::Top => Self::Top,
        }
    }
}

/// Inputs the chrome composer needs to paint one frame of the bar.
///
/// Mirrors what `phux_config::widget::WidgetContext` carries today
/// (clock + session name). Held as a struct here so the caller can
/// pass one borrow across the ratatui boundary without juggling
/// individual scalars.
#[derive(Debug, Clone, Copy)]
pub struct StatusBarContext<'a> {
    /// Wall-clock time the bar is rendering at.
    pub now: SystemTime,
    /// Current session name (`""` if not in a session).
    pub session_name: &'a str,
    /// Configured prefix chord.
    pub prefix: &'a str,
    /// The TUI's windows in display order (active one flagged), consumed
    /// by the `windows` widget. Empty ⇒ no window bar. TUI-side data fed
    /// into the widget pipeline via [`Self::as_widget`].
    pub windows: &'a [WindowInfo],
    /// phux-foz.4: the focused pane's live working directory (`""` when
    /// unknown), consumed by the `cwd` widget. Injected by the painter
    /// from driver-fed state, like `windows`.
    pub cwd: &'a str,
    /// phux-foz.4: the focused pane's last known command exit code
    /// (OSC-133 `command_finished`), consumed by the `exit` widget.
    pub last_exit: Option<i32>,
}

impl<'a> StatusBarContext<'a> {
    /// Convert to the lower-level [`WidgetContext`] expected by
    /// [`StatusBar::render`].
    #[must_use]
    pub const fn as_widget(&self) -> WidgetContext<'a> {
        WidgetContext {
            now: self.now,
            session_name: self.session_name,
            prefix: self.prefix,
            windows: self.windows,
            cwd: self.cwd,
            last_exit: self.last_exit,
            // Placeholder: `StatusBar::render` stamps the real row width
            // (the one place that knows it) before any widget sees this.
            cols: 0,
        }
    }
}

/// Build a [`StatusBarContext`] for one render pass.
///
/// Kept so callers in `attach/` can construct one without depending on
/// `phux-config`'s internals directly. Window data is injected by the
/// painter (see [`StatusBarPainter::paint`]); standalone callers pass an
/// empty slice. The focused-pane data feeds (`cwd`, `last_exit`) are
/// painter-owned too and injected the same way.
#[must_use]
pub const fn make_context(session_name: &str, now: SystemTime) -> StatusBarContext<'_> {
    StatusBarContext {
        now,
        session_name,
        prefix: "C-a",
        windows: &[],
        cwd: "",
        last_exit: None,
    }
}

/// Translate a phux-config [`CellStyle`] (plain data) into a ratatui
/// [`Style`]. Color strings are parsed at this boundary (ADR-0020); an
/// unparseable color degrades to the terminal default with a warning
/// rather than failing the paint.
fn to_ratatui_style(style: &CellStyle) -> Style {
    let mut s = Style::default();
    if let Some(fg) = parse_color(style.fg.as_deref()) {
        s = s.fg(fg);
    }
    if let Some(bg) = parse_color(style.bg.as_deref()) {
        s = s.bg(bg);
    }
    let mut m = Modifier::empty();
    m.set(Modifier::BOLD, style.bold);
    m.set(Modifier::DIM, style.dim);
    m.set(Modifier::ITALIC, style.italic);
    m.set(Modifier::UNDERLINED, style.underline);
    m.set(Modifier::REVERSED, style.reverse);
    s.add_modifier(m)
}

/// Parse a color string (`"red"`, `"#cdd6f4"`, `"12"`) into a ratatui
/// [`Color`]. `None`/unparseable ⇒ `None` (terminal default).
fn parse_color(spec: Option<&str>) -> Option<Color> {
    let s = spec?;
    Color::from_str(s).map_or_else(
        |_| {
            tracing::warn!(color = s, "unrecognized status-bar color; using default");
            None
        },
        Some,
    )
}

/// Render the composed status row at `row_index`, spanning `cols` columns from
/// origin column `x`, and emit raw VT bytes to `out`.
///
/// `x` is `0` for a full-width bar; a docked sidebar shifts the origin (and
/// narrows `cols`) so the row paints beside the strip rather than under it —
/// see [`BarInset`].
///
/// `bar` is the already-composed widget pipeline — we ask it for a
/// `Vec<WidgetCell>` of the right width and copy that into a ratatui
/// [`Buffer`] of shape `cols × 1`. The buffer is then walked
/// left-to-right and emitted as CUP + per-cell glyphs. A hard SGR
/// reset (`\x1b[0m`) closes the row per the invariant in the module
/// header.
///
/// # Errors
///
/// Forwards any [`io::Error`] from `out`.
pub fn render_status_bar<W: Write>(
    out: &mut W,
    bar: &StatusBar,
    ctx: &StatusBarContext<'_>,
    row_index: u16,
    x: u16,
    cols: u16,
) -> io::Result<()> {
    // phux-4li.17: paint when there are configured widgets OR a window
    // bar to draw. An empty bar with no windows is still a no-op.
    if cols == 0 || (bar.is_empty() && ctx.windows.is_empty()) {
        return Ok(());
    }

    // 1. Compose widget cells (left/center/right slot policy lives in
    //    phux-config; we just consume the resulting strip).
    let row = bar.render(&ctx.as_widget(), cols);

    // 2. Lay into a ratatui Buffer of shape `cols × 1`, carrying each
    //    cell's style across the boundary. The Buffer's coordinate space
    //    is (0,0)..(cols,1); we never read it back.
    let mut buffer = Buffer::empty(Rect::new(0, 0, cols, 1));
    fill_buffer(&mut buffer, &row, cols);

    // 3. Emit the buffer to VT. Cursor hide for the duration of the
    //    paint; SGR reset on entry and exit so we don't inherit nor
    //    bequeath attributes. Cursor restore is the caller's job —
    //    see module header.
    write_buffer(out, &buffer, row_index, x, cols)
}

/// phux-9vf: the persistent error strip's style — reverse video + bold,
/// so the diagnostic reads as an alarm strip rather than blending into
/// normal chrome.
fn alarm_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
}

/// phux-i0e8.2.1: compose `message` into a fresh 1-row [`Buffer`] of
/// `cols` cells, every cell styled `style`, the message truncated to the
/// span and the remainder padded with spaces so the strip covers the
/// bar's full width.
///
/// Shared by the live full-row paint (`paint_full_row_message`) and the
/// snapshot compose (`compose_buffer`) for both the persistent error line
/// and the transient notice, so all four sites lay the row identically.
fn full_row_buffer(message: &str, style: Style, cols: u16) -> Buffer {
    let mut buffer = Buffer::empty(Rect::new(0, 0, cols, 1));
    let mut tmp = [0u8; 4];
    let mut col: u16 = 0;
    for ch in message.chars() {
        if col >= cols {
            break;
        }
        let cell = &mut buffer[(col, 0)];
        cell.set_symbol(ch.encode_utf8(&mut tmp));
        cell.set_style(style);
        col = col.saturating_add(1);
    }
    while col < cols {
        let cell = &mut buffer[(col, 0)];
        cell.set_symbol(" ");
        cell.set_style(style);
        col = col.saturating_add(1);
    }
    buffer
}

/// Copy a [`StatusBar`] composer row into a ratatui [`Buffer`].
///
/// Blank widget cells map to a literal ASCII space; non-blank cells
/// concatenate their grapheme codepoints into the buffer cell's
/// symbol. Styling is intentionally minimal: the widget contract
/// today only carries text. When the widget cell shape grows colors
/// (tracked in `phux-config`), grow it here too.
fn fill_buffer(buffer: &mut Buffer, row: &[WidgetCell], cols: u16) {
    let mut tmp = [0u8; 4];
    for (col, cell) in row.iter().enumerate().take(usize::from(cols)) {
        // `col < cols (u16)` from the `.take(usize::from(cols))` bound, so
        // the narrowing back to `u16` is provably lossless.
        let Ok(x) = u16::try_from(col) else {
            break;
        };
        let target: &mut RatatuiCell = &mut buffer[(x, 0)];
        if cell.text.is_empty() {
            target.set_symbol(" ");
        } else {
            // Base codepoint + any combining marks. We build the
            // grapheme into a small stack string and hand it to
            // set_symbol in one go (ratatui's Cell stores symbols as
            // CompactString so the heap stays cold for ASCII).
            let mut s = String::with_capacity(cell.text.len());
            for ch in &cell.text {
                s.push_str(ch.encode_utf8(&mut tmp));
            }
            target.set_symbol(&s);
        }
        // phux-ahv.3: carry per-cell style (fg/bg/attrs) across the
        // ratatui boundary; `write_buffer` emits it as SGR.
        if let Some(style) = &cell.style {
            target.set_style(to_ratatui_style(style));
        }
    }
}

/// Walk a ratatui [`Buffer`] left-to-right at `y=0` and emit raw VT
/// bytes for the row. Encoding: hide cursor, CUP to `(row_index, x + 1)`,
/// SGR reset, per-cell symbol, SGR reset, flush. The painter does
/// NOT show the cursor again — the caller restores it at the focused
/// pane's logical position.
///
/// The buffer is composed at its own origin (`0..cols`); `x` places it on
/// screen, so a sidebar-inset bar lands beside the strip (phux-qtw8).
fn write_buffer<W: Write>(
    out: &mut W,
    buffer: &Buffer,
    row_index: u16,
    x: u16,
    cols: u16,
) -> io::Result<()> {
    let one_based_row = row_index.saturating_add(1);
    let one_based_col = x.saturating_add(1);
    // CUP to the bar row + SGR reset. We deliberately do NOT hide the
    // cursor here: the bar paint completes in sub-ms on a modern
    // terminal, and the old `?25l`-without-guaranteed-`?25h` pattern
    // stranded the cursor invisible when the caller had no last_cursor
    // to restore (fresh attach, libghostty snapshot before first PTY
    // output). Caller still positions the cursor at the focused pane
    // after this returns.
    write!(out, "\x1b[{one_based_row};{one_based_col}H\x1b[0m")?;
    let mut prev_styled = false;
    for x in 0..cols {
        let cell = &buffer[(x, 0)];
        // phux-ahv.3: per-cell SGR (shared with the overlay painter).
        crate::render::sgr::emit_cell_sgr(out, cell, &mut prev_styled)?;
        let sym = cell.symbol();
        if sym.is_empty() {
            out.write_all(b" ")?;
        } else {
            out.write_all(sym.as_bytes())?;
        }
    }
    // SGR reset on exit so the next paint inherits no attributes from us.
    out.write_all(b"\x1b[0m")?;
    out.flush()
}

/// ADR-0033: emit the supervisory badge right-aligned on `row_index`, as a
/// reverse-video + bold chip atop the already-painted widget row. ASCII-only
/// (no emojis, per repo convention), so the char count is the cell width.
///
/// `x` is the bar's origin column; the chip right-aligns to the bar's own right
/// edge (`x + cols`), which a sidebar inset may pull in from the viewport's.
fn paint_supervisory_overlay<W: Write>(
    out: &mut W,
    badge: &str,
    row_index: u16,
    x: u16,
    cols: u16,
) -> io::Result<()> {
    if cols == 0 || badge.is_empty() {
        return Ok(());
    }
    let visible: String = badge.chars().take(cols as usize).collect();
    let width = u16::try_from(visible.chars().count()).unwrap_or(cols);
    let start_col = x.saturating_add(cols.saturating_sub(width));
    let one_based_row = row_index.saturating_add(1);
    let one_based_col = start_col.saturating_add(1);
    // CUP to the chip's left edge, reverse+bold, text, hard reset.
    write!(
        out,
        "\x1b[{one_based_row};{one_based_col}H\x1b[7;1m{visible}\x1b[0m"
    )?;
    out.flush()
}

/// phux-foz.1: emit the agent-attention hint as a chip immediately left of
/// the supervisory badge (`right_offset` cells in from the right edge; `0`
/// when no badge is present). Same reverse+bold treatment as the ADR-0033
/// badge, but the foreground rides the theme's `attention` slot — under
/// reverse video it reads as the chip's fill color.
fn paint_attention_overlay<W: Write>(
    out: &mut W,
    hint: &str,
    row_index: u16,
    x: u16,
    cols: u16,
    right_offset: u16,
    color: Color,
) -> io::Result<()> {
    let avail = cols.saturating_sub(right_offset);
    if avail == 0 || hint.is_empty() {
        return Ok(());
    }
    let visible: String = hint.chars().take(avail as usize).collect();
    let width = u16::try_from(visible.chars().count()).unwrap_or(avail);
    let start_col = x.saturating_add(avail.saturating_sub(width));
    let one_based_row = row_index.saturating_add(1);
    let one_based_col = start_col.saturating_add(1);
    write!(out, "\x1b[{one_based_row};{one_based_col}H\x1b[7;1m")?;
    crate::render::sgr::write_sgr_color(out, color, true)?;
    write!(out, "{visible}\x1b[0m")?;
    out.flush()
}

/// ADR-0033 / phux-foz.1: overlay a badge into a composed bar buffer (the
/// `phux snapshot --rendered` path), right-aligned `right_offset` cells in
/// from the right edge, so the dense-cell snapshot matches the live VT paint.
fn overlay_badge_into_buffer(
    buffer: &mut Buffer,
    badge: &str,
    cols: u16,
    right_offset: u16,
    style: Style,
) {
    let avail = cols.saturating_sub(right_offset);
    if avail == 0 || badge.is_empty() {
        return;
    }
    let visible: Vec<char> = badge.chars().take(avail as usize).collect();
    let width = u16::try_from(visible.len()).unwrap_or(avail);
    let start = avail.saturating_sub(width);
    let mut tmp = [0u8; 4];
    for (i, ch) in visible.iter().enumerate() {
        let x = start.saturating_add(u16::try_from(i).unwrap_or(0));
        if x >= avail {
            break;
        }
        let cell = &mut buffer[(x, 0)];
        cell.set_symbol(ch.encode_utf8(&mut tmp));
        cell.set_style(style);
    }
}

/// Columns the bar yields at each edge of the viewport (phux-qtw8).
///
/// A docked sidebar is a full-height strip, so the bar cannot span the full
/// width without painting its window tabs underneath it. The caller
/// (`attach::paint::bar_inset`) folds the strip's width into the matching side
/// and the painter renders into the residual span — exactly the columns panes
/// tile into. [`Self::NONE`] (no sidebar) is a full-width bar, byte-identical
/// to the pre-sidebar paint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BarInset {
    /// Columns yielded at the left edge.
    pub left: u16,
    /// Columns yielded at the right edge.
    pub right: u16,
}

impl BarInset {
    /// The full-width bar: no sidebar docked, nothing yielded.
    pub const NONE: Self = Self { left: 0, right: 0 };

    /// The bar's origin column and width within a `cols`-wide viewport.
    ///
    /// Saturating throughout: an inset wider than the viewport yields a
    /// zero-width bar (which every paint path treats as a no-op) rather than
    /// underflowing.
    #[must_use]
    pub const fn span(self, cols: u16) -> (u16, u16) {
        // `Ord::min` is not const for u16.
        let x = if self.left < cols { self.left } else { cols };
        let width = cols.saturating_sub(self.left).saturating_sub(self.right);
        (x, width)
    }
}

/// phux-i0e8.2.1: how long a transient [`Notice`] stays on the bar row.
///
/// After this elapses, [`StatusBarPainter::clear_expired_notice`] drops
/// the notice. Expiry rides the driver's existing 1 s `status_tick`, so
/// the effective lifetime is this value rounded up to the next tick.
pub const NOTICE_TTL: Duration = Duration::from_secs(7);

/// phux-i0e8.2.1: severity of a transient status-bar [`Notice`].
///
/// Picks the full-row style only; it carries no routing semantics. `Warn`
/// renders bold (matching the persistent error strip's weight), `Info`
/// renders plain reverse video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeSeverity {
    /// Informational lifecycle event (e.g. an input-lease handover).
    Info,
    /// Something is degraded and the user should know (e.g. a federation
    /// satellite became unreachable).
    Warn,
}

impl NoticeSeverity {
    /// Full-row style for a notice of this severity.
    fn style(self) -> Style {
        match self {
            Self::Info => Style::default().add_modifier(Modifier::REVERSED),
            Self::Warn => Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
        }
    }
}

/// phux-i0e8.2.1: a transient, self-expiring status-bar message.
///
/// Produced by the server-frame dispatcher (`FrameOutcome::notices`) for
/// lifecycle events that deserve a moment of visibility but no persistent
/// chrome — input-authority handovers, degraded federation, pane exits.
/// One slot, newest-wins: a fresh notice replaces the current one and
/// restarts the [`NOTICE_TTL`] clock. The persistent error line
/// ([`StatusBarPainter::error_line`]) always outranks it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Render severity; see [`NoticeSeverity`].
    pub severity: NoticeSeverity,
    /// The message painted across the bar row (truncated to the span).
    pub text: String,
}

impl Notice {
    /// An [`NoticeSeverity::Info`] notice.
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            severity: NoticeSeverity::Info,
            text: text.into(),
        }
    }

    /// A [`NoticeSeverity::Warn`] notice.
    pub fn warn(text: impl Into<String>) -> Self {
        Self {
            severity: NoticeSeverity::Warn,
            text: text.into(),
        }
    }
}

/// VT painter for a composed [`StatusBar`].
///
/// Thin stateful wrapper over [`render_status_bar`]: caches the last
/// rendered widget row so repeated paints with unchanged inputs are
/// no-ops, and tracks viewport dims so a resize invalidates the
/// cache. The cache lives here (not in `render_status_bar`) because
/// the function-level renderer is stateless and reusable.
pub struct StatusBarPainter {
    bar: StatusBar,
    position: Position,
    /// Last painted strip, keyed by the `(x, width)` span it was painted
    /// into. `None` ⇒ never painted (or the span changed); next call paints
    /// unconditionally. The origin is part of the key so a sidebar toggle
    /// repaints an otherwise byte-identical row into its new columns — and so
    /// [`Self::window_hit_at`] can map a screen column back onto the strip.
    last_row: Option<(u16, u16, Vec<WidgetCell>)>,
    /// Last (cols, rows) we painted into. Different dims invalidate
    /// `last_row` and force a fresh paint.
    last_viewport: Option<(u16, u16)>,
    /// phux-ahv.3: the window list fed to the `windows` widget. Updated
    /// by the driver from the `Workspace` and injected into the render
    /// context inside [`Self::paint`]; a change invalidates the cache.
    windows: Vec<WindowInfo>,
    /// phux-9vf: when `Some`, the painter ignores `bar`/`windows` and
    /// paints this fixed error line instead. Set by the attach path when
    /// the on-disk config fails to load or build, so the user sees a
    /// visible reason the bar and keybindings are degraded rather than a
    /// silently empty row.
    error: Option<String>,
    /// phux-i0e8.2.1: the transient notice slot and its expiry deadline.
    /// One slot, newest-wins; painted full-row (like the error strip, via
    /// the shared [`Self::paint_full_row_message`]) until
    /// [`Self::clear_expired_notice`] drops it. Never set while `error`
    /// is active (the persistent diagnostic outranks it) and never set on
    /// an empty bar (no row is reserved to paint it on — see
    /// [`Self::set_notice`]).
    notice: Option<(Notice, std::time::Instant)>,
    /// ADR-0033: when `Some`, a supervisory badge (e.g. `[ FROZEN ]`,
    /// `[ WHEEL:you ]`) is overlaid right-aligned on the bar row for the
    /// focused pane. Set by the driver from inbound `TerminalControl` state; a
    /// change invalidates the cache so the row repaints (and erases a cleared
    /// badge). Painted over the composed widget row, not replacing it.
    supervisory: Option<String>,
    /// phux-foz.1: when `Some`, the agent-attention hint (e.g. `[ ASK ]`)
    /// is overlaid immediately left of the supervisory badge. Set by the
    /// driver whenever a pane's ADR-0035 asked flag flips; same cache
    /// semantics as `supervisory`.
    attention: Option<String>,
    /// phux-foz.1: chip foreground for the attention hint, from the theme's
    /// `attention` slot (the painter never hardcodes it). Under the chip's
    /// reverse video the foreground reads as the fill color.
    attention_fg: Color,
    prefix: String,
    /// phux-foz.4: the focused pane's live working directory, fed by the
    /// driver from `cwd_changed` events (via the pane slots) and injected
    /// into the render context like `windows`. `None` ⇒ unknown (the
    /// `cwd` widget renders nothing).
    focused_cwd: Option<String>,
    /// phux-foz.4: the focused pane's last known command exit code, fed
    /// by the driver from `command_finished` events. `None` ⇒ unknown.
    last_exit: Option<i32>,
}

impl std::fmt::Debug for StatusBarPainter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatusBarPainter")
            .field("bar", &self.bar)
            .field("position", &self.position)
            .field(
                "last_row.len",
                &self.last_row.as_ref().map(|(_, _, r)| r.len()),
            )
            .field("last_viewport", &self.last_viewport)
            .field("windows.len", &self.windows.len())
            .field("error", &self.error)
            .field("notice", &self.notice)
            .field("supervisory", &self.supervisory)
            .field("attention", &self.attention)
            .field("attention_fg", &self.attention_fg)
            .field("prefix", &self.prefix)
            .field("focused_cwd", &self.focused_cwd)
            .field("last_exit", &self.last_exit)
            .finish()
    }
}

impl StatusBarPainter {
    /// Build a painter from an already-composed [`StatusBar`].
    #[must_use]
    pub fn new(bar: StatusBar, position: Position) -> Self {
        Self {
            bar,
            position,
            last_row: None,
            last_viewport: None,
            windows: Vec::new(),
            error: None,
            notice: None,
            supervisory: None,
            attention: None,
            attention_fg: Color::Reset,
            prefix: "C-a".to_owned(),
            focused_cwd: None,
            last_exit: None,
        }
    }

    /// phux-9vf: build a painter that shows a fixed error line instead of
    /// the configured widgets.
    ///
    /// The attach path reaches for this when the on-disk config fails to
    /// load or build: rather than silently dropping to an empty bar (and
    /// no keybindings) with only a `tracing::warn` the user never sees,
    /// the bar row shows the parse error and points at `phux config check`
    /// for the full diagnostic. The painter built this way is never
    /// "empty" and always reports a poll interval, so the error stays on
    /// screen across repaints.
    #[must_use]
    pub fn error_line(message: impl Into<String>) -> Self {
        Self {
            bar: StatusBar::empty(),
            position: Position::default(),
            last_row: None,
            last_viewport: None,
            windows: Vec::new(),
            error: Some(message.into()),
            notice: None,
            supervisory: None,
            attention: None,
            attention_fg: Color::Reset,
            prefix: "C-a".to_owned(),
            focused_cwd: None,
            last_exit: None,
        }
    }

    /// `true` when this painter was built by [`Self::error_line`] and is
    /// showing a fixed error strip instead of the configured widgets.
    /// The attach path checks this before layering a lesser diagnostic
    /// (e.g. a disabled keybinding, phux-i0e8.3.4) over a config-load
    /// error that already owns the bar row.
    #[must_use]
    pub const fn is_error_line(&self) -> bool {
        self.error.is_some()
    }

    /// Which row this painter reserves ([`Position::Bottom`] or
    /// [`Position::Top`]). The paint/layout helpers read this so the pane
    /// content rect and the bar row agree on the reservation (phux-foz.8).
    #[must_use]
    pub const fn position(&self) -> Position {
        self.position
    }

    /// Set the configured prefix chord exposed to prefix-aware widgets.
    pub fn set_prefix(&mut self, prefix: impl Into<String>) {
        let prefix = prefix.into();
        if self.prefix != prefix {
            self.prefix = prefix;
            self.invalidate();
        }
    }

    /// Update the window list rendered by the `windows` widget. A change
    /// forces the next paint to redraw (the list isn't part of the
    /// widget-row cache key — the widget reads it from the context).
    /// Returns `true` if the list actually changed (so a caller with no
    /// other paint trigger can gate a repaint on it).
    pub fn set_windows(&mut self, windows: Vec<WindowInfo>) -> bool {
        if self.windows == windows {
            return false;
        }
        self.windows = windows;
        self.invalidate();
        true
    }

    /// phux-foz.4: set (or clear, with `None`) the focused pane's live
    /// working directory rendered by the `cwd` widget. Returns `true` if
    /// the value actually changed; a change invalidates the row cache.
    pub fn set_focused_cwd(&mut self, cwd: Option<String>) -> bool {
        if self.focused_cwd == cwd {
            return false;
        }
        self.focused_cwd = cwd;
        self.invalidate();
        true
    }

    /// phux-foz.4: set (or clear, with `None`) the focused pane's last
    /// command exit code rendered by the `exit` widget. Returns `true` if
    /// the value actually changed; a change invalidates the row cache.
    pub fn set_last_exit(&mut self, last_exit: Option<i32>) -> bool {
        if self.last_exit == last_exit {
            return false;
        }
        self.last_exit = last_exit;
        self.invalidate();
        true
    }

    /// phux-i0e8.2.1: show a transient notice full-row on the bar for
    /// [`NOTICE_TTL`] from `now`. Newest-wins: a fresh notice replaces the
    /// current one (and restarts the clock). Returns `true` when the notice
    /// was accepted (the caller should repaint the bar).
    ///
    /// Refused — degrading to a `tracing` line, so the event is never
    /// entirely silent — in two cases:
    ///
    /// - the persistent error line is active (phux-9vf): the fixed
    ///   diagnostic outranks any transient message;
    /// - the configured bar is empty: an empty-bar painter never reserves
    ///   a row ([`Self::is_empty`] / [`Self::min_poll_interval`] stay
    ///   unaffected by notices), so there is no row to paint the notice on
    ///   and no tick to expire it. This is a documented limitation (see
    ///   `docs/consumers/tui.md` §8.7).
    pub fn set_notice(&mut self, notice: Notice, now: std::time::Instant) -> bool {
        if self.error.is_some() {
            tracing::info!(
                severity = ?notice.severity,
                text = %notice.text,
                "status-bar notice suppressed under the persistent error line",
            );
            return false;
        }
        if self.bar.is_empty() {
            tracing::info!(
                severity = ?notice.severity,
                text = %notice.text,
                "status-bar notice dropped: no bar row is reserved (empty [status] config)",
            );
            return false;
        }
        self.notice = Some((notice, now + NOTICE_TTL));
        self.invalidate();
        true
    }

    /// phux-i0e8.2.1: drop the notice once its deadline passes. Called from
    /// the driver's existing 1 s `status_tick`; returns `true` when the
    /// notice was cleared (the cache is invalidated, so the next paint
    /// restores the normal widget row).
    pub fn clear_expired_notice(&mut self, now: std::time::Instant) -> bool {
        match &self.notice {
            Some((_, deadline)) if now >= *deadline => {
                self.notice = None;
                self.invalidate();
                true
            }
            _ => false,
        }
    }

    /// ADR-0033: set (or clear, with `None`) the supervisory badge overlaid on
    /// the bar for the focused pane. Returns `true` if the badge actually
    /// changed (so the caller can gate a repaint on it). A change invalidates
    /// the cache so the row repaints — which also erases a badge that just
    /// cleared.
    ///
    /// No-op while an error line is showing: the badge rides the normal bar and
    /// is suppressed under the error strip, so storing it (and invalidating the
    /// error-line cache) would only force a spurious error-strip re-emit.
    pub fn set_supervisory(&mut self, badge: Option<String>) -> bool {
        if self.error.is_some() || self.supervisory == badge {
            return false;
        }
        self.supervisory = badge;
        self.invalidate();
        true
    }

    /// phux-foz.1: set (or clear, with `None`) the agent-attention hint
    /// overlaid left of the supervisory badge. Returns `true` if the hint
    /// actually changed; same error-line suppression and cache semantics as
    /// [`Self::set_supervisory`].
    pub fn set_attention(&mut self, hint: Option<String>) -> bool {
        if self.error.is_some() || self.attention == hint {
            return false;
        }
        self.attention = hint;
        self.invalidate();
        true
    }

    /// phux-foz.1: set the attention chip's foreground from the theme's
    /// `attention` slot. The driver calls this once at attach; the painter
    /// itself never hardcodes the color.
    pub fn set_attention_color(&mut self, color: Color) {
        if self.attention_fg != color {
            self.attention_fg = color;
            self.invalidate();
        }
    }

    /// Cells the attention chip is shifted in from the right edge: the
    /// supervisory badge's width plus a 1-cell gap, or `0` when no badge is
    /// showing. Shared by the live paint and the snapshot compose so both
    /// place the chip identically.
    fn attention_offset(&self) -> u16 {
        self.supervisory.as_ref().map_or(0, |badge| {
            u16::try_from(badge.chars().count())
                .unwrap_or(u16::MAX)
                .saturating_add(1)
        })
    }

    /// True if the underlying bar has no widgets configured.
    ///
    /// phux-9vf: an error-line painter is never empty — the fixed
    /// diagnostic must always reserve and paint its row so the user sees
    /// why their chrome is degraded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.error.is_none() && self.bar.is_empty()
    }

    /// Tightest poll interval among the bar's widgets. `None` ⇒ no
    /// time-based repaint is needed. v0 returns a conservative
    /// `Some(1s)` when the bar isn't empty so the `time` widget
    /// refreshes at its declared cadence.
    ///
    /// phux-9vf: an error-line painter reports the same `Some(1s)` so the
    /// driver's `status_tick` arm keeps repainting it and the diagnostic
    /// survives pane output stomping the bottom row.
    #[must_use]
    pub fn min_poll_interval(&self) -> Option<Duration> {
        if self.is_empty() {
            None
        } else {
            Some(Duration::from_secs(1))
        }
    }

    /// Paint the bar onto `out` for a viewport of `cols × rows`, spanning the
    /// columns `inset` leaves it ([`BarInset::NONE`] ⇒ the full width).
    ///
    /// Cheap to call repeatedly: identical widget output + unchanged
    /// dims is a no-op (zero bytes written). Dimension changes force
    /// a fresh paint.
    ///
    /// # Errors
    ///
    /// Forwards any [`io::Error`] from `out`.
    pub fn paint<W: Write>(
        &mut self,
        out: &mut W,
        inset: BarInset,
        cols: u16,
        rows: u16,
        ctx: &StatusBarContext<'_>,
    ) -> io::Result<()> {
        if cols == 0 || rows == 0 {
            return Ok(());
        }
        // phux-qtw8: the bar yields the sidebar's columns. Everything below
        // composes and hit-tests against this span, not the viewport — an
        // inset wider than the terminal leaves nothing to paint.
        let (x, cols) = inset.span(cols);
        if cols == 0 {
            return Ok(());
        }
        // phux-9vf: an error-line painter bypasses the widget pipeline and
        // paints the fixed diagnostic. It takes priority over the normal
        // "empty bar with no windows is a no-op" short-circuit below.
        if self.error.is_some() {
            return self.paint_error_line(out, x, cols, rows);
        }
        // phux-i0e8.2.1: an active transient notice takes the row over from
        // the widget pipeline until it expires. Full-row like the error
        // strip; only reachable on a non-empty bar (see `set_notice`).
        if let Some((notice, _)) = self.notice.clone() {
            return self.paint_full_row_message(
                out,
                &notice.text,
                notice.severity.style(),
                x,
                cols,
                rows,
            );
        }
        // The supervisory badge rides the normal bar row, so it only paints
        // when there is a bar to host it. An empty configured bar with no
        // windows stays a no-op (the badge is suppressed rather than ghosting
        // over un-erased pane content on a row the bar never blanks).
        if self.bar.is_empty() && self.windows.is_empty() {
            return Ok(());
        }
        // The window list is owned by the painter (the driver sets it
        // from the Workspace); inject it into the render context so
        // callers don't have to thread it through every paint path.
        // Injected BEFORE the cache compose (phux-foz.12) so `last_row`
        // holds the strip actually painted — window tabs included — and
        // [`Self::window_hit_at`] hit-tests against what is on screen.
        let ctx = StatusBarContext {
            prefix: &self.prefix,
            windows: &self.windows,
            cwd: self.focused_cwd.as_deref().unwrap_or(""),
            last_exit: self.last_exit,
            ..*ctx
        };
        let new_row = self.bar.render(&ctx.as_widget(), cols);
        let viewport_changed = self.last_viewport != Some((cols, rows));
        // The origin is part of the key: toggling a sidebar can leave the
        // composed row byte-identical while moving the columns it belongs in.
        let row_changed = match &self.last_row {
            Some((prev_x, w, prev)) => *prev_x != x || *w != cols || prev != &new_row,
            None => true,
        };
        if !viewport_changed && !row_changed {
            return Ok(());
        }
        let row_index: u16 = match self.position {
            Position::Bottom => rows.saturating_sub(1),
            Position::Top => 0,
        };
        // Delegate to the ratatui-backed renderer. We pre-composed
        // `new_row` for cache-keying; the renderer recomposes — cheap
        // (same inputs, deterministic) and keeps `render_status_bar`
        // usable standalone in tests.
        render_status_bar(out, &self.bar, &ctx, row_index, x, cols)?;
        // ADR-0033: overlay the supervisory badge atop the freshly-painted
        // widget row (right-aligned). Emitted after the row so it wins; the
        // full-row repaint above erases any stale/cleared badge first.
        if let Some(badge) = &self.supervisory {
            paint_supervisory_overlay(out, badge, row_index, x, cols)?;
        }
        // phux-foz.1: the attention hint chips in immediately left of the
        // badge (or at the right edge when no badge is up). Same repaint
        // discipline: the full-row repaint erased any cleared hint.
        if let Some(hint) = &self.attention {
            paint_attention_overlay(
                out,
                hint,
                row_index,
                x,
                cols,
                self.attention_offset(),
                self.attention_fg,
            )?;
        }
        self.last_row = Some((x, cols, new_row));
        self.last_viewport = Some((cols, rows));
        Ok(())
    }

    /// Compose the status row into a fresh ratatui [`Buffer`] the width of the
    /// bar's `inset` span, without emitting VT or touching the paint cache
    /// (`phux-l5xa`).
    ///
    /// Returns `(buffer, x, row_index)` — the buffer's origin column and row in
    /// a `cols × rows` viewport — or `None` when nothing would paint (zero
    /// dims, an inset that leaves no columns, or an empty bar with no windows
    /// and no error). Mirrors the composition in [`Self::paint`] /
    /// [`Self::paint_error_line`] so the `phux snapshot --rendered` frame shows
    /// the same bar the live VT paint would — read as dense cells, with no
    /// emulator re-parse.
    pub(crate) fn compose_buffer(
        &self,
        inset: BarInset,
        cols: u16,
        rows: u16,
        ctx: &StatusBarContext<'_>,
    ) -> Option<(Buffer, u16, u16)> {
        if cols == 0 || rows == 0 {
            return None;
        }
        let (x, cols) = inset.span(cols);
        if cols == 0 {
            return None;
        }
        let row_index: u16 = match self.position {
            Position::Bottom => rows.saturating_sub(1),
            Position::Top => 0,
        };
        if let Some(message) = &self.error {
            return Some((full_row_buffer(message, alarm_style(), cols), x, row_index));
        }
        // phux-i0e8.2.1: mirror the live paint — an active transient notice
        // composes full-row into the snapshot, styled by severity.
        if let Some((notice, _)) = &self.notice {
            return Some((
                full_row_buffer(&notice.text, notice.severity.style(), cols),
                x,
                row_index,
            ));
        }
        // Match `paint`: the badge only composes onto a non-empty bar row.
        if self.bar.is_empty() && self.windows.is_empty() {
            return None;
        }
        let ctx = StatusBarContext {
            prefix: &self.prefix,
            windows: &self.windows,
            cwd: self.focused_cwd.as_deref().unwrap_or(""),
            last_exit: self.last_exit,
            ..*ctx
        };
        let row = self.bar.render(&ctx.as_widget(), cols);
        let mut buffer = Buffer::empty(Rect::new(0, 0, cols, 1));
        fill_buffer(&mut buffer, &row, cols);
        // ADR-0033: overlay the supervisory badge into the snapshot buffer so
        // `phux snapshot --rendered` shows the same chip the live paint draws.
        if let Some(badge) = &self.supervisory {
            overlay_badge_into_buffer(
                &mut buffer,
                badge,
                cols,
                0,
                Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            );
        }
        // phux-foz.1: the attention hint composes left of the badge, themed
        // like the live paint.
        if let Some(hint) = &self.attention {
            overlay_badge_into_buffer(
                &mut buffer,
                hint,
                cols,
                self.attention_offset(),
                Style::default()
                    .fg(self.attention_fg)
                    .add_modifier(Modifier::REVERSED | Modifier::BOLD),
            );
        }
        Some((buffer, x, row_index))
    }

    /// phux-9vf: paint the fixed error diagnostic onto the bar row.
    ///
    /// Bypasses the widget composer entirely: the message is laid into a
    /// reverse-video row (so it reads as an alarm strip rather than blending
    /// into normal chrome) and truncated to `cols`. Delegates to the shared
    /// [`Self::paint_full_row_message`] (phux-i0e8.2.1), which the transient
    /// notice path also rides.
    fn paint_error_line<W: Write>(
        &mut self,
        out: &mut W,
        x: u16,
        cols: u16,
        rows: u16,
    ) -> io::Result<()> {
        // Callers gate on `self.error.is_some()`; an empty string is a
        // valid (if unusual) diagnostic, so default to "" rather than
        // returning early.
        let message = self.error.clone().unwrap_or_default();
        self.paint_full_row_message(out, &message, alarm_style(), x, cols, rows)
    }

    /// phux-i0e8.2.1: paint `message` full-row onto the bar row in `style`,
    /// truncated to `cols` and padded to the span's full width.
    ///
    /// Shared by the persistent error line and the transient notice. Cached
    /// on `last_row` / `last_viewport` like the normal path so repeated
    /// paints with unchanged dims are no-ops; the cache is keyed on the span
    /// only — every message change goes through a setter that calls
    /// [`Self::invalidate`] (the error message is fixed for the painter's
    /// lifetime; a notice change replaces the slot via [`Self::set_notice`]).
    fn paint_full_row_message<W: Write>(
        &mut self,
        out: &mut W,
        message: &str,
        style: Style,
        x: u16,
        cols: u16,
        rows: u16,
    ) -> io::Result<()> {
        let viewport_changed = self.last_viewport != Some((cols, rows));
        let moved = self
            .last_row
            .as_ref()
            .is_some_and(|(px, pw, _)| *px != x || *pw != cols);
        if !viewport_changed && !moved && self.last_row.is_some() {
            return Ok(());
        }
        let row_index: u16 = match self.position {
            Position::Bottom => rows.saturating_sub(1),
            Position::Top => 0,
        };
        let buffer = full_row_buffer(message, style, cols);
        write_buffer(out, &buffer, row_index, x, cols)?;
        // Mark the cache populated so the span-only key short-circuits the
        // next repaint; the stored row is empty (we don't compose widgets).
        self.last_row = Some((x, cols, Vec::new()));
        self.last_viewport = Some((cols, rows));
        Ok(())
    }

    /// phux-r82.6: the async data feeds behind the bar's `exec` widgets.
    /// The driver spawns one bounded interval runner per feed; an
    /// error-line painter (empty bar) has none.
    #[must_use]
    pub fn exec_feeds(&self) -> Vec<phux_config::widget::ExecFeed> {
        self.bar.exec_feeds()
    }

    /// Force the next [`Self::paint`] to redraw unconditionally —
    /// e.g. after a SIGWINCH or after the pane renderer wrote the
    /// bottom row.
    pub fn invalidate(&mut self) {
        self.last_row = None;
        self.last_viewport = None;
    }

    /// phux-foz.12: resolve a click column on the bar row to the window
    /// tab painted there, reading the strip cached by the last
    /// [`Self::paint`] — so hit targets derive from exactly what is on
    /// screen and cannot drift from the composed layout (slot placement,
    /// separators, truncation, `Z`/`!` markers all included).
    ///
    /// `x` is a screen column; a sidebar-inset bar (phux-qtw8) is painted from
    /// its own origin, so the cached origin is subtracted to index the strip.
    ///
    /// `None` when the bar has never painted, `x` is off the strip (left of its
    /// origin, or past its end), or the cell under `x` is not a window tab (a
    /// separator, another widget, blank padding, or the error line — whose
    /// cached row is empty).
    #[must_use]
    pub fn window_hit_at(&self, x: u16) -> Option<usize> {
        match self.hit_at(x)? {
            phux_config::widget::CellHit::Window(i) => Some(i),
            phux_config::widget::CellHit::Switch => None,
        }
    }

    /// The interactive target under screen column `x` on the bar row, or
    /// `None` when the cell is inert.
    ///
    /// Resolved against the cached strip [`Self::paint`] last emitted, so
    /// hit targets are exactly the cells on screen — slot placement,
    /// separators, responsive tab dropping and widget visibility gating
    /// all included. There is no second layout to keep in step.
    #[must_use]
    pub fn hit_at(&self, x: u16) -> Option<phux_config::widget::CellHit> {
        let (origin, _, row) = self.last_row.as_ref()?;
        let col = x.checked_sub(*origin)?;
        row.get(usize::from(col))?.hit
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use phux_config::widget::WidgetRegistry;
    use phux_config::{StatusCfg, Widget, WidgetSpec};
    use std::time::UNIX_EPOCH;

    fn ctx_default(session: &str) -> StatusBarContext<'_> {
        make_context(session, UNIX_EPOCH)
    }

    fn spec(kind: &str, opts: &[(&str, toml::Value)]) -> Widget {
        Widget::Spec(WidgetSpec {
            kind: kind.to_owned(),
            opts: opts
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        })
    }

    fn build_bar(cfg: &StatusCfg) -> StatusBar {
        let reg = WidgetRegistry::with_builtins();
        StatusBar::build(cfg, &reg).unwrap()
    }

    #[test]
    fn empty_bar_writes_nothing() {
        let cfg = StatusCfg::default();
        let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        assert!(p.is_empty());
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 80, 24, &ctx_default(""))
            .unwrap();
        assert!(buf.is_empty());
    }

    #[test]
    fn bottom_position_targets_last_row() {
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..Default::default()
        };
        let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 10, 24, &ctx_default("hi"))
            .unwrap();
        let s = String::from_utf8_lossy(&buf);
        // Row 24 (last of 24-row viewport).
        assert!(s.contains("\x1b[24;1H"), "no CUP-to-row-24: {s:?}");
        assert!(s.contains("hi"), "missing widget text: {s:?}");
    }

    #[test]
    fn top_position_targets_row_1() {
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..Default::default()
        };
        let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Top);
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 10, 24, &ctx_default("hi"))
            .unwrap();
        let s = String::from_utf8_lossy(&buf);
        assert!(s.contains("\x1b[1;1H"), "no CUP-to-row-1: {s:?}");
    }

    /// phux-foz.8: the `[status] position` config value maps 1:1 onto the
    /// render enum, and the painter reports it back through `position()`
    /// (the layout helpers key the content-rect shift off that getter).
    #[test]
    fn config_position_maps_onto_render_position() {
        assert_eq!(
            Position::from(phux_config::StatusPosition::Bottom),
            Position::Bottom
        );
        assert_eq!(
            Position::from(phux_config::StatusPosition::Top),
            Position::Top
        );
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..Default::default()
        };
        let p = StatusBarPainter::new(build_bar(&cfg), Position::Top);
        assert_eq!(p.position(), Position::Top);
    }

    #[test]
    fn paint_is_idempotent_on_unchanged_row() {
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..Default::default()
        };
        let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 10, 24, &ctx_default("x"))
            .unwrap();
        let first_len = buf.len();
        // Second paint with same dims + same ctx must add nothing.
        p.paint(&mut buf, BarInset::NONE, 10, 24, &ctx_default("x"))
            .unwrap();
        assert_eq!(buf.len(), first_len);
    }

    #[test]
    fn paint_redraws_on_viewport_change() {
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..Default::default()
        };
        let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 10, 24, &ctx_default("x"))
            .unwrap();
        let first_len = buf.len();
        // Change width — must repaint.
        p.paint(&mut buf, BarInset::NONE, 20, 24, &ctx_default("x"))
            .unwrap();
        assert!(buf.len() > first_len);
    }

    #[test]
    fn paint_redraws_on_context_change() {
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..Default::default()
        };
        let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 10, 24, &ctx_default("a"))
            .unwrap();
        let first_len = buf.len();
        p.paint(&mut buf, BarInset::NONE, 10, 24, &ctx_default("b"))
            .unwrap();
        assert!(buf.len() > first_len);
    }

    #[test]
    fn zero_dims_skip_paint() {
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..Default::default()
        };
        let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 0, 24, &ctx_default("x"))
            .unwrap();
        p.paint(&mut buf, BarInset::NONE, 80, 0, &ctx_default("x"))
            .unwrap();
        assert!(buf.is_empty());
    }

    #[test]
    fn time_and_session_both_appear_when_configured() {
        // Integration scenario from the nz4.5 acceptance criteria.
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            right: vec![spec(
                "time",
                &[("format", toml::Value::String("LITERAL".into()))],
            )],
            ..Default::default()
        };
        let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 30, 24, &ctx_default("main"))
            .unwrap();
        let s = String::from_utf8_lossy(&buf);
        assert!(s.contains("main"), "session widget missing: {s:?}");
        assert!(s.contains("LITERAL"), "time widget missing: {s:?}");
    }

    #[test]
    fn error_line_painter_is_not_empty_and_polls() {
        // phux-9vf: an error-line painter must report non-empty + a poll
        // interval so the driver reserves the row and keeps repainting the
        // diagnostic (otherwise pane output stomps it and it never returns).
        let p = StatusBarPainter::error_line("config error: boom (run: phux config check)");
        assert!(!p.is_empty(), "error-line painter must not be empty");
        assert_eq!(p.min_poll_interval(), Some(Duration::from_secs(1)));
    }

    #[test]
    fn error_line_painter_renders_message_on_bar_row() {
        // phux-9vf: the fixed diagnostic paints onto the bar row even though
        // no widgets are configured (the normal empty-bar short-circuit
        // would otherwise emit nothing).
        let mut p =
            StatusBarPainter::error_line("config error: dup [status] (run: phux config check)");
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 80, 24, &ctx_default(""))
            .unwrap();
        let s = String::from_utf8_lossy(&buf);
        assert!(s.contains("\x1b[24;1H"), "no CUP-to-bar-row: {s:?}");
        // The painter emits one SGR-wrapped cell per glyph, so the message
        // is not a contiguous substring of the raw VT. Strip the CSI escapes
        // to recover the printable text and assert on that.
        let printable = strip_csi(&s);
        assert!(
            printable.contains("config error"),
            "missing error text: {printable:?} (raw {s:?})"
        );
        assert!(
            printable.contains("phux config check"),
            "missing call-to-action: {printable:?} (raw {s:?})"
        );
    }

    #[test]
    fn error_line_painter_repaint_is_idempotent_on_unchanged_dims() {
        let mut p = StatusBarPainter::error_line("config error: boom");
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 24, &ctx_default(""))
            .unwrap();
        let first_len = buf.len();
        assert!(first_len > 0, "first paint must emit the error row");
        p.paint(&mut buf, BarInset::NONE, 40, 24, &ctx_default(""))
            .unwrap();
        assert_eq!(buf.len(), first_len, "unchanged dims must be a no-op");
    }

    /// phux-i0e8.2.1: an accepted notice takes the row over full-row, and a
    /// newer notice replaces it (newest-wins single slot).
    #[test]
    fn notice_paints_full_row_and_newest_wins() {
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..Default::default()
        };
        let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        let now = std::time::Instant::now();
        assert!(p.set_notice(Notice::info("first notice"), now));
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 24, &ctx_default("sess"))
            .unwrap();
        let printable = strip_csi(&String::from_utf8_lossy(&buf));
        assert!(
            printable.contains("first notice"),
            "notice must paint the bar row: {printable:?}"
        );
        assert!(
            !printable.contains("sess"),
            "the notice takes the full row over from the widgets: {printable:?}"
        );
        assert!(p.set_notice(Notice::warn("second notice"), now));
        buf.clear();
        p.paint(&mut buf, BarInset::NONE, 40, 24, &ctx_default("sess"))
            .unwrap();
        let printable = strip_csi(&String::from_utf8_lossy(&buf));
        assert!(
            printable.contains("second notice") && !printable.contains("first notice"),
            "newest notice must win the slot: {printable:?}"
        );
    }

    /// phux-i0e8.2.1: the persistent error line outranks a transient
    /// notice — `set_notice` is refused and the diagnostic keeps the row.
    #[test]
    fn notice_is_suppressed_under_the_error_line() {
        let mut p = StatusBarPainter::error_line("config error: boom");
        assert!(
            !p.set_notice(
                Notice::warn("pane 3: exited 137"),
                std::time::Instant::now()
            ),
            "a notice must be refused while the error line is active"
        );
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 24, &ctx_default(""))
            .unwrap();
        let printable = strip_csi(&String::from_utf8_lossy(&buf));
        assert!(
            printable.contains("config error") && !printable.contains("exited 137"),
            "the error diagnostic must keep the row: {printable:?}"
        );
    }

    /// phux-i0e8.2.1: a notice expires after [`NOTICE_TTL`]; the clear
    /// invalidates the cache so the next paint restores the widget row.
    #[test]
    fn notice_expires_after_ttl_and_restores_the_widget_row() {
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..Default::default()
        };
        let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        let now = std::time::Instant::now();
        assert!(p.set_notice(Notice::info("input: wheel released"), now));
        assert!(
            !p.clear_expired_notice(now + Duration::from_secs(6)),
            "a notice must survive until its TTL elapses"
        );
        assert!(
            p.clear_expired_notice(now + NOTICE_TTL),
            "the notice must clear once the TTL elapses"
        );
        assert!(
            !p.clear_expired_notice(now + NOTICE_TTL),
            "a second clear is a no-op (slot already empty)"
        );
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 24, &ctx_default("sess"))
            .unwrap();
        let printable = strip_csi(&String::from_utf8_lossy(&buf));
        assert!(
            printable.contains("sess") && !printable.contains("wheel released"),
            "the widget row must return after expiry: {printable:?}"
        );
    }

    /// phux-i0e8.2.1: an empty-bar painter never reserves a row for a
    /// notice — the notice is refused (degrading to tracing), the painter
    /// stays empty (no row reservation), and no poll interval appears.
    #[test]
    fn notice_on_an_empty_bar_never_reserves_a_row() {
        let mut p = StatusBarPainter::new(build_bar(&StatusCfg::default()), Position::Bottom);
        assert!(p.is_empty(), "precondition: empty configured bar");
        assert!(
            !p.set_notice(
                Notice::warn("federation degraded"),
                std::time::Instant::now()
            ),
            "a notice on an empty bar must be refused"
        );
        assert!(p.is_empty(), "a refused notice must not un-empty the bar");
        assert_eq!(
            p.min_poll_interval(),
            None,
            "no poll interval may appear (no row is reserved, no tick to expire on)"
        );
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 80, 24, &ctx_default(""))
            .unwrap();
        assert!(buf.is_empty(), "nothing may paint on an empty bar");
    }

    #[test]
    fn error_line_painter_repaints_after_invalidate() {
        // The driver invalidates the bar after pane output overwrites the
        // bottom row; the diagnostic must then repaint.
        let mut p = StatusBarPainter::error_line("config error: boom");
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 24, &ctx_default(""))
            .unwrap();
        let first_len = buf.len();
        p.invalidate();
        p.paint(&mut buf, BarInset::NONE, 40, 24, &ctx_default(""))
            .unwrap();
        assert!(buf.len() > first_len, "invalidate must force a repaint");
    }

    #[test]
    fn min_poll_interval_some_when_non_empty() {
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..Default::default()
        };
        let p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        assert_eq!(p.min_poll_interval(), Some(Duration::from_secs(1)));
    }

    #[test]
    fn min_poll_interval_none_when_empty() {
        let cfg = StatusCfg::default();
        let p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        assert_eq!(p.min_poll_interval(), None);
    }

    #[test]
    fn invalidate_forces_repaint() {
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..Default::default()
        };
        let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 10, 24, &ctx_default("x"))
            .unwrap();
        let first_len = buf.len();
        p.invalidate();
        p.paint(&mut buf, BarInset::NONE, 10, 24, &ctx_default("x"))
            .unwrap();
        assert!(buf.len() > first_len);
    }

    #[test]
    fn make_context_helper_exposes_session_and_now() {
        let now = UNIX_EPOCH;
        let c = make_context("alpha", now);
        assert_eq!(c.session_name, "alpha");
        assert_eq!(c.now, now);
    }

    fn windows_bar() -> StatusBar {
        let cfg = StatusCfg {
            left: vec![spec("windows", &[])],
            ..StatusCfg::default()
        };
        build_bar(&cfg)
    }

    /// Strip CSI escape sequences so a text assertion isn't defeated by
    /// the per-cell SGR that styled tabs interleave between glyphs.
    fn strip_csi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' && chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            } else if c != '\x1b' {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn windows_widget_renders_tab_strip() {
        let bar = windows_bar();
        let windows = [
            WindowInfo {
                name: "bash".to_owned(),
                active: true,
                zoomed: false,
                attention: false,
                branch: None,
            },
            WindowInfo {
                name: "vim".to_owned(),
                active: false,
                zoomed: false,
                attention: false,
                branch: None,
            },
        ];
        let ctx = StatusBarContext {
            windows: &windows,
            ..make_context("", UNIX_EPOCH)
        };
        let mut buf = Vec::new();
        render_status_bar(&mut buf, &bar, &ctx, 0, 0, 40).unwrap();
        let s = String::from_utf8(buf).unwrap();
        // The active tab carries an SGR (default preset = bold+reverse),
        // so glyphs interleave with escapes — strip CSI before the text
        // assertion.
        assert!(
            s.contains("\x1b[1"),
            "expected bold SGR for the active tab; got {s:?}"
        );
        let visible = strip_csi(&s);
        assert!(visible.contains("0:bash"), "first tab; got {visible:?}");
        assert!(visible.contains("1:vim"), "second tab; got {visible:?}");
    }

    #[test]
    fn empty_bar_and_no_windows_is_noop() {
        let bar = build_bar(&StatusCfg::default());
        let ctx = make_context("", UNIX_EPOCH);
        let mut buf = Vec::new();
        render_status_bar(&mut buf, &bar, &ctx, 0, 0, 40).unwrap();
        assert!(buf.is_empty(), "empty bar + no windows must not paint");
    }

    #[test]
    fn painter_set_windows_paints_tab_strip() {
        // A painter whose bar has the `windows` widget renders the strip
        // from its injected window list; a changed list forces a repaint.
        let mut p = StatusBarPainter::new(windows_bar(), Position::Bottom);
        p.set_windows(vec![WindowInfo {
            name: "a".to_owned(),
            active: true,
            zoomed: false,
            attention: false,
            branch: None,
        }]);
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 10, &ctx_default(""))
            .unwrap();
        let s = strip_csi(&String::from_utf8(buf).unwrap());
        assert!(
            s.contains("0:a"),
            "painter should render the strip; got {s:?}"
        );
    }

    /// phux-foz.1: the attention hint paints as a right-aligned chip on the
    /// bar row, colored by the theme-fed `attention_fg` (reverse video makes
    /// the fg the chip fill).
    #[test]
    fn painter_paints_attention_hint_right_aligned() {
        let mut p = StatusBarPainter::new(windows_bar(), Position::Bottom);
        p.set_windows(vec![WindowInfo {
            name: "a".to_owned(),
            active: true,
            zoomed: false,
            attention: false,
            branch: None,
        }]);
        p.set_attention_color(Color::Rgb(251, 191, 36));
        assert!(p.set_attention(Some("[ ASK ]".to_owned())));
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 10, &ctx_default(""))
            .unwrap();
        let s = String::from_utf8(buf).unwrap();
        // Right-aligned: "[ ASK ]" is 7 cells wide in a 40-col bar on the
        // bottom row (row 10) => CUP col 34.
        assert!(
            s.contains("\x1b[10;34H"),
            "attention chip must right-align; got {s:?}"
        );
        assert!(
            s.contains("\x1b[38;2;251;191;36m"),
            "chip must carry the themed attention color; got {s:?}"
        );
        assert!(strip_csi(&s).contains("[ ASK ]"), "chip text; got {s:?}");
    }

    /// phux-foz.1: with a supervisory badge up, the attention chip shifts
    /// left of it (badge width + 1-cell gap) instead of overpainting it.
    #[test]
    fn attention_hint_sits_left_of_the_supervisory_badge() {
        let mut p = StatusBarPainter::new(windows_bar(), Position::Bottom);
        p.set_windows(vec![WindowInfo {
            name: "a".to_owned(),
            active: true,
            zoomed: false,
            attention: false,
            branch: None,
        }]);
        assert!(p.set_supervisory(Some("[ FROZEN ]".to_owned())));
        assert!(p.set_attention(Some("[ ASK ]".to_owned())));
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 10, &ctx_default(""))
            .unwrap();
        let s = String::from_utf8(buf).unwrap();
        // Badge: 10 cells at cols 31..40. Chip: offset 11 from the right
        // edge => right-aligned at col 40-11-7+1 = 23.
        assert!(
            s.contains("\x1b[10;31H"),
            "badge keeps the right edge; got {s:?}"
        );
        assert!(
            s.contains("\x1b[10;23H"),
            "chip must shift left of the badge; got {s:?}"
        );
        let visible = strip_csi(&s);
        assert!(visible.contains("[ ASK ]") && visible.contains("[ FROZEN ]"));
    }

    /// phux-foz.1: clearing the hint reports the change and the repainted
    /// row no longer carries it.
    #[test]
    fn cleared_attention_hint_stops_painting() {
        let mut p = StatusBarPainter::new(windows_bar(), Position::Bottom);
        p.set_windows(vec![WindowInfo {
            name: "a".to_owned(),
            active: true,
            zoomed: false,
            attention: false,
            branch: None,
        }]);
        assert!(p.set_attention(Some("[ ASK ]".to_owned())));
        assert!(
            !p.set_attention(Some("[ ASK ]".to_owned())),
            "unchanged hint must report no change"
        );
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 10, &ctx_default(""))
            .unwrap();
        assert!(p.set_attention(None), "clearing must report a change");
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 10, &ctx_default(""))
            .unwrap();
        let visible = strip_csi(&String::from_utf8(buf).unwrap());
        assert!(
            !visible.contains("ASK"),
            "cleared hint must not repaint; got {visible:?}"
        );
    }

    /// phux-foz.4: the painter-owned focused-pane cwd feeds the `cwd`
    /// widget; setting it invalidates the cache and the widget renders
    /// the (home-uncollapsed here) directory.
    #[test]
    fn painter_renders_focused_cwd_through_cwd_widget() {
        let cfg = StatusCfg {
            left: vec![spec("cwd", &[])],
            ..Default::default()
        };
        let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 24, &ctx_default(""))
            .unwrap();
        assert!(
            !strip_csi(&String::from_utf8_lossy(&buf)).contains("/tmp"),
            "unknown cwd renders nothing"
        );
        assert!(p.set_focused_cwd(Some("/tmp/project".to_owned())));
        assert!(
            !p.set_focused_cwd(Some("/tmp/project".to_owned())),
            "unchanged cwd reports no change"
        );
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 24, &ctx_default(""))
            .unwrap();
        let visible = strip_csi(&String::from_utf8_lossy(&buf));
        assert!(
            visible.contains("/tmp/project"),
            "cwd must render; got {visible:?}"
        );
    }

    /// phux-foz.4: the painter-owned last-exit feeds the `exit` widget.
    /// Clearing it (a code-less `command_finished`) blanks the widget again.
    #[test]
    fn painter_renders_last_exit_through_exit_widget() {
        let cfg = StatusCfg {
            right: vec![spec(
                "exit",
                &[("format", toml::Value::String("rc={code}".into()))],
            )],
            ..Default::default()
        };
        let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        assert!(p.set_last_exit(Some(127)));
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 24, &ctx_default(""))
            .unwrap();
        let visible = strip_csi(&String::from_utf8_lossy(&buf));
        assert!(
            visible.contains("rc=127"),
            "exit code must render; got {visible:?}"
        );
        assert!(p.set_last_exit(None), "clearing reports a change");
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 24, &ctx_default(""))
            .unwrap();
        let visible = strip_csi(&String::from_utf8_lossy(&buf));
        assert!(
            !visible.contains("rc="),
            "cleared exit must blank the widget; got {visible:?}"
        );
    }

    /// phux-r82.6: the painter exposes its bar's exec feeds so the driver
    /// can spawn runners; pushing output through a feed shows up on the
    /// next paint (the async-refresh-into-cached-state contract).
    #[test]
    fn painter_exec_feed_output_lands_on_the_bar() {
        let cfg = StatusCfg {
            left: vec![spec(
                "exec",
                &[("command", toml::Value::String("battery.sh".into()))],
            )],
            ..Default::default()
        };
        let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        let feeds = p.exec_feeds();
        assert_eq!(feeds.len(), 1, "one exec widget => one feed");

        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 24, &ctx_default(""))
            .unwrap();
        assert!(
            !strip_csi(&String::from_utf8_lossy(&buf)).contains("BAT"),
            "no output before the first run"
        );

        feeds[0].apply_output("BAT 87%\n");
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 24, &ctx_default(""))
            .unwrap();
        let visible = strip_csi(&String::from_utf8_lossy(&buf));
        assert!(
            visible.contains("BAT 87%"),
            "cached exec output must render; got {visible:?}"
        );
    }

    /// phux-foz.12: after a paint, the painter resolves click columns to
    /// the window tabs of the strip it painted: "0:bash 1:vim" in the left
    /// slot puts window 0 on columns 0..6, the separator on 6, window 1 on
    /// 7..12, and blank padding after — hit, miss, hit, miss.
    #[test]
    fn window_hit_at_maps_painted_tab_columns() {
        let mut p = StatusBarPainter::new(windows_bar(), Position::Bottom);
        p.set_windows(vec![
            WindowInfo {
                name: "bash".to_owned(),
                active: true,
                zoomed: false,
                attention: false,
                branch: None,
            },
            WindowInfo {
                name: "vim".to_owned(),
                active: false,
                zoomed: false,
                attention: false,
                branch: None,
            },
        ]);
        // Before the first paint there is no strip to hit.
        assert_eq!(p.window_hit_at(0), None);
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 10, &ctx_default(""))
            .unwrap();
        // "0:bash 1:vim": tabs at 0..=5 and 7..=11.
        for x in 0..=5 {
            assert_eq!(p.window_hit_at(x), Some(0), "col {x}");
        }
        assert_eq!(p.window_hit_at(6), None, "separator is inert");
        for x in 7..=11 {
            assert_eq!(p.window_hit_at(x), Some(1), "col {x}");
        }
        assert_eq!(p.window_hit_at(12), None, "padding is inert");
        assert_eq!(p.window_hit_at(39), None, "right edge is inert");
        assert_eq!(p.window_hit_at(40), None, "off-strip is inert");
    }

    /// phux-qtw8: with a left sidebar docked the bar starts BESIDE the strip,
    /// not under it — the window tabs the user reported reading as "under the
    /// sidebar". The CUP lands on the strip's first free column and no cell is
    /// emitted left of it.
    #[test]
    fn left_sidebar_inset_shifts_the_bar_out_of_the_strip() {
        let mut p = StatusBarPainter::new(windows_bar(), Position::Bottom);
        p.set_windows(vec![WindowInfo {
            name: "bash".to_owned(),
            active: true,
            zoomed: false,
            attention: false,
            branch: None,
        }]);
        let inset = BarInset { left: 20, right: 0 };
        let mut buf = Vec::new();
        p.paint(&mut buf, inset, 40, 10, &ctx_default("")).unwrap();
        let s = String::from_utf8(buf).expect("utf8");
        // Bottom row of a 10-row viewport, column 21 (1-based) = x 20.
        assert!(
            s.contains("\x1b[10;21H"),
            "bar must start at the strip's right edge: {s:?}"
        );
        assert!(
            !s.contains("\x1b[10;1H"),
            "bar must not paint from column 0 (under the strip): {s:?}"
        );
        // Only the residual span is composed — 40 cols minus the 20-col strip.
        assert_eq!(
            p.last_row.as_ref().map(|(x, w, _)| (*x, *w)),
            Some((20, 20))
        );
    }

    /// phux-qtw8: a screen column is mapped back through the origin the bar
    /// painted at, so a tab click with a sidebar docked selects the window
    /// actually under the pointer rather than one 20 columns to its left.
    #[test]
    fn window_hit_at_is_relative_to_the_inset_origin() {
        let mut p = StatusBarPainter::new(windows_bar(), Position::Bottom);
        p.set_windows(vec![
            WindowInfo {
                name: "bash".to_owned(),
                active: true,
                zoomed: false,
                attention: false,
                branch: None,
            },
            WindowInfo {
                name: "vim".to_owned(),
                active: false,
                zoomed: false,
                attention: false,
                branch: None,
            },
        ]);
        let mut buf = Vec::new();
        p.paint(
            &mut buf,
            BarInset { left: 20, right: 0 },
            40,
            10,
            &ctx_default(""),
        )
        .unwrap();
        // Same "0:bash 1:vim" strip as the full-width case, shifted right 20.
        for x in 0..20 {
            assert_eq!(
                p.window_hit_at(x),
                None,
                "col {x} is the strip, not the bar"
            );
        }
        for x in 20..=25 {
            assert_eq!(p.window_hit_at(x), Some(0), "col {x}");
        }
        assert_eq!(p.window_hit_at(26), None, "separator is inert");
        for x in 27..=31 {
            assert_eq!(p.window_hit_at(x), Some(1), "col {x}");
        }
        assert_eq!(p.window_hit_at(32), None, "padding is inert");
    }

    /// phux-qtw8: a right-docked sidebar narrows the bar instead of moving it —
    /// the origin stays at 0 and the right-aligned widgets (and the supervisory
    /// badge) stop at the strip's left edge.
    #[test]
    fn right_sidebar_inset_narrows_the_bar_in_place() {
        let mut p = StatusBarPainter::new(windows_bar(), Position::Bottom);
        p.set_windows(vec![WindowInfo {
            name: "bash".to_owned(),
            active: true,
            zoomed: false,
            attention: false,
            branch: None,
        }]);
        p.set_supervisory(Some("[F]".to_owned()));
        let mut buf = Vec::new();
        p.paint(
            &mut buf,
            BarInset { left: 0, right: 20 },
            40,
            10,
            &ctx_default(""),
        )
        .unwrap();
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("\x1b[10;1H"), "bar keeps its origin: {s:?}");
        // The badge right-aligns to the BAR's right edge (col 20, 0-based 17),
        // not the viewport's — 1-based column 18.
        assert!(
            s.contains("\x1b[10;18H\x1b[7;1m[F]"),
            "badge must right-align to the bar, not the viewport: {s:?}"
        );
        assert_eq!(p.last_row.as_ref().map(|(x, w, _)| (*x, *w)), Some((0, 20)));
    }

    /// phux-foz.12: the hit map tracks the strip across a window-list
    /// change + repaint — after a select the active marker moves but the
    /// columns keep resolving against the fresh paint.
    #[test]
    fn window_hit_at_follows_repaints() {
        let mut p = StatusBarPainter::new(windows_bar(), Position::Bottom);
        p.set_windows(vec![WindowInfo {
            name: "a".to_owned(),
            active: true,
            zoomed: false,
            attention: false,
            branch: None,
        }]);
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 10, &ctx_default(""))
            .unwrap();
        assert_eq!(p.window_hit_at(0), Some(0));
        assert_eq!(p.window_hit_at(4), None, "only one 3-cell tab");
        // Grow the list; the next paint extends the hit map.
        p.set_windows(vec![
            WindowInfo {
                name: "a".to_owned(),
                active: false,
                zoomed: false,
                attention: false,
                branch: None,
            },
            WindowInfo {
                name: "b".to_owned(),
                active: true,
                zoomed: false,
                attention: false,
                branch: None,
            },
        ]);
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 10, &ctx_default(""))
            .unwrap();
        assert_eq!(p.window_hit_at(4), Some(1), "new tab is hittable");
    }

    /// phux-foz.12: the error-line painter paints a diagnostic strip, not
    /// tabs — every column is inert.
    #[test]
    fn window_hit_at_is_inert_on_the_error_line() {
        let mut p = StatusBarPainter::error_line("config error: boom");
        let mut buf = Vec::new();
        p.paint(&mut buf, BarInset::NONE, 40, 10, &ctx_default(""))
            .unwrap();
        for x in 0..40 {
            assert_eq!(p.window_hit_at(x), None);
        }
    }

    #[test]
    fn render_status_bar_function_emits_cup_and_text() {
        // Direct test of the stateless function form.
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..Default::default()
        };
        let bar = build_bar(&cfg);
        let mut buf = Vec::new();
        render_status_bar(&mut buf, &bar, &ctx_default("hello"), 23, 0, 20).unwrap();
        let s = String::from_utf8_lossy(&buf);
        // 23 → 24 (1-based).
        assert!(s.contains("\x1b[24;1H"), "no CUP-to-row-24: {s:?}");
        assert!(s.contains("hello"), "missing text: {s:?}");
        assert!(s.ends_with("\x1b[0m"), "missing SGR reset tail: {s:?}");
    }

    #[test]
    fn painter_threads_configured_prefix_to_help_hints_widget() {
        let cfg = StatusCfg {
            center: vec![spec("help-hints", &[])],
            ..Default::default()
        };
        let mut painter = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        painter.set_prefix("C-b");

        let mut buf = Vec::new();
        painter
            .paint(&mut buf, BarInset::NONE, 80, 24, &ctx_default(""))
            .unwrap();
        let visible = strip_csi(&String::from_utf8(buf).unwrap());

        assert!(
            visible.contains("C-b  Space palette"),
            "configured prefix should reach hints widget: {visible:?}"
        );
        assert!(
            !visible.contains("C-a"),
            "default prefix must not leak after rebind: {visible:?}"
        );
    }

    #[test]
    fn render_status_bar_empty_bar_is_noop() {
        let cfg = StatusCfg::default();
        let bar = build_bar(&cfg);
        let mut buf = Vec::new();
        render_status_bar(&mut buf, &bar, &ctx_default(""), 0, 0, 80).unwrap();
        assert!(buf.is_empty());
    }

    #[test]
    fn render_status_bar_zero_cols_is_noop() {
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..Default::default()
        };
        let bar = build_bar(&cfg);
        let mut buf = Vec::new();
        render_status_bar(&mut buf, &bar, &ctx_default("x"), 0, 0, 0).unwrap();
        assert!(buf.is_empty());
    }
}
