//! Pane dividers, pane titles, and the pane-grid rail.
//!
//! Panes share their rules (one cell of chrome per split), and the grid is
//! closed at the top by a rail row carrying each pane's title. Every rule is
//! the light box-drawing set (heavy focus rules forced mixed-weight
//! junctions most fonts cannot draw); focus is colour: the focused pane's
//! frame and title take `divider_focus`, the rest `divider`. Emphasis is
//! resolved here from the client's focused pane, not the layout tree's
//! remembered focus, which lags.
//!
//! The composer collects only chrome cells and emits them as positioned,
//! style-coalesced runs between SGR resets; it never writes inside a pane
//! interior (the ADR-0020 skip-cell carve-out: libghostty owns those cells)
//! and leaves the cursor to the focused pane's render.

use std::io::{self, Write};

use phux_protocol::ResourceId;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect as RataRect;
use ratatui::style::{Color, Modifier, Style};

use crate::attach::multi_pane::PaneLayout;
use crate::layout::Rect;
use crate::render::chrome::{AgentBadge, agent_fact, pane_badge};
use crate::render::theme::Theme;
use crate::render::{ELLIPSIS, cell_width};
use phux_client::agent_meta::AgentMetaState;

/// Cells of chrome a pane title needs before any of its label shows:
/// one lead-in `\u{2500}`, a space, a space, and the closing `\u{2500}` the run
/// continues into. A pane narrower than this simply gets no title.
const TITLE_CHROME_CELLS: u16 = 4;

/// UTF-8 bytes one chrome cell can hold: a base plus a few combining marks
/// (a longer cluster keeps the marks that fit).
const CELL_BYTES: usize = 16;

/// What to write into one pane's top rule, borrowed from the caller's cached
/// title.
#[derive(Debug, Clone, Copy)]
pub struct PaneLabel<'a> {
    /// The pane's display name — its OSC-2 title in practice.
    pub text: &'a str,
    /// The pane's declared agent lifecycle state, when it runs an agent
    /// (ADR-0040). `None` for an ordinary shell pane.
    pub agent: Option<AgentMetaState>,
    /// `true` when the pane is waiting on a human (ADR-0035 asked).
    pub attention: bool,
    /// `true` once the user has visited the pane since its last state
    /// change; drives the "finished but unread" badge.
    pub seen: bool,
    /// Satellite that hosts this pane, drawn as a badge ahead of the title.
    /// `None` for a pane on the attached server.
    pub host: Option<&'a str>,
    /// The satellite link is down: title and badge draw in the recessive
    /// divider tone, and the pane's frame is not the focus colour.
    pub unreachable: bool,
    /// Transient origin of the latest input (`input_activity`), drawn as
    /// `[name]` and dimmed once the fade has started.
    pub origin: Option<OriginBadge<'a>>,
    /// Rows between the viewport's bottom and the live tail while the user
    /// has scrolled into history; `None` at the tail. Drawn as `↑ N` at the
    /// right end of the rule.
    pub scrolled_back: Option<u64>,
    /// The retained pane's exit status (ADR-0124), once it exited: nonzero
    /// wears the failed mark, zero the ended (or done) one.
    pub exit: Option<i32>,
}

/// A fading `[origin]` mark inset into the pane's title rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OriginBadge<'a> {
    /// The origin label. Empty draws nothing.
    pub text: &'a str,
    /// `true` once the badge has left its bright interval.
    pub dim: bool,
}

impl PaneLabel<'_> {
    /// The badge to draw ahead of the label, if any: the pane's ladder
    /// state through [`phux_design::tone_of`]. A plain shell pane that is
    /// alive and quiet wears none.
    fn badge(&self, theme: &Theme) -> Option<AgentBadge> {
        match (self.agent, self.attention, self.exit) {
            (None, false, None) => None,
            (agent, attention, exit) => Some(pane_badge(
                theme,
                &phux_design::ToneInput {
                    agent: agent.map(agent_fact),
                    attention,
                    exit,
                    seen: self.seen,
                    ..phux_design::ToneInput::default()
                },
            )),
        }
    }
}

// -----------------------------------------------------------------------------
// Cell buffer
// -----------------------------------------------------------------------------

