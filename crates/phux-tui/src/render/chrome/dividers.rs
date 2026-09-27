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
use crate::render::chrome::{AgentBadge, agent_badge, attention_badge};
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
}

impl PaneLabel<'_> {
    /// The badge to draw ahead of the label, if any.
    fn badge(&self, theme: &Theme) -> Option<AgentBadge> {
        match (self.agent, self.attention) {
            (Some(state), _) => Some(agent_badge(theme, state, self.attention, self.seen)),
            (None, true) => Some(attention_badge(theme)),
            (None, false) => None,
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

/// Render the divider layer for `layout` to `out`.
///
/// `content` is the tiled pane area and `rail` the row reserved above it, as
/// `attach::paint::content_layout` reports (not inferred from `content.y`: a
/// top bar also pushes content down). `focused` is the client's focused
/// pane; `label_of` gives each pane's title. Emits SGR-reset-bracketed runs
/// and no final cursor position; never writes into a pane interior.
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
    if text.is_empty() && host.is_none() {
        return;
    }
    let badge = label.badge(theme);
    // Budget: the pane's width, less the lead-in rule cell, the two
    // padding spaces, and one closing rule cell.
    let mut budget = usize::from(rect.w - TITLE_CHROME_CELLS);
    // Measure the badge: `put_measured` writes it at its real width.
    let badge_cells = badge.map_or(0, |b| text_columns(b.glyph) + 1);
    // Host badge plus the space that separates it from the title.
    let host_cells = host.map_or(0, |host| text_columns(host) + 1);
    if budget <= badge_cells + host_cells {
        return;
    }
    budget -= badge_cells + host_cells;

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
    if !text.is_empty() {
        x = put_clipped(cells, x, y, text, budget, title_style);
        put_measured(cells, x, y, " ", pad);
    }
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
/// foreground. The reset is what makes a run boundary cheap to reason
/// about — no attribute can survive from the previous run.
fn write_style<W: Write>(out: &mut W, style: Style) -> io::Result<()> {
    out.write_all(b"\x1b[0m")?;
    if style.add_modifier.contains(Modifier::BOLD) {
        out.write_all(b"\x1b[1m")?;
    }
    match style.fg {
        None | Some(Color::Reset) => Ok(()),
        Some(color) => crate::render::sgr::write_sgr_color(out, color, true),
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    use std::collections::HashMap;

    use crate::attach::multi_pane::{DividerCell, compute_layout_in};
    use crate::layout::{LayoutNode, LayoutState, SplitDir, split_at};

    fn t(id: u32) -> ResourceId {
        ResourceId::local(id)
    }

    fn leaf(id: u32) -> LayoutNode {
        LayoutNode::Leaf(t(id))
    }

    /// A pane area with one rail row above it, the shipped shape.
    const fn railed(cols: u16, rows: u16) -> Rect {
        Rect {
            x: 0,
            y: 1,
            w: cols,
            h: rows - 1,
        }
    }

    fn theme() -> Theme {
        Theme::default()
    }

    /// The rail row: the one above the pane area, if any.
    fn rail_row(content: Rect) -> Option<u16> {
        content.y.checked_sub(1)
    }

    fn label(text: &'static str) -> PaneLabel<'static> {
        PaneLabel {
            text,
            agent: None,
            attention: false,
            seen: true,
            host: None,
            unreachable: false,
        }
    }

    /// Render with every knob explicit.
    fn render_full(
        layout: &PaneLayout,
        content: Rect,
        rail: Option<u16>,
        focus: Option<&ResourceId>,
        pane_label: Option<PaneLabel<'static>>,
    ) -> String {
        let mut bytes: Vec<u8> = Vec::new();
        render_dividers(&mut bytes, layout, content, rail, focus, &theme(), |_| {
            pane_label
        })
        .unwrap();
        String::from_utf8(bytes).unwrap()
    }

    /// Render focused on pane 1 with `pane_label` on every pane.
    fn render_label(
        layout: &PaneLayout,
        content: Rect,
        pane_label: Option<PaneLabel<'static>>,
    ) -> String {
        render_full(layout, content, rail_row(content), Some(&t(1)), pane_label)
    }

    /// Render with a plain text label on every pane.
    fn render(layout: &PaneLayout, content: Rect, text: Option<&'static str>) -> String {
        render_label(layout, content, text.map(label))
    }

    fn state(tree: LayoutNode, focus: u32) -> LayoutState {
        LayoutState {
            tree: Some(tree),
            focus: Some(t(focus)),
        }
    }

    fn layout_of(state: &LayoutState, content: Rect) -> PaneLayout {
        compute_layout_in(state, content, (content.w, content.y + content.h))
    }

    /// `1 | 2`, focused on 1.
    fn split_layout(content: Rect) -> PaneLayout {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        layout_of(&state(tree, 1), content)
    }

    /// `(1 / 3) | 2`, focused on `focus`.
    fn cross_layout(content: Rect, focus: u32) -> PaneLayout {
        let t1 = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let t2 = split_at(&t1, &t(1), &t(3), SplitDir::Vertical, 0.5).unwrap();
        layout_of(&state(t2, focus), content)
    }

    fn lone_layout(content: Rect) -> PaneLayout {
        layout_of(&state(leaf(1), 1), content)
    }

    fn rect_contains(r: Rect, x: u16, y: u16) -> bool {
        x >= r.x && y >= r.y && x < r.x.saturating_add(r.w) && y < r.y.saturating_add(r.h)
    }

    /// No painted cell lands in a pane interior or past the area's right
    /// edge (a wide glyph that walked past it would wrap into a pane row).
    fn assert_chrome_only(layout: &PaneLayout, content: Rect, s: &str) {
        let right_edge = content.x.saturating_add(content.w);
        for (x, y, sym) in painted_cells(s) {
            assert!(x < right_edge, "{sym:?} at column {x}, past {right_edge}");
            for r in layout.rects.values() {
                assert!(
                    !rect_contains(*r, x, y),
                    "painted {sym:?} at ({x}, {y}) inside {r:?}"
                );
            }
        }
    }

    /// The highest column painted on the rail row.
    fn rail_extent(s: &str, content: Rect) -> u16 {
        let rail_y = rail_row(content).unwrap();
        painted_cells(s)
            .into_iter()
            .filter(|(_, y, _)| *y == rail_y)
            .map(|(x, _, _)| x)
            .max()
            .unwrap()
    }

    #[test]
    fn empty_or_zero_width_layouts_paint_nothing() {
        let empty = PaneLayout {
            viewport: (80, 24),
            rects: HashMap::new(),
            dividers: Vec::new(),
            divider_hits: Vec::new(),
        };
        let full = Rect {
            x: 0,
            y: 0,
            w: 80,
            h: 24,
        };
        assert!(render(&empty, full, None).is_empty());
        let zero = PaneLayout {
            viewport: (0, 24),
            dividers: vec![DividerCell {
                x: 0,
                y: 0,
                ch: '\u{2502}',
            }],
            ..empty
        };
        assert!(render(&zero, Rect { w: 0, ..full }, None).is_empty());
    }

    /// THE load-bearing skip-cell invariant: libghostty owns pane interiors
    /// (ADR-0020), so no chrome byte may land there, for a split, a cross
    /// split, or a wide title.
    #[test]
    fn chrome_never_paints_inside_a_pane() {
        let content = railed(80, 24);
        for layout in [split_layout(content), cross_layout(content, 2)] {
            let s = render(&layout, content, Some("shell"));
            assert!(!extract_cups(&s).is_empty());
            assert_chrome_only(&layout, content, &s);
            assert!(
                s.starts_with("\x1b[0m") && s.ends_with("\x1b[0m"),
                "SGR resets"
            );
        }
        let narrow = railed(40, 10);
        let layout = split_layout(narrow);
        let s = render(&layout, narrow, Some("日本語のペイン名前"));
        assert_chrome_only(&layout, narrow, &s);
        let rail_y = rail_row(narrow).unwrap();
        assert!(
            painted_cells(&s)
                .iter()
                .all(|(_, y, _)| *y == rail_y || layout.dividers.iter().any(|d| d.y == *y))
        );
    }

    /// Structural twin of the above: compose marks every interior cell skip
    /// and every divider cell with its glyph.
    #[test]
    fn compose_buffer_marks_pane_interiors_skip() {
        let content = railed(80, 24);
        let layout = split_layout(content);
        let buf = compose_buffer(
            &layout,
            content,
            rail_row(content),
            Some(&t(1)),
            &theme(),
            |_| None,
        );
        for r in layout.rects.values() {
            for y in r.y..r.y + r.h {
                for x in r.x..r.x + r.w {
                    assert!(
                        buf.cell((x, y)).unwrap().diff_option == CellDiffOption::Skip,
                        "({x}, {y})"
                    );
                }
            }
        }
        for d in &layout.dividers {
            let cell = buf.cell((d.x, d.y)).unwrap();
            assert!(
                cell.diff_option != CellDiffOption::Skip,
                "({}, {})",
                d.x,
                d.y
            );
            assert_eq!(cell.symbol().chars().next(), Some(d.ch));
        }
    }

    /// Focus is a colour, never a heavier stroke (mixed-weight junctions do
    /// not render); unfocused rules recede, and a lone pane is not tinted.
    #[test]
    fn focus_tints_rules_and_nothing_else() {
        let content = railed(80, 24);
        let s = render(&split_layout(content), content, None);
        assert!(!s.contains('\u{2503}') && !s.contains('\u{2501}') && s.contains('\u{2502}'));
        assert!(s.contains(&sgr_fg(theme().divider_focus)), "{s:?}");
        assert!(!s.contains("\x1b[1m"), "focus is colour, not bold");

        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let tree = split_at(&tree, &t(2), &t(3), SplitDir::Horizontal, 0.5).unwrap();
        let s = render(&layout_of(&state(tree, 1), content), content, None);
        assert!(s.contains(&sgr_fg(theme().divider)), "{s:?}");

        let s = render(&lone_layout(content), content, None);
        assert!(
            !s.contains(&sgr_fg(theme().divider_focus)) && s.contains(&sgr_fg(theme().divider))
        );
    }

    /// Emphasis follows the client's focused pane, not the layout tree's
    /// remembered focus (the two diverge until the layout is persisted).
    #[test]
    fn emphasis_follows_the_passed_focus_not_the_layout_tree() {
        let content = railed(80, 24);
        let layout = split_layout(content);
        let one = layout.rects[&t(1)];
        let two = layout.rects[&t(2)];
        let rail_y = rail_row(content).unwrap();
        for (focus, on, off) in [(2, two, one), (1, one, two)] {
            let cells = styled_cells(&render_full(
                &layout,
                content,
                Some(rail_y),
                Some(&t(focus)),
                None,
            ));
            assert!(
                cells[&(on.x + 2, rail_y)].1 && !cells[&(off.x + 2, rail_y)].1,
                "focus {focus}"
            );
        }
    }

    /// The rail tees into each vertical rule; without a reserved row (or
    /// with a top bar in the row above) no rail is painted at all.
    #[test]
    fn the_rail_tees_and_is_never_invented() {
        let content = railed(80, 24);
        let layout = split_layout(content);
        let cells: HashMap<(u16, u16), String> = painted_cells(&render(&layout, content, None))
            .into_iter()
            .map(|(x, y, s)| ((x, y), s))
            .collect();
        let col = layout.dividers.first().unwrap().x;
        assert_eq!(cells.get(&(col, 0)).map(String::as_str), Some("┬"));
        assert_eq!(cells.get(&(0, 0)).map(String::as_str), Some("─"));
        assert_eq!(cells.get(&(79, 0)).map(String::as_str), Some("─"));

        // A top-docked bar also puts content at row 1; with `rail: None` the
        // bar's row must stay untouched.
        let content = Rect {
            x: 0,
            y: 1,
            w: 40,
            h: 1,
        };
        let s = render_full(
            &lone_layout(content),
            content,
            None,
            Some(&t(1)),
            Some(label("shell")),
        );
        assert!(painted_cells(&s).iter().all(|(_, y, _)| *y != 0), "{s:?}");
    }

    /// A title is inset into the rule above its pane, accented when focused;
    /// an empty title draws nothing (no invented names).
    #[test]
    fn titles_inset_into_the_rail() {
        let content = railed(80, 24);
        let layout = split_layout(content);
        let s = render(&layout, content, Some("editor"));
        let cells: HashMap<(u16, u16), String> = painted_cells(&s)
            .into_iter()
            .map(|(x, y, sym)| ((x, y), sym))
            .collect();
        let expect = [(0, "─"), (1, " "), (2, "e"), (8, " ")];
        for (x, sym) in expect {
            assert_eq!(cells.get(&(x, 0)).map(String::as_str), Some(sym), "col {x}");
        }
        assert!(
            s.contains(&sgr_fg(theme().pane_title_focus))
                && s.contains(&sgr_fg(theme().pane_title))
        );
        assert_eq!(
            render(&layout, content, Some("")),
            render(&layout, content, None)
        );
    }

    /// Long and double-width titles clip by display cells to their pane.
    #[test]
    fn long_titles_clip_by_display_width() {
        let content = railed(24, 6);
        let layout = lone_layout(content);
        for title in [
            "a-very-long-pane-title-indeed",
            "編集器編集器編集器編集器編集器",
        ] {
            let s = render(&layout, content, Some(title));
            assert!(rail_extent(&s, content) < 24, "{title}");
        }
        assert!(render(&layout, content, Some("a-very-long-pane-title-indeed")).contains(ELLIPSIS));
    }

    /// Badges: an asking pane badges in the attention tone; a satellite pane
    /// badges its host and greys (dropping the focus colour) when down; the
    /// agent glyph matches the sidebar vocabulary.
    #[test]
    fn pane_badges() {
        let content = railed(80, 24);
        let layout = split_layout(content);
        let th = theme();
        let s = render_label(
            &layout,
            content,
            Some(PaneLabel {
                attention: true,
                ..label("claude")
            }),
        );
        assert!(
            s.contains('●') && s.contains(&sgr_fg(th.attention)),
            "{s:?}"
        );

        let sat = |unreachable| {
            render_label(
                &layout,
                content,
                Some(PaneLabel {
                    host: Some("devbox"),
                    unreachable,
                    ..label("shell")
                }),
            )
        };
        let up = sat(false);
        assert!(up.contains("devbox") && up.contains("shell"));
        assert!(up.contains(&sgr_fg(th.chord)) && up.contains(&sgr_fg(th.divider_focus)));
        let down = sat(true);
        assert!(down.contains("devbox") && !down.contains(&sgr_fg(th.divider_focus)));
        assert!(down.contains(&sgr_fg(th.divider)) && !down.contains(&sgr_fg(th.chord)));

        for state in [
            AgentMetaState::Working,
            AgentMetaState::Blocked,
            AgentMetaState::Done,
            AgentMetaState::Idle,
        ] {
            let badge = PaneLabel {
                agent: Some(state),
                ..label("claude")
            }
            .badge(&th)
            .unwrap();
            assert_eq!(badge, agent_badge(&th, state, false, true));
        }
    }

    /// A same-styled run costs one CUP; a wide glyph's trailing half is left
    /// unpainted and the next cell re-anchors with its own CUP, as does the
    /// cell after a badge. The badge reservation equals its written width.
    #[test]
    fn runs_reanchor_where_widths_could_drift() {
        let content = railed(80, 6);
        let s = render(&lone_layout(content), content, None);
        assert_eq!(extract_cups(&s).len(), 1, "an unbroken rail is one CUP");
        assert!(s.len() < 80 * 4, "{} bytes", s.len());

        let content = railed(40, 10);
        let layout = split_layout(content);
        let s = render(&layout, content, Some("日x"));
        let cells = painted_cells(&s);
        let wide = cells.iter().find(|(_, _, sym)| sym == "日").unwrap();
        let after = cells.iter().find(|(_, _, sym)| sym == "x").unwrap();
        assert_eq!(after.0, wide.0 + 2);
        assert!(
            !cells
                .iter()
                .any(|(x, y, _)| *x == wide.0 + 1 && *y == wide.1)
        );
        assert!(extract_cups(&s).contains(&(after.1 + 1, after.0 + 1)));

        let working = PaneLabel {
            agent: Some(AgentMetaState::Working),
            ..label("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        };
        let s = render_label(&layout, content, Some(working));
        let badge = painted_cells(&s)
            .into_iter()
            .find(|(_, _, sym)| sym == "◐")
            .unwrap();
        assert!(extract_cups(&s).contains(&(badge.1 + 1, badge.0 + 2)));
        assert_chrome_only(&layout, content, &s);
        assert_eq!(text_columns("\u{25d0}") + 1, 2);
        assert_eq!(text_columns("\u{65e5}") + 1, 3);
    }

    /// Zero-width marks ride with their base and cost no budget (breaking at
    /// one once silently truncated titles).
    #[test]
    fn zero_width_marks_share_their_base_cell() {
        let content = railed(60, 10);
        let layout = split_layout(content);
        for (title, tail) in [
            ("cafe\u{301}/src", "/src"),
            ("\u{2705}\u{fe0f} build", "build"),
        ] {
            let s = render(&layout, content, Some(title));
            assert!(
                s.contains(tail) && !s.contains(ELLIPSIS),
                "{title:?}: {s:?}"
            );
        }
        let cells = painted_cells(&render(&layout, content, Some("e\u{301}x")));
        let base = cells
            .iter()
            .find(|(_, _, sym)| sym.starts_with('e'))
            .unwrap();
        assert_eq!(base.2, "e\u{301}");
        assert_eq!(
            cells.iter().find(|(_, _, sym)| sym == "x").unwrap().0,
            base.0 + 1
        );
    }

    /// A title is untrusted OSC-2 input on a VT path: no control byte or
    /// bidi override may reach the wire (a pane could inject escapes or make
    /// its label read as a neighbour's), and an override costs no budget.
    #[test]
    fn titles_cannot_inject_controls_or_bidi_overrides() {
        let content = railed(60, 10);
        let layout = split_layout(content);
        let s = render(&layout, content, Some("a\u{1b}]0;pwned\u{7}b\u{9b}c"));
        assert!(
            s.split('\u{1b}').skip(1).all(|seq| seq.starts_with('[')),
            "{s:?}"
        );
        assert!(s.chars().all(|c| c == '\u{1b}' || !c.is_control()), "{s:?}");
        assert!(s.contains("a]0;pwnedbc"));

        let s = render(
            &layout,
            content,
            Some("run\u{202e}gpj.exe\u{202c} \u{2066}x\u{2069}\u{200f}"),
        );
        for bad in [
            '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}', '\u{2067}',
            '\u{2068}', '\u{2069}', '\u{200e}', '\u{200f}', '\u{061c}',
        ] {
            assert!(!s.contains(bad), "U+{:04X}", bad as u32);
        }
        assert!(s.contains("gpj.exe"));
        assert_eq!(
            render(&layout, content, Some("abcdef")),
            render(&layout, content, Some("abc\u{202e}def"))
        );
    }

    /// A zero-height leaf shares its row with the leaf below; labelling it
    /// would make the surviving title depend on `HashMap` order.
    #[test]
    fn a_zero_height_leaf_is_not_labelled() {
        let content = railed(40, 6);
        let mut layout = split_layout(content);
        let one = layout.rects[&t(1)];
        layout.rects.insert(t(2), Rect { h: 0, ..one });
        let a = render(&layout, content, Some("Pane"));
        assert_eq!(a, render(&layout, content, Some("Pane")));
        let titles = painted_cells(&a)
            .iter()
            .filter(|(_, y, sym)| *y == 0 && sym == "P")
            .count();
        assert_eq!(titles, 1);
    }

    /// Each painted cell as `(symbol, accented)` (accented = focus style).
    fn styled_cells(s: &str) -> HashMap<(u16, u16), (String, bool)> {
        let focus_sgr = sgr_fg(theme().divider_focus);
        let mut out = HashMap::new();
        let (mut x, mut y) = (0u16, 0u16);
        let mut accented = false;
        let mut rest = s;
        let mut put = |text: &str, x: &mut u16, y: u16, accented: bool| {
            for c in text.chars() {
                let w = u16::try_from(crate::render::cell_width(c).unwrap_or(0)).unwrap_or(1);
                if w == 0 {
                    continue;
                }
                out.insert((*x, y), (c.to_string(), accented));
                *x = x.saturating_add(w);
            }
        };
        while let Some(i) = rest.find('\x1b') {
            put(&rest[..i], &mut x, y, accented);
            let tail = &rest[i..];
            if tail.starts_with(&focus_sgr) {
                accented = true;
                rest = &tail[focus_sgr.len()..];
                continue;
            }
            let end = tail[1..]
                .find(|c: char| c.is_ascii_alphabetic())
                .map_or(tail.len(), |j| j + 2);
            let seq = &tail[..end];
            if seq.ends_with('H')
                && let Some((rr, cc)) = seq[2..seq.len() - 1].split_once(';')
                && let (Ok(rn), Ok(cn)) = (rr.parse::<u16>(), cc.parse::<u16>())
            {
                y = rn.saturating_sub(1);
                x = cn.saturating_sub(1);
            } else if seq.ends_with('m') {
                accented = false;
            }
            rest = &tail[end..];
        }
        put(rest, &mut x, y, accented);
        out
    }

    fn sgr_fg(color: Color) -> String {
        let mut out: Vec<u8> = Vec::new();
        crate::render::sgr::write_sgr_color(&mut out, color, true).unwrap();
        String::from_utf8(out).unwrap()
    }

    /// Decode the stream into `(x, y, symbol)` per painted cell, advancing by
    /// display width exactly as a terminal does (a per-char harness would
    /// share the very bug it exists to catch).
    fn painted_cells(s: &str) -> Vec<(u16, u16, String)> {
        let mut out = Vec::new();
        let (mut x, mut y) = (0u16, 0u16);
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                let mut body = String::new();
                if chars.peek() == Some(&'[') {
                    chars.next();
                }
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        if c == 'H'
                            && let Some((r, col)) = body.split_once(';')
                            && let (Ok(rn), Ok(cn)) = (r.parse::<u16>(), col.parse::<u16>())
                        {
                            y = rn.saturating_sub(1);
                            x = cn.saturating_sub(1);
                        }
                        break;
                    }
                    body.push(c);
                }
                continue;
            }
            let w = u16::try_from(crate::render::cell_width(c).unwrap_or(0)).unwrap_or(1);
            if w == 0 {
                if let Some(last) = out.last_mut() {
                    let (_, _, sym): &mut (u16, u16, String) = last;
                    sym.push(c);
                }
                continue;
            }
            out.push((x, y, c.to_string()));
            x = x.saturating_add(w);
        }
        out
    }

    /// Every CUP target `(row_1b, col_1b)` in a VT stream.
    fn extract_cups(s: &str) -> Vec<(u16, u16)> {
        s.split("\x1b[")
            .skip(1)
            .filter_map(|seq| {
                let end = seq.find(|c: char| c.is_ascii_alphabetic())?;
                if !seq[end..].starts_with('H') {
                    return None;
                }
                let (r, c) = seq[..end].split_once(';')?;
                Some((r.parse().ok()?, c.parse().ok()?))
            })
            .collect()
    }
}
