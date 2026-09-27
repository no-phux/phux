//! Status-bar chrome layer.
//!
//! [`phux_config::widget::StatusBar`] composes the widget row;
//! [`StatusBarPainter`] lays it into a `cols × 1` ratatui buffer and emits
//! raw VT (CUP + per-cell SGR + grapheme), ending in an SGR reset.
//! The caller restores the focused pane's cursor afterwards (ADR-0020). The
//! bar defaults to [`Position::Top`] (`docs/consumers/tui.md` §8.5); the pane
//! content rect shifts to match.

use std::io::{self, Write};
use std::time::{Duration, SystemTime};

use std::str::FromStr;

use phux_config::widget::{
    Cell as WidgetCell, CellHit, CellStyle, StatusBar, WidgetContext, WindowInfo,
};
use ratatui::buffer::{Buffer, Cell as RatatuiCell};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

/// Where the status bar lives in the outer terminal. Defaults to
/// [`Self::Top`] per `docs/consumers/tui.md` §8.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Position {
    /// One row at the very bottom of the outer terminal.
    Bottom,
    /// One row at the very top of the outer terminal. Surfaced by the
    /// `[status] position = "top"` config key.
    #[default]
    Top,
}

impl From<phux_config::StatusPosition> for Position {
    /// Map the `[status] position` config value onto the render enum.
    /// The mapping lives at this boundary so `phux-config`
    /// stays free of render types (ADR-0020).
    fn from(pos: phux_config::StatusPosition) -> Self {
        match pos {
            phux_config::StatusPosition::Bottom => Self::Bottom,
            phux_config::StatusPosition::Top => Self::Top,
        }
    }
}

/// Inputs for one frame of the bar, borrowed across the ratatui boundary.
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
    /// The focused pane's live working directory (`""` when
    /// unknown), consumed by the `cwd` widget. Injected by the painter
    /// from driver-fed state, like `windows`.
    pub cwd: &'a str,
    /// The focused pane's last known command exit code
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

/// Build a [`StatusBarContext`] for one render pass. Window list, cwd, and
/// last exit are injected by the painter.
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

/// A config [`CellStyle`] as a ratatui [`Style`]; an unparseable color
/// degrades to the terminal default with a warning.
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

/// The persistent error strip's style — reverse video + bold,
/// so the diagnostic reads as an alarm strip rather than blending into
/// normal chrome.
fn alarm_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
}

/// `message` in a fresh `cols`-wide row buffer, styled `style`, truncated
/// and space-padded to the full width. Shared by the error line and the
/// notice toast, live and in the snapshot compose.
fn full_row_buffer(message: &str, style: Style, cols: u16) -> Buffer {
    let mut buffer = Buffer::empty(Rect::new(0, 0, cols, 1));
    let mut tmp = [0u8; 4];
    let mut col: u16 = 0;
    for ch in message.chars() {
        // Refuses control characters and explicit bidi overrides: a
        // notice carries a config-reload error, which quotes the user's
        // own file, and the strip is emitted as one uninterrupted run.
        let Some(width) = crate::render::cell_width(ch) else {
            continue;
        };
        let width = u16::try_from(width).unwrap_or(1);
        if width == 0 {
            // A combining mark belongs to the cell before it.
            if col > 0 {
                let cell = &mut buffer[(col - 1, 0)];
                let joined = format!("{}{}", cell.symbol(), ch);
                cell.set_symbol(&joined);
            }
            continue;
        }
        // A glyph that would straddle the last column is dropped whole, or
        // the row would emit `cols + 1` columns and wrap into the panes.
        if col.saturating_add(width) > cols {
            break;
        }
        let cell = &mut buffer[(col, 0)];
        cell.set_symbol(ch.encode_utf8(&mut tmp));
        cell.set_style(style);
        // The columns a wide glyph claims are styled but never emitted:
        // `write_buffer` skips them, having already advanced past them.
        for claimed in 1..width {
            let cell = &mut buffer[(col + claimed, 0)];
            cell.set_symbol(" ");
            cell.set_style(style);
        }
        col = col.saturating_add(width);
    }
    while col < cols {
        let cell = &mut buffer[(col, 0)];
        cell.set_symbol(" ");
        cell.set_style(style);
        col = col.saturating_add(1);
    }
    buffer
}

/// Reverse-video + underline the tab under a live drag so the drop slot
/// is visible before release. Hit stamps stay put, so a release still
/// resolves against the same cells.
fn mark_window_drop(row: &mut [WidgetCell], drop_at: Option<usize>) {
    let Some(index) = drop_at else {
        return;
    };
    for cell in row {
        if cell.hit == Some(CellHit::Window(index)) {
            let mut style = cell.style.clone().unwrap_or_default();
            style.reverse = true;
            style.underline = true;
            cell.style = Some(style);
        }
    }
}

/// Copy a composed widget row into a ratatui [`Buffer`]. Cells a widget
/// left without a background take `fill` (`Reset` shows the host's), so the
/// bar reads as one surface with the sidebar.
fn fill_buffer(buffer: &mut Buffer, row: &[WidgetCell], cols: u16, fill: Color) {
    if fill != Color::Reset {
        buffer.set_style(Rect::new(0, 0, cols, 1), Style::default().bg(fill));
    }
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
            let mut s = String::with_capacity(cell.text.len());
            for ch in &cell.text {
                s.push_str(ch.encode_utf8(&mut tmp));
            }
            target.set_symbol(&s);
        }
        // Carry per-cell style (fg/bg/attrs) across the
        // ratatui boundary; `write_buffer` emits it as SGR.
        if let Some(style) = &cell.style {
            target.set_style(to_ratatui_style(style));
        }
    }
}