/// One cell's grapheme, stored inline so composing a frame allocates nothing
/// per cell.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Sym {
    buf: [u8; CELL_BYTES],
    len: u8,
}

impl Sym {
    /// A positioned cell that paints nothing: the trailing half of a wide
    /// glyph. The emitter skips it and breaks its run, so the next cell
    /// re-anchors with its own CUP.
    const EMPTY: Self = Self {
        buf: [0; CELL_BYTES],
        len: 0,
    };

    /// Store `s`, truncated to whole UTF-8 characters that fit.
    fn new(s: &str) -> Self {
        let mut out = Self::EMPTY;
        for ch in s.chars() {
            out.push(ch);
        }
        out
    }

    /// Append `ch` if the remaining room holds it; otherwise drop it.
    fn push(&mut self, ch: char) {
        let len = usize::from(self.len);
        let room = CELL_BYTES - len;
        if ch.len_utf8() > room {
            return;
        }
        let written = ch.encode_utf8(&mut self.buf[len..]).len();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "len + written <= CELL_BYTES = 16, which fits u8"
        )]
        {
            self.len = (len + written) as u8;
        }
    }

    fn as_str(&self) -> &str {
        // Every push wrote whole chars, so this cannot fail; a failure would
        // degrade to an unpainted cell.
        std::str::from_utf8(&self.buf[..usize::from(self.len)]).unwrap_or("")
    }

    const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// One painted chrome cell.
#[derive(Clone, Copy)]
struct ChromeCell {
    x: u16,
    y: u16,
    sym: Sym,
    style: Style,
}

/// The chrome cells of one frame in paint order: a few hundred cells, not a
/// viewport-sized buffer, so a title change is not an O(viewport) event.
#[derive(Default)]
struct ChromeCells {
    cells: Vec<ChromeCell>,
}

impl ChromeCells {
    fn clear(&mut self) {
        self.cells.clear();
    }

    /// Lay `fill` under every chrome cell: [`Theme::ground_fill`], the
    /// surface when the terminal's background is unknown, so no rule or
    /// title ink sits on a ground it was not solved against.
    fn fill_ground(&mut self, fill: Color) {
        if fill == Color::Reset {
            return;
        }
        for cell in &mut self.cells {
            cell.style = cell.style.bg(fill);
        }
    }

    /// Record one cell. A later write to the same coordinate wins, which
    /// is what lets a title overwrite the rule the rail laid down first.
    fn put(&mut self, x: u16, y: u16, sym: Sym, style: Style) {
        self.cells.push(ChromeCell { x, y, sym, style });
    }

    fn put_str(&mut self, x: u16, y: u16, symbol: &str, style: Style) {
        self.put(x, y, Sym::new(symbol), style);
    }

    /// Sort for emission and drop duplicates (last write wins) and anything
    /// inside a pane interior: the skip-cell carve-out, applied once here.
    fn finalize(&mut self, layout: &PaneLayout) {
        let (cols, rows) = layout.viewport;
        self.cells
            .retain(|c| c.x < cols && c.y < rows && !in_any_pane(layout, c.x, c.y));
        // Stable, so the insertion order of equal coordinates survives
        // and "last write wins" is well defined.
        self.cells.sort_by_key(|c| (c.y, c.x));
        let mut kept: usize = 0;
        for i in 0..self.cells.len() {
            let last_of_run = i + 1 == self.cells.len()
                || (self.cells[i + 1].y, self.cells[i + 1].x) != (self.cells[i].y, self.cells[i].x);
            if last_of_run {
                self.cells[kept] = self.cells[i];
                kept += 1;
            }
        }
        self.cells.truncate(kept);
    }
}

/// `true` when `(x, y)` sits inside any pane's interior rectangle.
fn in_any_pane(layout: &PaneLayout, x: u16, y: u16) -> bool {
    layout
        .rects
        .values()
        .any(|r| x >= r.x && y >= r.y && x < r.x.saturating_add(r.w) && y < r.y.saturating_add(r.h))
}

thread_local! {
    /// Scratch cells reused across frames (single-threaded, never
    /// re-entered).
    static SCRATCH: std::cell::RefCell<ChromeCells> =
        std::cell::RefCell::new(ChromeCells::default());
}

// -----------------------------------------------------------------------------
// Public entry points
// -----------------------------------------------------------------------------

/// Render the divider layer for `layout` to `out`. `content` and `rail` are
/// `attach::paint::content_layout`'s; `label_of` titles each pane. Emits
/// SGR-reset-bracketed runs, never into a pane interior.
///
/// # Errors
///
/// Forwards any `io::Error` from `out`.
pub fn render_dividers<'p, W, F>(
    out: &mut W,
    layout: &PaneLayout,
    content: Rect,
    rail: Option<u16>,
    focused: Option<&ResourceId>,
    theme: &Theme,
    label_of: F,
) -> io::Result<()>
where
    W: Write,
    F: Fn(&ResourceId) -> Option<PaneLabel<'p>>,
{
    let (cols, rows) = layout.viewport;
    if cols == 0 || rows == 0 {
        return Ok(());
    }
    if layout.dividers.is_empty() && rail.is_none_or(|y| y >= rows) {
        return Ok(());
    }

    SCRATCH.with(|scratch| {
        let mut cells = scratch.borrow_mut();
        cells.clear();
        build_cells(&mut cells, layout, content, rail, focused, theme, label_of);
        cells.fill_ground(theme.ground_fill());
        cells.finalize(layout);
        emit_cells(out, &cells.cells)
    })
}

/// Build the viewport-sized chrome buffer (the cold path: tests and the
/// `phux snapshot --rendered` compositor; live paints never build it).
pub(crate) fn compose_buffer<'p, F>(
    layout: &PaneLayout,
    content: Rect,
    rail: Option<u16>,
    focused: Option<&ResourceId>,
    theme: &Theme,
    label_of: F,
) -> Buffer
where
    F: Fn(&ResourceId) -> Option<PaneLabel<'p>>,
{
    let (cols, rows) = layout.viewport;
    let mut buf = Buffer::empty(RataRect::new(0, 0, cols, rows));
    // Everything not painted below belongs to someone else, so the whole
    // viewport starts skipped.
    for y in 0..rows {
        for x in 0..cols {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_diff_option(CellDiffOption::Skip);
            }
        }
    }
    let mut cells = ChromeCells::default();
    build_cells(&mut cells, layout, content, rail, focused, theme, label_of);
    cells.fill_ground(theme.ground_fill());
    cells.finalize(layout);
    for c in &cells.cells {
        if let Some(cell) = buf.cell_mut((c.x, c.y)) {
            cell.set_symbol(c.sym.as_str());
            cell.set_style(c.style);
            cell.set_diff_option(CellDiffOption::None);
        }
    }
    buf
}

// -----------------------------------------------------------------------------
// Composition
// -----------------------------------------------------------------------------

/// Compose one frame's chrome: rules, then the rail, then titles over
/// both.
fn build_cells<'p, F>(
    cells: &mut ChromeCells,
    layout: &PaneLayout,
    content: Rect,
    rail: Option<u16>,
    focused: Option<&ResourceId>,
    theme: &Theme,
    label_of: F,
) where
    F: Fn(&ResourceId) -> Option<PaneLabel<'p>>,
{
    // A rule is on the focused frame exactly when it borders the focused
    // rect; an unreachable satellite pane loses the focus colour.
    let focused_down =
        focused.is_some_and(|id| label_of(id).is_some_and(|label| label.unreachable));
    // A lone pane has no neighbour to be told apart from, so its frame stays
    // in the structural tone: lime is a signal, and a signal that is always
    // on across the full width says nothing.
    let alone = layout.rects.values().filter(|r| r.w > 0 && r.h > 0).count() < 2;
    let frame = if focused_down || alone {
        None
    } else {
        focused.and_then(|id| layout.rects.get(id)).copied()
    };

    for cell in &layout.dividers {
        let mut sym = Sym::EMPTY;
        sym.push(cell.ch);
        cells.put(
            cell.x,
            cell.y,
            sym,
            rule_style(theme, on_frame(frame, cell.x, cell.y)),
        );
    }
    draw_rail(cells, layout, content, rail, frame, theme);
    draw_titles(cells, layout, rail, focused, theme, label_of);
}