/// Emit row 0 of `buffer` at (`row_index`, `x`) as raw VT: CUP, SGR reset,
/// per-cell symbols, SGR reset. The cursor is left for the caller.
fn write_buffer<W: Write>(
    out: &mut W,
    buffer: &Buffer,
    row_index: u16,
    x: u16,
    cols: u16,
) -> io::Result<()> {
    let one_based_row = row_index.saturating_add(1);
    let one_based_col = x.saturating_add(1);
    // No `?25l` here: a hide without a guaranteed show once stranded the
    // cursor invisible when the caller had nothing to restore.
    write!(out, "\x1b[{one_based_row};{one_based_col}H\x1b[0m")?;
    let mut prev_styled = None;
    let mut x = 0;
    while x < cols {
        let cell = &buffer[(x, 0)];
        // Per-cell SGR (shared with the overlay painter).
        crate::render::sgr::emit_cell_sgr(out, cell, &mut prev_styled)?;
        let sym = cell.symbol();
        // One run from one CUP must advance exactly `cols` columns, so the
        // cell a wide glyph claims is skipped rather than written.
        let advance = if sym.is_empty() {
            out.write_all(b" ")?;
            1
        } else {
            out.write_all(sym.as_bytes())?;
            u16::try_from(crate::render::display_width(sym))
                .unwrap_or(1)
                .max(1)
        };
        x = x.saturating_add(advance);
    }
    // SGR reset on exit so the next paint inherits no attributes from us.
    out.write_all(b"\x1b[0m")?;
    out.flush()
}

/// ADR-0033: the supervisory badge's columns, right-aligned `right_offset`
/// cells in from the bar's own right edge (ASCII-only, so chars are cells).
fn badge_span(badge: &str, cols: u16, right_offset: u16) -> std::ops::Range<u16> {
    let end = cols.saturating_sub(right_offset);
    let width = u16::try_from(badge.chars().count())
        .unwrap_or(u16::MAX)
        .min(end);
    end - width..end
}

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
    let start_col = x.saturating_add(badge_span(badge, cols, 0).start);
    let one_based_row = row_index.saturating_add(1);
    let one_based_col = start_col.saturating_add(1);
    // CUP to the chip's left edge, reverse+bold, text, hard reset.
    write!(
        out,
        "\x1b[{one_based_row};{one_based_col}H\x1b[7;1m{visible}\x1b[0m"
    )?;
    out.flush()
}

/// Emit the attention chip just left of the supervisory badge, reverse and
/// bold in the theme's `attention` color.
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
    let start_col = x.saturating_add(badge_span(hint, cols, right_offset).start);
    let one_based_row = row_index.saturating_add(1);
    let one_based_col = start_col.saturating_add(1);
    write!(out, "\x1b[{one_based_row};{one_based_col}H\x1b[7;1m")?;
    crate::render::sgr::write_sgr_color(out, color, true)?;
    write!(out, "{visible}\x1b[0m")?;
    out.flush()
}

/// Paint a transient notice as a compact right-aligned toast over the bar.
/// The one-cell padding gives the reversed region a chip boundary without
/// adding punctuation to the notice itself.
fn paint_notice_overlay<W: Write>(
    out: &mut W,
    notice: &Notice,
    row_index: u16,
    x: u16,
    cols: u16,
) -> io::Result<()> {
    let Some((buffer, start, width)) = notice_buffer(notice, cols) else {
        return Ok(());
    };
    write_buffer(out, &buffer, row_index, x.saturating_add(start), width)
}

/// Build the clipped cells shared by the live toast and rendered snapshots.
fn notice_buffer(notice: &Notice, cols: u16) -> Option<(Buffer, u16, u16)> {
    let label = format!(" {} ", notice.text);
    let span = notice_span(notice, cols)?;
    let width = span.end - span.start;
    Some((
        full_row_buffer(&label, notice.severity.style(), width),
        span.start,
        width,
    ))
}

/// Columns occupied by the right-aligned toast, including its padding.
fn notice_span(notice: &Notice, cols: u16) -> Option<std::ops::Range<u16>> {
    let width = u16::try_from(crate::render::display_width(&format!(" {} ", notice.text)))
        .unwrap_or(u16::MAX)
        .min(cols);
    (width > 0).then(|| cols - width..cols)
}

/// Copy the transient toast into the status snapshot's already-composed row.
fn overlay_notice_into_buffer(buffer: &mut Buffer, notice: &Notice, cols: u16) {
    let Some((toast, start, width)) = notice_buffer(notice, cols) else {
        return;
    };
    for offset in 0..width {
        buffer[(start + offset, 0)] = toast[(offset, 0)].clone();
    }
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
    let start = badge_span(badge, cols, right_offset).start;
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

/// Columns the bar yields at each viewport edge to a docked sidebar, so its
/// tabs never paint under the strip. [`Self::NONE`] is a full-width bar.
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

    /// The bar's origin column and width in a `cols`-wide viewport
    /// (saturating: an oversized inset is a zero-width, no-op bar).
    #[must_use]
    pub const fn span(self, cols: u16) -> (u16, u16) {
        // `Ord::min` is not const for u16.
        let x = if self.left < cols { self.left } else { cols };
        let width = cols.saturating_sub(self.left).saturating_sub(self.right);
        (x, width)
    }
}

/// How long a transient [`Notice`] stays on the bar (expiry rides the 1 s
/// status tick).
pub const NOTICE_TTL: Duration = Duration::from_secs(7);

/// A [`Notice`]'s chip style: `Warn` bold, `Info` plain reverse video.
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

/// A transient, self-expiring status-bar message (input handovers, degraded
/// federation, pane exits).
///
/// One newest-wins slot restarting the
/// [`NOTICE_TTL`] clock, painted as a right-aligned chip over the bar; the
/// persistent error line outranks it.
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

/// Whether a bar paint may reuse its last composed strip. The painter sees
/// its own inputs (each setter invalidates) but not the clock, `exec` caches,
/// or the session name, so the caller decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ComposePolicy {
    /// Run the widget pipeline. Every trigger that can have moved an input the
    /// painter does not own uses this: the 1 s poll tick, a chrome repaint, a
    /// full-frame redraw, and every standalone caller.
    Always,
    /// Reuse the cached strip unless a setter invalidated it; for pane-output
    /// paints, which change no bar input.
    WhenDirty,
}