/// The style of one rule cell.
fn rule_style(theme: &Theme, focused: bool) -> Style {
    if focused {
        Style::default().fg(theme.divider_focus)
    } else {
        Style::default().fg(theme.divider)
    }
}

/// Draw the rail across the top of the pane area: `┬` where a divider drops
/// out of it, `─` elsewhere, with the same focus tint as the rules. Two
/// linear passes, not O(columns x dividers).
fn draw_rail(
    cells: &mut ChromeCells,
    layout: &PaneLayout,
    content: Rect,
    rail: Option<u16>,
    frame: Option<Rect>,
    theme: &Theme,
) {
    let Some(y) = rail else {
        return;
    };
    let (cols, rows) = layout.viewport;
    if y >= rows || content.w == 0 || content.h == 0 {
        return;
    }
    let x1 = content.x.saturating_add(content.w).min(cols);
    for x in content.x..x1 {
        cells.put_str(x, y, "\u{2500}", rule_style(theme, on_frame(frame, x, y)));
    }
    for cell in &layout.dividers {
        if cell.y == content.y
            && cell.x >= content.x
            && cell.x < x1
            && matches!(
                cell.ch,
                '\u{2502}' | '\u{251c}' | '\u{2524}' | '\u{253c}' | '\u{252c}'
            )
        {
            let style = rule_style(theme, on_frame(frame, cell.x, y));
            cells.put_str(cell.x, y, "\u{252c}", style);
        }
    }
}

/// Whether `(x, y)` is on the ring of chrome cells around the focused rect
/// (corners included): the whole, deliberately geometric, focus model.
fn on_frame(frame: Option<Rect>, x: u16, y: u16) -> bool {
    frame.is_some_and(|r| {
        let left = r.x.saturating_sub(1);
        let right = r.x.saturating_add(r.w);
        let top = r.y.saturating_sub(1);
        let bottom = r.y.saturating_add(r.h);
        let in_rows = y >= top && y <= bottom;
        let in_cols = x >= left && x <= right;
        in_rows && in_cols && (x == left || x == right || y == top || y == bottom)
    })
}

/// Inset each pane's title into the rule above it.
fn draw_titles<'p, F>(
    cells: &mut ChromeCells,
    layout: &PaneLayout,
    rail: Option<u16>,
    focused: Option<&ResourceId>,
    theme: &Theme,
    label_of: F,
) where
    F: Fn(&ResourceId) -> Option<PaneLabel<'p>>,
{
    for (id, rect) in &layout.rects {
        // A zero-height leaf has no pane to label — and it shares its
        // `y` with the leaf below it, so labelling it would make which
        // title survives depend on hash iteration order.
        if rect.h == 0 || rect.w <= TITLE_CHROME_CELLS {
            continue;
        }
        // The rule above the pane (rail or interior divider); none for a
        // pane at row 0 or under an unpainted reserved row.
        let Some(y) = rect.y.checked_sub(1) else {
            continue;
        };
        if y >= layout.viewport.1 || (rail != Some(y) && !layout.dividers.iter().any(|d| d.y == y))
        {
            continue;
        }
        let Some(label) = label_of(id) else {
            continue;
        };
        draw_one_title(cells, *rect, y, &label, theme, focused == Some(id));
    }
}

/// Write ` <badge> <label> ` into the rule at row `y`, starting one cell
/// inside the pane's left edge.
fn draw_one_title(
    cells: &mut ChromeCells,
    rect: Rect,
    y: u16,
    label: &PaneLabel<'_>,
    theme: &Theme,
    focused: bool,
) {
    let text = label.text.trim();
    let host = label.host.map(str::trim).filter(|host| !host.is_empty());
    let mut origin = label.origin.filter(|origin| !origin.text.trim().is_empty());
    let scroll = draw_scroll_mark(cells, rect, y, label.scrolled_back, theme, focused);
    if text.is_empty() && host.is_none() && origin.is_none() {
        return;
    }
    let badge = label.badge(theme);
    // Budget: the pane's width, less the lead-in rule cell, the two
    // padding spaces, one closing rule cell, and the scroll mark's cells.
    let mut budget = usize::from(rect.w - TITLE_CHROME_CELLS).saturating_sub(scroll);
    // Measure the badge: `put_measured` writes it at its real width.
    let badge_cells = badge.map_or(0, |b| text_columns(b.glyph) + 1);
    // Host badge plus the space that separates it from the title.
    let host_cells = host.map_or(0, |host| text_columns(host) + 1);
    if budget <= badge_cells + host_cells {
        return;
    }
    budget -= badge_cells + host_cells;
    // `[origin]` plus the space before the title. Drop it when it will not fit.
    if let Some(mark) = origin {
        let need = text_columns(mark.text.trim()) + 3;
        if budget <= need {
            origin = None;
        } else {
            budget -= need;
        }
    }

    let muted = label.unreachable;
    let title_style = if muted {
        Style::default().fg(theme.divider)
    } else if focused {
        Style::default().fg(theme.pane_title_focus)
    } else {
        Style::default().fg(theme.pane_title)
    };
    let host_style = Style::default().fg(if muted { theme.divider } else { theme.chord });
    let pad = Style::default().fg(if focused && !muted {
        theme.divider_focus
    } else {
        theme.divider
    });

    let mut x = rect.x + 1;
    x = put_measured(cells, x, y, " ", pad);
    if let Some(b) = badge {
        let mut style = Style::default().fg(b.color);
        if b.emphatic {
            style = style.add_modifier(Modifier::BOLD);
        }
        x = put_measured(cells, x, y, b.glyph, style);
        x = put_measured(cells, x, y, " ", pad);
    }
    if let Some(host) = host {
        x = put_clipped(cells, x, y, host, text_columns(host), host_style);
        x = put_measured(cells, x, y, " ", pad);
    }
    if let Some(mark) = origin {
        let mark_text = mark.text.trim();
        let mut style = Style::default().fg(if mark.dim { theme.dim } else { theme.chord });
        if !mark.dim {
            style = style.add_modifier(Modifier::BOLD);
        }
        x = put_measured(cells, x, y, "[", style);
        x = put_clipped(cells, x, y, mark_text, text_columns(mark_text), style);
        x = put_measured(cells, x, y, "]", style);
        x = put_measured(cells, x, y, " ", pad);
    }
    if !text.is_empty() {
        x = put_clipped(cells, x, y, text, budget, title_style);
        put_measured(cells, x, y, " ", pad);
    }
}

/// Inset ` ↑ N ` at the rule's right end when the pane is scrolled back,
/// returning the cells taken off the title's budget (or 0).
fn draw_scroll_mark(
    cells: &mut ChromeCells,
    rect: Rect,
    y: u16,
    rows: Option<u64>,
    theme: &Theme,
    focused: bool,
) -> usize {
    let Some(rows) = rows.filter(|&rows| rows > 0) else {
        return 0;
    };
    let mark = format!("↑ {rows}");
    let width = text_columns(&mark) + 2;
    // The mark, its two pads, and a rule cell on each side.
    if usize::from(rect.w) < width + 2 {
        return 0;
    }
    let x = rect.x + rect.w - 1 - u16::try_from(width).unwrap_or(u16::MAX);
    let pad = Style::default().fg(if focused {
        theme.divider_focus
    } else {
        theme.divider
    });
    let style = Style::default()
        .fg(if focused {
            theme.pane_title_focus
        } else {
            theme.pane_title
        })
        .add_modifier(Modifier::BOLD);
    let x = put_measured(cells, x, y, " ", pad);
    let x = put_measured(cells, x, y, &mark, style);
    put_measured(cells, x, y, " ", pad);
    width + 1
}

/// The columns `text` advances, counting only what may reach the wire (the
/// one width measure for reservations and writes).
fn text_columns(text: &str) -> usize {
    text.chars().filter_map(cell_width).sum()
}

/// Write a symbol that is expected to advance one cell, blanking any
/// trailing cells it actually claims. Returns the next column.
fn put_measured(cells: &mut ChromeCells, x: u16, y: u16, symbol: &str, style: Style) -> u16 {
    let width = text_columns(symbol).max(1);
    cells.put_str(x, y, symbol, style);
    blank_continuations(cells, x, y, width, style)
}