/// VT painter for a composed [`StatusBar`], caching the last row so an
/// unchanged repaint is a no-op.
pub struct StatusBarPainter {
    bar: StatusBar,
    position: Position,
    /// Last painted strip with its `(x, width)` span (a sidebar toggle moves
    /// an otherwise identical row); also what hit tests read. `None` forces a
    /// paint.
    last_row: Option<(u16, u16, Vec<WidgetCell>)>,
    /// Last (cols, rows) we painted into. Different dims invalidate
    /// `last_row` and force a fresh paint.
    last_viewport: Option<(u16, u16)>,
    /// The window list fed to the `windows` widget. Updated
    /// by the driver from the `Workspace` and injected into the render
    /// context inside [`Self::paint`]; a change invalidates the cache.
    windows: Vec<WindowInfo>,
    /// A fixed config-error line painted instead of the widgets.
    error: Option<String>,
    /// The notice slot and its expiry; never set under the error line or on
    /// an empty bar.
    notice: Option<(Notice, std::time::Instant)>,
    /// ADR-0033 supervisory badge overlaid right-aligned on the widget row.
    supervisory: Option<String>,
    /// Agent-attention hint overlaid just left of the badge.
    attention: Option<String>,
    /// Chip foreground for the attention hint, from the theme's
    /// `attention` slot (the painter never hardcodes it). Under the chip's
    /// reverse video the foreground reads as the fill color.
    attention_fg: Color,
    /// The row's bed, from the theme's `surface` slot: the same material
    /// as the sidebar, so the two read as one frame around the panes.
    fill: Color,
    prefix: String,
    /// The focused pane's live cwd for the `cwd` widget (`None` renders
    /// nothing).
    focused_cwd: Option<String>,
    /// The focused pane's last known command exit code, fed
    /// by the driver from `command_finished` events. `None` ⇒ unknown.
    last_exit: Option<i32>,
    /// Window index under a live tab drag, painted as the insertion
    /// marker. `None` when no tab drag is over this strip.
    drop_at: Option<usize>,
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
            .field("fill", &self.fill)
            .field("prefix", &self.prefix)
            .field("focused_cwd", &self.focused_cwd)
            .field("last_exit", &self.last_exit)
            .field("drop_at", &self.drop_at)
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
            fill: Color::Reset,
            prefix: "C-a".to_owned(),
            focused_cwd: None,
            last_exit: None,
            drop_at: None,
        }
    }

    /// A painter showing a fixed error line instead of widgets, used when the
    /// config fails to load: it is never empty and always polls, so the
    /// diagnostic (pointing at `phux config check`) stays on screen.
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
            fill: Color::Reset,
            prefix: "C-a".to_owned(),
            focused_cwd: None,
            last_exit: None,
            drop_at: None,
        }
    }

    /// Whether this painter shows the config-error line (a lesser diagnostic
    /// must not replace it).
    #[must_use]
    pub const fn is_error_line(&self) -> bool {
        self.error.is_some()
    }

    /// Which row this painter reserves ([`Position::Bottom`] or
    /// [`Position::Top`]). The paint/layout helpers read this so the pane
    /// content rect and the bar row agree on the reservation.
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

    /// Update the `windows` widget's list; true when it changed. A change
    /// invalidates the row cache.
    pub fn set_windows(&mut self, windows: Vec<WindowInfo>) -> bool {
        if self.windows == windows {
            return false;
        }
        self.windows = windows;
        self.invalidate();
        true
    }

    /// Point (or clear) the tab-drag insertion marker at window `drop_at`;
    /// true when it changed. No-op on the error line.
    pub fn set_drop_index(&mut self, drop_at: Option<usize>) -> bool {
        if self.error.is_some() || self.drop_at == drop_at {
            return false;
        }
        self.drop_at = drop_at;
        self.invalidate();
        true
    }

    /// Set (or clear, with `None`) the focused pane's live
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

    /// Set (or clear, with `None`) the focused pane's last
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

    /// Show `notice` for [`NOTICE_TTL`] from `now`, replacing any current
    /// one; true when accepted. Refused (logged instead) under the error line,
    /// or on an empty bar, which reserves no row to paint it on
    /// (`docs/consumers/tui.md` §8.7).
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

    /// Drop the notice once its deadline passes (from the status tick); true
    /// when cleared.
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

    /// Whether `expected` currently owns the transient notice slot.
    pub(crate) fn notice_is(&self, expected: &str) -> bool {
        self.notice
            .as_ref()
            .is_some_and(|(notice, _)| notice.text == expected)
    }

    /// ADR-0033: set or clear the supervisory badge; true when it changed. A
    /// no-op under the error line, where the badge cannot show.
    pub fn set_supervisory(&mut self, badge: Option<String>) -> bool {
        if self.error.is_some() || self.supervisory == badge {
            return false;
        }
        self.supervisory = badge;
        self.invalidate();
        true
    }

    /// Set or clear the attention hint; same contract as the badge.
    pub fn set_attention(&mut self, hint: Option<String>) -> bool {
        if self.error.is_some() || self.attention == hint {
            return false;
        }
        self.attention = hint;
        self.invalidate();
        true
    }

    /// Set the attention chip's foreground from the theme's
    /// `attention` slot. The driver calls this once at attach; the painter
    /// itself never hardcodes the color.
    pub fn set_attention_color(&mut self, color: Color) {
        if self.attention_fg != color {
            self.attention_fg = color;
            self.invalidate();
        }
    }

    /// Set the row's bed from the theme's `surface` slot. The driver calls
    /// this beside [`Self::set_attention_color`]; `Reset` shows the host
    /// terminal's own background.
    pub fn set_fill(&mut self, color: Color) {
        if self.fill != color {
            self.fill = color;
            self.invalidate();
        }
    }

    /// Cells the attention chip sits in from the right edge: the badge width
    /// plus a gap, or `0` with no badge.
    fn attention_offset(&self) -> u16 {
        self.supervisory.as_ref().map_or(0, |badge| {
            u16::try_from(badge.chars().count())
                .unwrap_or(u16::MAX)
                .saturating_add(1)
        })
    }

    /// The separator is painted, not merely skipped over: widget text must
    /// not leak between two status chips. Same geometry for VT and snapshots.
    fn badge_gap(&self, cols: u16) -> Option<u16> {
        let badge = self.supervisory.as_deref().filter(|s| !s.is_empty())?;
        let hint = self.attention.as_deref().filter(|s| !s.is_empty())?;
        let attention = badge_span(hint, cols, self.attention_offset());
        (!attention.is_empty()).then(|| badge_span(badge, cols, 0).start.saturating_sub(1))
    }

    /// True when no widgets are configured (never for the error line).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.error.is_none() && self.bar.is_empty()
    }

    /// The repaint cadence: `Some(1s)` for any non-empty bar (and the error
    /// line, so it survives pane output), `None` otherwise.
    #[must_use]
    pub fn min_poll_interval(&self) -> Option<Duration> {
        if self.is_empty() {
            None
        } else {
            Some(Duration::from_secs(1))
        }
    }

    /// Paint the bar for a `cols × rows` viewport within `inset`; unchanged
    /// output and dims write nothing.
    #[cfg(test)]
    pub fn paint<W: Write>(
        &mut self,
        out: &mut W,
        inset: BarInset,
        cols: u16,
        rows: u16,
        ctx: &StatusBarContext<'_>,
    ) -> io::Result<()> {
        self.paint_outcome(out, inset, cols, rows, ctx, ComposePolicy::Always)
            .map(drop)
    }

    /// Paint the bar and report whether this call emitted the row rather than
    /// taking the unchanged-content cache fast path.
    pub(crate) fn paint_outcome<W: Write>(
        &mut self,
        out: &mut W,
        inset: BarInset,
        cols: u16,
        rows: u16,
        ctx: &StatusBarContext<'_>,
        compose: ComposePolicy,
    ) -> io::Result<bool> {
        if cols == 0 || rows == 0 {
            return Ok(false);
        }
        // The bar yields the sidebar's columns. Everything below
        // composes and hit-tests against this span, not the viewport — an
        // inset wider than the terminal leaves nothing to paint.
        let (x, cols) = inset.span(cols);
        if cols == 0 {
            return Ok(false);
        }
        if let Some(outcome) = self.paint_error_takeover(out, x, cols, rows)? {
            return Ok(outcome);
        }
        // The badge rides the bar, so an empty bar with no windows paints
        // nothing at all.
        if self.bar.is_empty() && self.windows.is_empty() {
            return Ok(false);
        }
        // Pane-output paints never change the bar; skip the widget pipeline.
        if self.cached_compose_stands_in(compose, x, cols, rows) {
            return Ok(false);
        }
        let ctx = self.ctx_with_window_list(ctx);
        crate::attach::render_prof::note_bar_composes(1);
        let mut new_row = self.bar.render(&ctx.as_widget(), cols);
        mark_window_drop(&mut new_row, self.drop_at);
        if !self.needs_repaint(x, cols, rows, &new_row) {
            return Ok(false);
        }
        let row_index = self.row_index(rows);
        let mut buffer = Buffer::empty(Rect::new(0, 0, cols, 1));
        fill_buffer(&mut buffer, &new_row, cols, self.fill);
        write_buffer(out, &buffer, row_index, x, cols)?;
        self.paint_row_overlays(out, row_index, x, cols)?;
        self.last_row = Some((x, cols, new_row));
        self.last_viewport = Some((cols, rows));
        Ok(true)
    }

    /// Paint the error line when it owns the row; `None` when the widget
    /// pipeline does.
    fn paint_error_takeover<W: Write>(
        &mut self,
        out: &mut W,
        x: u16,
        cols: u16,
        rows: u16,
    ) -> io::Result<Option<bool>> {
        if self.error.is_some() {
            return self.paint_error_line(out, x, cols, rows).map(Some);
        }
        Ok(None)
    }

    /// The caller's context plus the painter-owned window list, cwd, and exit
    /// code, injected before compose so `last_row` holds what is on screen.
    fn ctx_with_window_list<'a>(&'a self, ctx: &StatusBarContext<'a>) -> StatusBarContext<'a> {
        StatusBarContext {
            prefix: &self.prefix,
            windows: &self.windows,
            cwd: self.focused_cwd.as_deref().unwrap_or(""),
            last_exit: self.last_exit,
            ..*ctx
        }
    }

    /// Whether the cached strip can stand in for a compose: a pane-output
    /// paint with a cache for exactly this span and viewport.
    fn cached_compose_stands_in(
        &self,
        compose: ComposePolicy,
        x: u16,
        cols: u16,
        rows: u16,
    ) -> bool {
        if matches!(compose, ComposePolicy::Always) {
            return false;
        }
        let Some((prev_x, prev_cols, _)) = self.last_row.as_ref() else {
            return false;
        };
        *prev_x == x && *prev_cols == cols && self.last_viewport == Some((cols, rows))
    }

    /// Whether the fresh row differs from the cached paint in content,
    /// origin, or viewport.
    fn needs_repaint(&self, x: u16, cols: u16, rows: u16, new_row: &[WidgetCell]) -> bool {
        let viewport_changed = self.last_viewport != Some((cols, rows));
        let row_changed = match &self.last_row {
            Some((prev_x, w, prev)) => *prev_x != x || *w != cols || prev.as_slice() != new_row,
            None => true,
        };
        viewport_changed || row_changed
    }

    /// The viewport row the bar occupies, given its configured position.
    const fn row_index(&self, rows: u16) -> u16 {
        match self.position {
            Position::Bottom => rows.saturating_sub(1),
            Position::Top => 0,
        }
    }

    /// Overlay the badge, the attention chip, and the notice toast on the
    /// freshly painted row (the full-row repaint already erased stale ones).
    fn paint_row_overlays<W: Write>(
        &self,
        out: &mut W,
        row_index: u16,
        x: u16,
        cols: u16,
    ) -> io::Result<()> {
        if let Some(gap) = self.badge_gap(cols) {
            let mut blank = Buffer::empty(Rect::new(0, 0, 1, 1));
            blank.set_style(Rect::new(0, 0, 1, 1), Style::default().bg(self.fill));
            write_buffer(out, &blank, row_index, x + gap, 1)?;
        }
        if let Some(badge) = &self.supervisory {
            paint_supervisory_overlay(out, badge, row_index, x, cols)?;
        }
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
        if let Some((notice, _)) = &self.notice {
            paint_notice_overlay(out, notice, row_index, x, cols)?;
        }
        Ok(())
    }

    /// Compose the bar row into an `inset`-wide buffer without emitting VT or
    /// touching the cache, for `phux snapshot --rendered`. Returns
    /// `(buffer, x, row_index)`, or `None` when nothing would paint.
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
        let mut row = self.bar.render(&ctx.as_widget(), cols);
        mark_window_drop(&mut row, self.drop_at);
        let mut buffer = Buffer::empty(Rect::new(0, 0, cols, 1));
        fill_buffer(&mut buffer, &row, cols, self.fill);
        if let Some(gap) = self.badge_gap(cols) {
            buffer[(gap, 0)].reset();
            buffer[(gap, 0)].set_bg(self.fill);
        }
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
        // The attention hint composes left of the badge, themed
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
        // Mirror the live paint. The transient notice is a
        // compact toast over the normal row and temporarily outranks the
        // persistent right-edge chips beneath it.
        if let Some((notice, _)) = &self.notice {
            overlay_notice_into_buffer(&mut buffer, notice, cols);
        }
        Some((buffer, x, row_index))
    }

    /// Paint the error diagnostic as a reverse-video alarm strip.
    fn paint_error_line<W: Write>(
        &mut self,
        out: &mut W,
        x: u16,
        cols: u16,
        rows: u16,
    ) -> io::Result<bool> {
        // Callers gate on `self.error.is_some()`; an empty string is a
        // valid (if unusual) diagnostic, so default to "" rather than
        // returning early.
        let message = self.error.clone().unwrap_or_default();
        self.paint_full_row_message(out, &message, alarm_style(), x, cols, rows)
    }

    /// Paint `message` full-row in `style`, cached like the widget path
    /// (message changes always go through an invalidating setter).
    fn paint_full_row_message<W: Write>(
        &mut self,
        out: &mut W,
        message: &str,
        style: Style,
        x: u16,
        cols: u16,
        rows: u16,
    ) -> io::Result<bool> {
        let viewport_changed = self.last_viewport != Some((cols, rows));
        let moved = self
            .last_row
            .as_ref()
            .is_some_and(|(px, pw, _)| *px != x || *pw != cols);
        if !viewport_changed && !moved && self.last_row.is_some() {
            return Ok(false);
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
        Ok(true)
    }

    /// The async data feeds behind the bar's `exec` widgets.
    /// The driver spawns one bounded interval runner per feed; an
    /// error-line painter (empty bar) has none.
    #[must_use]
    pub fn exec_feeds(&self) -> Vec<phux_config::widget::ExecFeed> {
        self.bar.exec_feeds()
    }

    /// Force the next `paint_outcome` to redraw unconditionally —
    /// e.g. after a SIGWINCH or after the pane renderer wrote the
    /// bottom row.
    pub fn invalidate(&mut self) {
        self.last_row = None;
        self.last_viewport = None;
    }

    /// The window tab under screen column `x`, read from the strip last
    /// painted (so hits match the screen); `None` off the strip or on a
    /// non-tab cell.
    #[must_use]
    pub fn window_hit_at(&self, x: u16) -> Option<usize> {
        match self.hit_at(x)? {
            phux_config::widget::CellHit::Window(i) => Some(i),
            phux_config::widget::CellHit::Switch | phux_config::widget::CellHit::Action(_) => None,
        }
    }

    /// The interactive target under screen column `x`, resolved against the
    /// strip last painted.
    #[must_use]
    pub fn hit_at(&self, x: u16) -> Option<phux_config::widget::CellHit> {
        let (origin, cols, row) = self.last_row.as_ref()?;
        let col = x.checked_sub(*origin)?;
        if self
            .notice
            .as_ref()
            .and_then(|(notice, _)| notice_span(notice, *cols))
            .is_some_and(|span| span.contains(&col))
            || self
                .supervisory
                .as_deref()
                .is_some_and(|s| badge_span(s, *cols, 0).contains(&col))
            || self
                .attention
                .as_deref()
                .is_some_and(|s| badge_span(s, *cols, self.attention_offset()).contains(&col))
            || self.badge_gap(*cols) == Some(col)
        {
            return None;
        }
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

    /// The columns an emitted row advances the terminal, walked the way
    /// `write_buffer` does (by each symbol's display width).
    fn emitted_columns(buffer: &Buffer, cols: u16) -> u16 {
        let mut x = 0u16;
        let mut columns = 0u16;
        while x < cols {
            let sym = buffer[(x, 0)].symbol();
            let w = u16::try_from(crate::render::display_width(sym))
                .unwrap_or(1)
                .max(1);
            columns = columns.saturating_add(w);
            x = x.saturating_add(w);
        }
        columns
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
        StatusBar::build(cfg, &WidgetRegistry::with_builtins()).unwrap()
    }

    /// A painter with just the `session-name` widget on the left.
    fn session_bar(position: Position) -> StatusBarPainter {
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..Default::default()
        };
        StatusBarPainter::new(build_bar(&cfg), position)
    }

    fn windows_bar() -> StatusBarPainter {
        let cfg = StatusCfg {
            left: vec![spec("windows", &[])],
            ..StatusCfg::default()
        };
        StatusBarPainter::new(build_bar(&cfg), Position::Bottom)
    }

    fn wins(names: &[(&str, bool)]) -> Vec<WindowInfo> {
        names
            .iter()
            .map(|(name, active)| WindowInfo {
                name: (*name).to_owned(),
                active: *active,
                ..WindowInfo::default()
            })
            .collect()
    }

    /// One paint as raw VT.
    fn paint(
        p: &mut StatusBarPainter,
        inset: BarInset,
        cols: u16,
        rows: u16,
        session: &str,
    ) -> String {
        let mut buf = Vec::new();
        p.paint(&mut buf, inset, cols, rows, &ctx_default(session))
            .unwrap();
        String::from_utf8(buf).unwrap()
    }

    /// Strip CSI escapes (styled cells interleave SGR between glyphs).
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

    fn visible(p: &mut StatusBarPainter, cols: u16, rows: u16, session: &str) -> String {
        strip_csi(&paint(p, BarInset::NONE, cols, rows, session))
    }

    /// A notice strip laid by char would emit `cols + 1` columns when a wide
    /// character hit the last column, wrapping the bar into the panes. A
    /// straddling character is dropped whole.
    #[test]
    fn a_notice_strip_advances_exactly_its_own_width() {
        let style = Style::default();
        for message in [
            "\u{65e5}\u{672c}\u{8a9e}のエラー",
            "config error: \u{65e5}",
            "a\u{65e5}b\u{672c}c",
            "plain ascii notice",
        ] {
            for cols in 1u16..24 {
                let buffer = full_row_buffer(message, style, cols);
                assert_eq!(
                    emitted_columns(&buffer, cols),
                    cols,
                    "{message:?} at {cols}"
                );
            }
        }
        let buffer = full_row_buffer("a\u{65e5}", style, 2);
        assert_eq!(
            (buffer[(0, 0)].symbol(), buffer[(1, 0)].symbol()),
            ("a", " ")
        );
        let buffer = full_row_buffer("a\u{65e5}", style, 3);
        assert_eq!(buffer[(1, 0)].symbol(), "\u{65e5}");
    }

    /// A notice may quote the user's config; neither a control character nor
    /// a bidi override may become a cell of the raw-VT strip.
    #[test]
    fn a_notice_cannot_carry_controls_or_bidi_overrides() {
        let buffer = full_row_buffer("a\u{1b}[31m\u{202e}b", Style::default(), 10);
        let row: String = (0..10).map(|x| buffer[(x, 0)].symbol()).collect();
        assert!(
            !row.contains('\u{1b}') && !row.contains('\u{202e}'),
            "{row:?}"
        );
        assert!(row.starts_with("a[31mb"), "inert payload survives: {row:?}");
    }

    #[test]
    fn position_picks_the_bar_row() {
        assert_eq!(
            Position::from(phux_config::StatusPosition::Bottom),
            Position::Bottom
        );
        assert_eq!(
            Position::from(phux_config::StatusPosition::Top),
            Position::Top
        );
        for (position, cup) in [
            (Position::Bottom, "\x1b[24;1H"),
            (Position::Top, "\x1b[1;1H"),
        ] {
            let mut p = session_bar(position);
            assert_eq!(p.position(), position);
            let s = paint(&mut p, BarInset::NONE, 10, 24, "hi");
            assert!(s.contains(cup) && s.contains("hi"), "{s:?}");
        }
    }

    /// Unchanged inputs paint nothing; a viewport, context, or invalidate
    /// repaints; zero dims and an empty bar never paint.
    #[test]
    fn the_row_cache_repaints_only_on_change() {
        let mut p = session_bar(Position::Bottom);
        assert!(!paint(&mut p, BarInset::NONE, 10, 24, "x").is_empty());
        assert!(paint(&mut p, BarInset::NONE, 10, 24, "x").is_empty());
        assert!(!paint(&mut p, BarInset::NONE, 20, 24, "x").is_empty());
        assert!(!paint(&mut p, BarInset::NONE, 20, 24, "y").is_empty());
        p.invalidate();
        assert!(!paint(&mut p, BarInset::NONE, 20, 24, "y").is_empty());
        assert!(paint(&mut p, BarInset::NONE, 0, 24, "x").is_empty());
        assert!(paint(&mut p, BarInset::NONE, 80, 0, "x").is_empty());
        assert_eq!(p.min_poll_interval(), Some(Duration::from_secs(1)));

        let mut empty = StatusBarPainter::new(build_bar(&StatusCfg::default()), Position::Bottom);
        assert!(empty.is_empty());
        assert_eq!(empty.min_poll_interval(), None);
        assert!(paint(&mut empty, BarInset::NONE, 80, 24, "").is_empty());
    }

    #[test]
    fn time_and_session_both_appear_when_configured() {
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            right: vec![spec(
                "time",
                &[("format", toml::Value::String("LITERAL".into()))],
            )],
            ..Default::default()
        };
        let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        let s = paint(&mut p, BarInset::NONE, 30, 24, "main");
        assert!(s.contains("main") && s.contains("LITERAL"), "{s:?}");
    }

    /// The config-error painter reserves the row and keeps repainting it
    /// (polling, repainting after invalidate), with every column inert.
    #[test]
    fn error_line_painter_holds_the_bar_row() {
        let mut p = StatusBarPainter::error_line("config error: boom (run: phux config check)");
        assert!(!p.is_empty());
        assert_eq!(p.min_poll_interval(), Some(Duration::from_secs(1)));
        let s = paint(&mut p, BarInset::NONE, 80, 24, "");
        assert!(s.contains("\x1b[1;1H"), "{s:?}");
        let printable = strip_csi(&s);
        assert!(printable.contains("config error") && printable.contains("phux config check"));
        assert!(paint(&mut p, BarInset::NONE, 80, 24, "").is_empty());
        p.invalidate();
        assert!(!paint(&mut p, BarInset::NONE, 80, 24, "").is_empty());
        assert!((0..80).all(|x| p.window_hit_at(x).is_none()));
    }

    /// A notice floats over the bar as a compact right-aligned toast, the
    /// newest wins, it masks covered hit targets, and it expires at the TTL.
    #[test]
    fn notice_toast_lifecycle() {
        let mut p = session_bar(Position::Bottom);
        let now = std::time::Instant::now();
        assert!(p.set_notice(Notice::info("first notice"), now));
        let v = visible(&mut p, 40, 24, "sess");
        assert!(v.contains("first notice") && v.contains("sess"), "{v:?}");
        assert!(p.set_notice(Notice::warn("second notice"), now));
        let v = visible(&mut p, 40, 24, "sess");
        assert!(
            v.contains("second notice") && !v.contains("first notice"),
            "{v:?}"
        );
        assert!(!p.clear_expired_notice(now + Duration::from_secs(6)));
        assert!(p.clear_expired_notice(now + NOTICE_TTL));
        assert!(!p.clear_expired_notice(now + NOTICE_TTL));
        let v = visible(&mut p, 40, 24, "sess");
        assert!(v.contains("sess") && !v.contains("second notice"), "{v:?}");

        let mut p = session_bar(Position::Top);
        assert!(p.set_notice(Notice::warn("pane 4: exited 127"), now));
        let (buffer, _, _) = p
            .compose_buffer(BarInset::NONE, 40, 24, &ctx_default("main"))
            .expect("composes");
        let row: String = (0..40).map(|x| buffer[(x, 0)].symbol()).collect();
        assert!(
            row.starts_with("main") && row.ends_with(" pane 4: exited 127 "),
            "{row:?}"
        );

        let cfg = StatusCfg {
            right: vec![Widget::Bare("windows".into())],
            ..Default::default()
        };
        let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Top);
        p.set_windows(wins(&[("shell", true)]));
        assert!(p.set_notice(Notice::warn("pane 4: exited 127"), now));
        paint(&mut p, BarInset::NONE, 40, 24, "main");
        assert_eq!(p.hit_at(39), None, "the toast masks the tab beneath it");
    }

    /// The error line outranks a notice, and an empty bar never reserves a
    /// row for one.
    #[test]
    fn notices_are_refused_without_a_bar_row_of_their_own() {
        let now = std::time::Instant::now();
        let mut p = StatusBarPainter::error_line("config error: boom");
        assert!(!p.set_notice(Notice::warn("pane 3: exited 137"), now));
        let v = visible(&mut p, 40, 24, "");
        assert!(
            v.contains("config error") && !v.contains("exited 137"),
            "{v:?}"
        );

        let mut p = StatusBarPainter::new(build_bar(&StatusCfg::default()), Position::Bottom);
        assert!(!p.set_notice(Notice::warn("federation degraded"), now));
        assert!(p.is_empty());
        assert_eq!(p.min_poll_interval(), None);
        assert!(paint(&mut p, BarInset::NONE, 80, 24, "").is_empty());
    }

    #[test]
    fn badges_mask_underlying_click_targets_in_an_inset_bar() {
        use phux_config::widget::CellHit;
        for hit in [CellHit::Switch, CellHit::Window(2)] {
            let mut p = windows_bar();
            p.supervisory = Some("[ FROZEN ]".to_owned());
            p.attention = Some("[ ASK ]".to_owned());
            let row = vec![
                WidgetCell {
                    hit: Some(hit),
                    ..WidgetCell::default()
                };
                24
            ];
            p.last_row = Some((36, 24, row));
            for col in 0..24 {
                // 7 attention cells, a blank separator, 10 badge cells.
                assert_eq!(p.hit_at(36 + col), if col < 6 { Some(hit) } else { None });
            }
            assert_eq!(p.badge_gap(24), Some(13));
            assert_eq!(p.hit_at(35), None);
            assert_eq!(p.hit_at(60), None);
        }
    }

    /// The attention chip right-aligns in the theme color, shifts left of a
    /// supervisory badge, and stops painting once cleared.
    #[test]
    fn attention_hint_placement_and_clearing() {
        let mut p = windows_bar();
        p.set_windows(wins(&[("a", true)]));
        assert!(visible(&mut p, 40, 10, "").contains("0:a"));
        p.set_attention_color(Color::Rgb(251, 191, 36));
        assert!(p.set_attention(Some("[ ASK ]".to_owned())));
        assert!(!p.set_attention(Some("[ ASK ]".to_owned())));
        let s = paint(&mut p, BarInset::NONE, 40, 10, "");
        assert!(
            s.contains("\x1b[10;34H") && s.contains("\x1b[38;2;251;191;36m"),
            "{s:?}"
        );
        assert!(strip_csi(&s).contains("[ ASK ]"));

        assert!(p.set_supervisory(Some("[ FROZEN ]".to_owned())));
        let s = paint(&mut p, BarInset::NONE, 40, 10, "");
        // Badge at cols 31..40; the chip right-aligns 11 cells further left.
        assert!(
            s.contains("\x1b[10;31H") && s.contains("\x1b[10;23H"),
            "{s:?}"
        );

        assert!(p.set_attention(None));
        assert!(!visible(&mut p, 40, 10, "").contains("ASK"));
    }

    /// Painter-owned focused-pane state feeds the `cwd` and `exit` widgets;
    /// exec feed output lands on the next paint.
    #[test]
    fn painter_state_feeds_cwd_exit_and_exec_widgets() {
        let cfg = StatusCfg {
            left: vec![
                spec("cwd", &[]),
                spec(
                    "exec",
                    &[("command", toml::Value::String("battery.sh".into()))],
                ),
            ],
            right: vec![spec(
                "exit",
                &[("format", toml::Value::String("rc={code}".into()))],
            )],
            ..Default::default()
        };
        let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        let feeds = p.exec_feeds();
        assert_eq!(feeds.len(), 1);
        let v = visible(&mut p, 60, 24, "");
        assert!(
            !v.contains("/tmp") && !v.contains("rc=") && !v.contains("BAT"),
            "{v:?}"
        );

        assert!(p.set_focused_cwd(Some("/tmp/project".to_owned())));
        assert!(!p.set_focused_cwd(Some("/tmp/project".to_owned())));
        assert!(p.set_last_exit(Some(127)));
        feeds[0].apply_output("BAT 87%\n");
        let v = visible(&mut p, 60, 24, "");
        assert!(
            v.contains("/tmp/project") && v.contains("rc=127") && v.contains("BAT 87%"),
            "{v:?}"
        );

        assert!(p.set_last_exit(None));
        assert!(!visible(&mut p, 60, 24, "").contains("rc="));
    }

    fn tab_underlined(buf: &Buffer, start: u16, end: u16) -> bool {
        (start..=end).all(|x| buf[(x, 0)].modifier.contains(Modifier::UNDERLINED))
    }

    /// "0:bash 1:vim" puts window 0 on columns 0..=5 and window 1 on
    /// 7..=11; separator and padding are inert. The map follows repaints and
    /// a live drag marker never steals hits.
    #[test]
    fn window_hit_at_maps_painted_tab_columns() {
        let mut p = windows_bar();
        p.set_windows(wins(&[("bash", true), ("vim", false)]));
        assert_eq!(p.window_hit_at(0), None, "nothing painted yet");
        paint(&mut p, BarInset::NONE, 40, 10, "");
        let expect = |x: u16| match x {
            0..=5 => Some(0),
            7..=11 => Some(1),
            _ => None,
        };
        for x in 0..=40 {
            assert_eq!(p.window_hit_at(x), expect(x), "col {x}");
        }

        let ctx = ctx_default("");
        let compose =
            |p: &mut StatusBarPainter| p.compose_buffer(BarInset::NONE, 40, 10, &ctx).unwrap().0;
        assert!(!tab_underlined(&compose(&mut p), 7, 11));
        assert!(p.set_drop_index(Some(1)));
        assert!(!p.set_drop_index(Some(1)));
        let marked = compose(&mut p);
        assert!(tab_underlined(&marked, 7, 11) && !tab_underlined(&marked, 0, 5));
        paint(&mut p, BarInset::NONE, 40, 10, "");
        assert!((7..=11).all(|x| p.window_hit_at(x) == Some(1)));
        assert!(p.set_drop_index(None));
        assert!(!tab_underlined(&compose(&mut p), 7, 11));

        p.set_windows(wins(&[("a", false), ("b", true)]));
        paint(&mut p, BarInset::NONE, 40, 10, "");
        assert_eq!(p.window_hit_at(4), Some(1), "the fresh paint's tabs");
    }

    /// A left sidebar shifts the bar beside the strip (hits map through the
    /// shifted origin); a right one narrows it in place, badge included.
    #[test]
    fn sidebar_insets_shift_or_narrow_the_bar() {
        let mut p = windows_bar();
        p.set_windows(wins(&[("bash", true), ("vim", false)]));
        let s = paint(&mut p, BarInset { left: 20, right: 0 }, 40, 10, "");
        assert!(
            s.contains("\x1b[10;21H") && !s.contains("\x1b[10;1H"),
            "{s:?}"
        );
        assert_eq!(
            p.last_row.as_ref().map(|(x, w, _)| (*x, *w)),
            Some((20, 20))
        );
        for x in 0..40 {
            let expect = match x {
                20..=25 => Some(0),
                27..=31 => Some(1),
                _ => None,
            };
            assert_eq!(p.window_hit_at(x), expect, "col {x}");
        }

        let mut p = windows_bar();
        p.set_windows(wins(&[("bash", true)]));
        p.set_supervisory(Some("[F]".to_owned()));
        let s = paint(&mut p, BarInset { left: 0, right: 20 }, 40, 10, "");
        assert!(
            s.contains("\x1b[10;1H") && s.contains("\x1b[10;18H\x1b[7;1m[F]"),
            "{s:?}"
        );
        assert_eq!(p.last_row.as_ref().map(|(x, w, _)| (*x, *w)), Some((0, 20)));
    }

    #[test]
    fn painter_threads_configured_prefix_to_help_hints_widget() {
        let cfg = StatusCfg {
            center: vec![spec("help-hints", &[])],
            ..Default::default()
        };
        let mut painter = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
        painter.set_prefix("C-b");
        let v = visible(&mut painter, 80, 24, "");
        assert!(v.contains("C-b  s Sessions") && !v.contains("C-a"), "{v:?}");
    }

    /// Cells a widget leaves without a background take the bar fill; a tab
    /// with its own keeps it.
    #[test]
    fn the_bar_takes_its_fill_where_widgets_leave_no_background() {
        let bed = toml::Value::Table(toml::value::Table::from_iter([(
            "bg".to_owned(),
            toml::Value::String("#293628".to_owned()),
        )]));
        let cfg = StatusCfg {
            left: vec![spec("windows", &[("active", bed)])],
            right: vec![spec("session-name", &[])],
            ..StatusCfg::default()
        };
        let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Top);
        p.set_windows(wins(&[("zsh", true)]));
        let fill = Color::Rgb(0x17, 0x1b, 0x23);
        p.set_fill(fill);
        let (buf, _, _) = p
            .compose_buffer(BarInset::NONE, 40, 10, &ctx_default("main"))
            .expect("composes");
        assert_eq!(buf[(0, 0)].bg, Color::Rgb(0x29, 0x36, 0x28));
        assert!((10..40).all(|x| buf[(x, 0)].bg == fill));
    }
}