/// Reserve the trailing cells of a `width`-wide glyph with [`Sym::EMPTY`]
/// and return the next column. That keeps the rule from showing through the
/// glyph's right half and re-anchors the emitter, which would otherwise
/// drift right per wide glyph and wrap into the pane (ADR-0020).
fn blank_continuations(cells: &mut ChromeCells, x: u16, y: u16, width: usize, style: Style) -> u16 {
    let mut cur = x.saturating_add(1);
    for _ in 1..width {
        cells.put(cur, y, Sym::EMPTY, style);
        cur = cur.saturating_add(1);
    }
    cur
}

/// Write `text` clipped to `budget` display cells, marking a cut with
/// [`ELLIPSIS`]; returns the next column. Zero-width marks join their base
/// cell (stopping at one once truncated titles silently); bidi controls are
/// refused by [`cell_width`], so an untrusted title cannot reorder labels.
fn put_clipped(
    cells: &mut ChromeCells,
    x: u16,
    y: u16,
    text: &str,
    budget: usize,
    style: Style,
) -> u16 {
    if budget == 0 {
        return x;
    }
    let total: usize = text.chars().filter_map(cell_width).sum();
    let fits = total <= budget;
    // When it does not fit, the last cell is spent on the ellipsis.
    let text_budget = if fits { budget } else { budget - 1 };

    let mut cur = x;
    let mut used = 0usize;
    // Index into `cells` of the cell zero-width marks attach to.
    let mut base: Option<usize> = None;
    for ch in text.chars() {
        let Some(w) = cell_width(ch) else {
            continue;
        };
        if w == 0 {
            if let Some(i) = base {
                cells.cells[i].sym.push(ch);
            }
            continue;
        }
        if used + w > text_budget {
            break;
        }
        let mut sym = Sym::EMPTY;
        sym.push(ch);
        base = Some(cells.cells.len());
        cells.put(cur, y, sym, style);
        cur = blank_continuations(cells, cur, y, w, style);
        used += w;
    }
    if !fits {
        cur = put_measured(cells, cur, y, ELLIPSIS.encode_utf8(&mut [0u8; 4]), style);
    }
    cur
}

// -----------------------------------------------------------------------------
// Emission
// -----------------------------------------------------------------------------

/// Emit `cells` as positioned VT: consecutive same-style cells in a row are
/// one CUP + SGR run; a gap, style change, or empty symbol re-anchors.
fn emit_cells<W: Write>(out: &mut W, cells: &[ChromeCell]) -> io::Result<()> {
    out.write_all(b"\x1b[0m")?;
    let mut style: Option<Style> = None;
    // The column the current run would continue at; `None` = no open run.
    let mut run: Option<(u16, u16)> = None;
    for cell in cells {
        if cell.sym.is_empty() {
            run = None;
            continue;
        }
        if style != Some(cell.style) {
            write_style(out, cell.style)?;
            style = Some(cell.style);
            run = None;
        }
        if run != Some((cell.y, cell.x)) {
            // CUP is 1-based.
            write!(
                out,
                "\x1b[{};{}H",
                cell.y.saturating_add(1),
                cell.x.saturating_add(1)
            )?;
        }
        out.write_all(cell.sym.as_str().as_bytes())?;
        run = Some((cell.y, cell.x.saturating_add(1)));
    }
    // Trailing reset so the next layer (status bar, focused pane render)
    // doesn't inherit any chrome SGR.
    out.write_all(b"\x1b[0m")?;
    out.flush()
}

/// Emit the SGR for one chrome cell: a full reset, then bold, then the
/// foreground and any ground fill. The reset is what makes a run boundary
/// cheap to reason about — no attribute can survive from the previous run.
fn write_style<W: Write>(out: &mut W, style: Style) -> io::Result<()> {
    out.write_all(b"\x1b[0m")?;
    if style.add_modifier.contains(Modifier::BOLD) {
        out.write_all(b"\x1b[1m")?;
    }
    if let Some(color) = style.fg {
        crate::render::sgr::write_sgr_color(out, color, true)?;
    }
    match style.bg {
        None | Some(Color::Reset) => Ok(()),
        Some(color) => crate::render::sgr::write_sgr_color(out, color, false),
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests;
