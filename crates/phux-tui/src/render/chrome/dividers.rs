//! Pane dividers, pane titles, and the pane-grid rail — ratatui composer.
//!
//! Replaces `attach::multi_pane::paint_dividers` with a ratatui-based
//! composer that respects the **skip-cell carve-out** invariant from
//! ADR-0020: every cell inside a pane interior `Rect` is marked
//! `Cell::skip` so libghostty's direct VT output owns those cells
//! exclusively. The composer only emits VT bytes for the chrome cells
//! around panes — never for pane interiors.
//!
//! # The visual model
//!
//! Panes SHARE their rules. A split costs exactly one cell of chrome,
//! not two adjacent borders, so a 2x2 grid is one `\u{2502}` column and one
//! `\u{2500}` row crossing at a `\u{253c}`. The grid is closed at the top by a
//! **rail** — one row above the pane area — which is where each pane's
//! title lives.
//!
//! Two rules make it read as one object rather than a pile of glyphs:
//!
//! 1. **One stroke weight.** Every rule uses the LIGHT box-drawing set.
//!    Focus used to be drawn as a HEAVY rule, which forced the
//!    mixed-weight junction pictographs (`\u{2545}` `\u{2546}` `\u{2548}` `\u{2549}` ...) at
//!    every crossing; most terminal fonts either lack them or draw
//!    strokes that miss their light neighbours, so an emphasised grid
//!    looked like a broken one.
//! 2. **Focus is colour.** The rules on the focused pane's own frame
//!    (and its title) take `theme.divider_focus` + `BOLD`; everything
//!    else recedes to `theme.divider`. Bold is carried alongside the
//!    colour so the cue survives a terminal that ignores our fg.
//!
//! Emphasis is resolved HERE, from the `focused` pane the caller passes
//! and its rect, rather than in the rasterizer. The rasterizer only sees
//! the layout tree, whose remembered `focus` lags the client's — which
//! made the old heavy-rule emphasis point at the pane you had just left.
//!
//! # Architecture
//!
//! `compute_layout` (in `attach::multi_pane`) stays pure data: it walks
//! the layout tree and yields per-pane `Rect`s plus a list of
//! pre-resolved `DividerCell`s (one per box-drawing cell, junction shape
//! already resolved). This module consumes that data:
//!
//! 1. Allocate a ratatui `Buffer` covering the full viewport.
//! 2. Mark every cell inside a pane interior `Rect` with
//!    `set_diff_option(CellDiffOption::Skip)`.
//! 3. Write each `DividerCell` glyph + style into the buffer.
//! 4. Draw the rail across the top of the pane area, resolving `\u{252c}`
//!    where a vertical rule meets it.
//! 5. Inset each pane's title into the rule above it.
//! 6. Emit positioned VT bytes for non-skip cells only.
//!
//! Step 6 is hand-rolled (not via `CrosstermBackend`) because:
//! - There is no previous-frame buffer to diff against; we always paint
//!   from scratch (the orchestrator owns frame-level invalidation).
//! - We must not touch any pane interior cell — even a no-op write would
//!   stomp libghostty's SGR / cursor state.
//!
//! # Invariants
//!
//! - **Skip-cell**: VT bytes are emitted only for non-skip cells. A unit
//!   test (`skip_cells_never_emit_vt`) asserts no CUP target lands
//!   inside any pane interior `Rect`. THIS IS LOAD-BEARING — if it
//!   breaks, libghostty's pane output gets stomped.
//! - **SGR reset**: emits `\x1b[0m` before any chrome paint and again
//!   after the last cell, so leftover SGR from a prior pane render or
//!   from a rule's own styling never bleeds into the next paint.
//! - **No cursor positioning at exit**: the focused-pane render runs
//!   after dividers and is responsible for the final cursor placement.

use std::io::{self, Write};

use phux_protocol::ResourceId;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect as RataRect;
use ratatui::style::{Color, Modifier, Style};

use crate::attach::multi_pane::PaneLayout;
use crate::layout::Rect;
use crate::render::chrome::{AgentBadge, agent_badge, attention_badge};
use crate::render::overlay::HardcodedBinding;
use crate::render::theme::Theme;
use crate::render::{ELLIPSIS, cell_width};
use phux_client::agent_meta::AgentMetaState;

/// The divider drag-resize table for handler-adjacency tests
/// section (phux-i0e8.10.3).
///
/// COLOCATED with the divider layer: the drag
/// itself is driven by the dispatcher's ADR-0048 grab, whose targets are
/// the `divider_hits` that `compute_layout` produces alongside the glyph
/// cells this module paints. The `help_table_targets_exist` adjacency
/// test asserts a split layout actually yields those grab targets.
pub static HELP_BINDINGS: &[HardcodedBinding] = &[HardcodedBinding {
    chord: "drag divider",
    action: "resize the panes either side",
}];

/// Cells of chrome a pane title needs before any of its label shows:
/// one lead-in `\u{2500}`, a space, a space, and the closing `\u{2500}` the run
/// continues into. A pane narrower than this simply gets no title.
const TITLE_CHROME_CELLS: u16 = 4;

/// Bytes of UTF-8 one chrome cell can hold.
///
/// A cell holds one grapheme cluster: a base character plus whatever
/// zero-width marks follow it. Four bytes covers any single scalar;
/// sixteen leaves room for a base plus a few combining marks, which is
/// every realistic pane title. A cluster longer than this keeps the
/// marks that fit — a truncated cluster still renders as its base, which
/// is what the reader needs.
const CELL_BYTES: usize = 16;

/// What to write into one pane's top rule.
///
/// Borrowed rather than owned: the caller already holds the pane's
/// cached OSC-2 title, and a per-frame `String` per pane is exactly the
/// allocation this layer exists without.
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

/// One cell's grapheme, stored inline.
///
/// Inline rather than a `String` so composing a frame allocates nothing
/// per cell: the chrome repaints on every OSC-2 title change, and a
/// heap allocation per painted cell would make a title change cost more
/// than the frame it appears in.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Sym {
    buf: [u8; CELL_BYTES],
    len: u8,
}

impl Sym {
    /// The empty symbol: a cell that is positioned but paints nothing.
    ///
    /// Load-bearing, not a placeholder. It is what the composer writes
    /// into the trailing half of a double-width glyph: the emitter skips
    /// it AND breaks its run there, so the next cell re-anchors with its
    /// own `CUP` instead of trusting where the terminal's cursor ended
    /// up after drawing a wide character.
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
        // SAFETY-equivalent without unsafe: every push wrote whole
        // `char`s through `encode_utf8`, so the prefix is valid UTF-8;
        // `from_utf8` cannot fail, and an unexpected failure degrades to
        // an unpainted cell rather than a panic.
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

/// The chrome cells of one frame, in paint order.
///
/// The whole point of collecting cells instead of painting a
/// viewport-sized `Buffer` is cost: the chrome touches a rail row, a
/// handful of divider rows and columns, and some title spans — a few
/// hundred cells. A 250x70 terminal is 17,500. The chrome repaints on
/// every pane-title change, so composing and scanning the viewport each
/// time would make a title change an `O(viewport)` event.
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

    /// Order the cells for emission and drop everything that must not be
    /// painted: duplicates (keeping the last write) and anything that
    /// landed inside a pane interior.
    ///
    /// The interior filter is the ADR-0020 skip-cell carve-out. It is
    /// applied here, once, over the cells actually produced, so no path
    /// through this module can emit into a pane whatever the layout
    /// hands it.
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
    /// Scratch cell list, reused across frames so a steady-state repaint
    /// allocates nothing. The chrome paints from the attach driver's
    /// single current-thread runtime and never re-enters itself, so a
    /// thread-local is the whole synchronisation story.
    static SCRATCH: std::cell::RefCell<ChromeCells> =
        std::cell::RefCell::new(ChromeCells::default());
}

// -----------------------------------------------------------------------------
// Public entry points
// -----------------------------------------------------------------------------

/// Render the divider layer for `layout` to `out`.
///
/// `content` is the pane area `Rect` the layout was tiled into and
/// `rail` is the row reserved above it, as reported by
/// `attach::paint::content_layout`. `rail` is passed rather than
/// inferred from `content.y`, because a top-docked status bar also
/// pushes the content down a row: inferring would paint a rule across
/// the bar's own row on a viewport too short to have reserved one.
/// `focused` is the CLIENT's focused pane — the authority for which
/// frame is emphasised. `label_of` supplies each pane's title; return
/// `None` for a pane that should stay unlabelled.
///
/// # Behavior
///
/// - No-op when there is no chrome to draw at all.
/// - Emits a leading `\x1b[0m` SGR reset, then the chrome cells as
///   style-coalesced runs, then a trailing `\x1b[0m`.
/// - **Does not** emit a final cursor position. The focused pane's
///   render runs after this and owns cursor placement, per the
///   chrome <-> pane handoff documented in ADR-0020.
/// - Cells inside a pane interior are dropped and never emitted —
///   libghostty owns those cells exclusively.
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

/// Build the ratatui `Buffer` for the chrome layer.
///
/// Public-in-crate so the skip-cell invariant test can introspect the
/// composition without re-emitting bytes, and so the rendered-frame
/// compositor (`phux snapshot --rendered`, phux-l5xa) can overlay the
/// chrome cells into its dense cell grid without re-parsing emitted VT.
///
/// This is the COLD path: it materialises a viewport-sized buffer
/// because its consumer wants one. The live paint path
/// ([`render_dividers`]) never builds it.
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
    // Everything this composer does not explicitly paint belongs to
    // somebody else — a pane interior, the sidebar strip, the status
    // row, an overlay — so the whole viewport starts skipped and only
    // the cells below clear the flag.
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
    // The focused pane's rect is the whole emphasis model: a rule is on
    // the focused frame exactly when it lies on that rect's perimeter.
    let frame = focused.and_then(|id| layout.rects.get(id)).copied();

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
        Style::default()
            .fg(theme.divider_focus)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.divider)
    }
}

/// Draw the rail across the top of the pane area.
///
/// A rail cell is a `\u{252c}` where a pane boundary drops out of it (which
/// is exactly where a divider cell sits on the pane area's first row)
/// and a plain `\u{2500}` everywhere else. The rail carries the same focus
/// tint as the rules below it, so the focused pane's frame is
/// continuous around its whole perimeter.
///
/// Two linear passes — lay the full rule, then walk the divider cells
/// once and tee the columns they occupy. Probing every column against
/// the divider list instead would be `O(columns x dividers)` per frame.
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

/// `true` when `(x, y)` lies on the perimeter of the focused pane's
/// rect — the ring of chrome cells one step outside it, corners
/// included.
///
/// This is the whole focus model, and it is deliberately geometric: any
/// rule cell touching the focused pane is part of its frame, whichever
/// split produced it, so a `\u{253c}` where the focused pane's left rule
/// crosses an unrelated horizontal rule is emphasised rather than left
/// recessive in the middle of the frame.
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
        // The rule above the pane: the rail for a top-row pane, the
        // interior horizontal divider otherwise. A pane whose top row is
        // 0 has no rule to write into, and a pane sitting directly under
        // a reserved-but-unpainted row must not invent one.
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
    if text.is_empty() {
        return;
    }
    let badge = label.badge(theme);
    // Budget: the pane's width, less the lead-in rule cell, the two
    // padding spaces, and one closing rule cell.
    let mut budget = usize::from(rect.w - TITLE_CHROME_CELLS);
    // MEASURE the badge rather than assuming it is one column wide.
    // `put_measured` writes it by its real width, so a two-column glyph
    // reserved as one would spend a column the title budget still
    // believed it had. The two must be the same arithmetic.
    let badge_cells = badge.map_or(0, |b| text_columns(b.glyph) + 1);
    if budget <= badge_cells {
        return;
    }
    budget -= badge_cells;

    let title_style = if focused {
        Style::default()
            .fg(theme.pane_title_focus)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.pane_title)
    };
    let pad = Style::default().fg(if focused {
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
    x = put_clipped(cells, x, y, text, budget, title_style);
    put_measured(cells, x, y, " ", pad);
}

/// The columns `text` advances, counting only what may reach the wire.
///
/// One function, so a reservation and the write it reserves for can
/// never disagree about how wide a glyph is.
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

/// Reserve the trailing cells of a glyph `width` cells wide and return
/// the column after it.
///
/// The trailing cells get [`Sym::EMPTY`], which does two jobs. It stops
/// the rail's rule showing through the right half of a wide glyph (a
/// literal space there would be WRITTEN, overprinting that half), and it
/// breaks the emitter's run so the next cell re-anchors with its own
/// `CUP`. Without the break, the emitter would advance its cursor one
/// column per cell while the terminal advanced two, and every later cell
/// in the run would slide right — far enough, on a narrow pane with a
/// CJK title, to push the ellipsis past the divider and wrap it into the
/// pane's first content row, which libghostty owns (ADR-0020).
fn blank_continuations(cells: &mut ChromeCells, x: u16, y: u16, width: usize, style: Style) -> u16 {
    let mut cur = x.saturating_add(1);
    for _ in 1..width {
        cells.put(cur, y, Sym::EMPTY, style);
        cur = cur.saturating_add(1);
    }
    cur
}

/// Write `text` clipped to `budget` DISPLAY CELLS, marking a cut with
/// [`ELLIPSIS`], and return the next column.
///
/// Display cells, not chars: a pane title is an arbitrary OSC-2 string,
/// so a CJK or emoji title measured by `chars().count()` overflows its
/// budget and writes over the rule that was supposed to close it.
///
/// Zero-width characters — combining marks, variation selectors, ZWJ —
/// are appended to the cell they belong to rather than ending the
/// string. Stopping at the first one truncated `"cafe\u{301}/src"` to
/// `"cafe"` and `"\u{2705}\u{fe0f} build"` to the emoji alone, silently: they carry
/// no width, so the budget check that produces the ellipsis never fired
/// either.
///
/// Explicit bidi controls are the exception: [`cell_width`] refuses
/// them, so they never reach a cell. They are zero-width and would
/// otherwise ride an untrusted OSC-2 title straight onto the rail, where
/// they reorder every label after them — a pane could make its own title
/// read as its neighbour's.
///
/// Allocation-free by construction: cells carry their grapheme inline,
/// so a per-frame `String` per pane never exists.
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

/// Emit `cells` (row-major, deduplicated) as positioned VT paints.
///
/// Consecutive cells in a row that share a style are emitted as ONE
/// `CUP` plus one SGR plus their symbols, so a rail spanning 160 columns
/// costs one escape sequence rather than 160. A gap, a style change, or
/// an empty symbol closes the run, and the next cell re-anchors — which
/// is what keeps the emitter's idea of the cursor and the terminal's in
/// agreement across a double-width glyph.
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

    use crate::attach::multi_pane::{DividerCell, compute_layout, compute_layout_in};
    use crate::layout::{LayoutNode, LayoutState, SplitDir, split_at};

    fn t(id: u32) -> ResourceId {
        ResourceId::local(id)
    }

    fn leaf(id: u32) -> LayoutNode {
        LayoutNode::Leaf(t(id))
    }

    /// The whole viewport, with no rail row reserved. Used by the tests
    /// that predate the rail and only care about the interior grid.
    fn full(cols: u16, rows: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: cols,
            h: rows,
        }
    }

    /// A pane area with one rail row above it, the shipped shape.
    fn railed(cols: u16, rows: u16) -> Rect {
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

    /// No labels at all — the pre-title behaviour.
    fn unlabelled(_: &ResourceId) -> Option<PaneLabel<'static>> {
        None
    }

    /// The rail row the fixtures reserve: `railed()` puts the pane area
    /// at row 1, so the rail is row 0; `full()` reserves nothing.
    fn rail_row(content: Rect) -> Option<u16> {
        content.y.checked_sub(1)
    }

    /// Render to a string with a fixed label on every pane.
    fn render(layout: &PaneLayout, content: Rect, label: Option<&'static str>) -> String {
        render_with_focus(layout, content, Some(&t(1)), label)
    }

    /// Render with an explicit focused pane.
    fn render_with_focus(
        layout: &PaneLayout,
        content: Rect,
        focus: Option<&ResourceId>,
        label: Option<&'static str>,
    ) -> String {
        render_full(layout, content, rail_row(content), focus, label)
    }

    /// Render with every knob explicit.
    fn render_full(
        layout: &PaneLayout,
        content: Rect,
        rail: Option<u16>,
        focus: Option<&ResourceId>,
        label: Option<&'static str>,
    ) -> String {
        let mut bytes: Vec<u8> = Vec::new();
        render_dividers(&mut bytes, layout, content, rail, focus, &theme(), |_| {
            label.map(|text| PaneLabel {
                text,
                agent: None,
                attention: false,
                seen: true,
            })
        })
        .unwrap();
        String::from_utf8(bytes).unwrap()
    }

    /// Empty layout with no rail: no bytes emitted.
    #[test]
    fn empty_layout_is_noop() {
        let layout = PaneLayout {
            viewport: (80, 24),
            rects: HashMap::new(),
            dividers: Vec::new(),
            divider_hits: Vec::new(),
        };
        assert!(render(&layout, full(80, 24), None).is_empty());
    }

    /// Zero-axis viewport: no-op.
    #[test]
    fn zero_viewport_is_noop() {
        let layout = PaneLayout {
            viewport: (0, 24),
            rects: HashMap::new(),
            dividers: vec![DividerCell {
                x: 0,
                y: 0,
                ch: '\u{2502}',
            }],
            divider_hits: Vec::new(),
        };
        assert!(render(&layout, full(0, 24), None).is_empty());
    }

    /// Two-pane horizontal split: every emitted byte targets a chrome
    /// cell; no CUP lands inside either pane's interior.
    /// THIS IS THE LOAD-BEARING SKIP-CELL INVARIANT TEST.
    #[test]
    fn skip_cells_never_emit_vt() {
        let content = railed(80, 24);
        let layout = split_layout(content);
        assert!(
            !layout.dividers.is_empty(),
            "test precondition: have dividers"
        );

        let s = render(&layout, content, Some("shell"));
        let pane_rects: Vec<Rect> = layout.rects.values().copied().collect();
        let cups = extract_cups(&s);
        assert!(!cups.is_empty(), "expected at least one CUP");
        for (row_1b, col_1b) in cups {
            // Convert to 0-based outer-viewport coords.
            let y = row_1b.saturating_sub(1);
            let x = col_1b.saturating_sub(1);
            for r in &pane_rects {
                assert!(
                    !rect_contains(*r, x, y),
                    "CUP at ({x}, {y}) landed inside pane interior rect {r:?} — \
                     skip-cell invariant violated"
                );
            }
        }
    }

    /// A CUP only opens a run; the cells that FOLLOW it in the same run
    /// carry no CUP of their own, so this walks the decoded cell stream
    /// rather than the CUP list. Every painted cell must be chrome.
    #[test]
    fn no_painted_cell_lands_inside_a_pane() {
        let content = railed(80, 24);
        let layout = split_layout(content);
        let s = render(&layout, content, Some("shell"));
        for (x, y, sym) in painted_cells(&s) {
            for r in layout.rects.values() {
                assert!(
                    !rect_contains(*r, x, y),
                    "painted {sym:?} at ({x}, {y}) inside pane rect {r:?}"
                );
            }
        }
    }

    /// Three-pane cross split: same skip invariant holds across T-piece
    /// junctions and inner dividers.
    #[test]
    fn skip_cells_invariant_holds_for_cross_split() {
        let content = railed(80, 24);
        let layout = cross_layout(content, t(2));
        let s = render(&layout, content, Some("shell"));
        let pane_rects: Vec<Rect> = layout.rects.values().copied().collect();
        for (row_1b, col_1b) in extract_cups(&s) {
            let y = row_1b.saturating_sub(1);
            let x = col_1b.saturating_sub(1);
            for r in &pane_rects {
                assert!(
                    !rect_contains(*r, x, y),
                    "CUP at ({x}, {y}) landed inside pane interior rect {r:?}"
                );
            }
        }
    }

    /// Emitted bytes start with an SGR reset and end with one.
    #[test]
    fn emits_leading_and_trailing_sgr_reset() {
        let content = railed(80, 24);
        let layout = split_layout(content);
        let s = render(&layout, content, None);
        assert!(s.starts_with("\x1b[0m"), "expected leading SGR reset");
        assert!(s.ends_with("\x1b[0m"), "expected trailing SGR reset");
    }

    /// phux-l96p.8: focus is a COLOUR, not a stroke weight. The grid is
    /// uniformly light and the focused pane's own rules are tinted with
    /// `divider_focus` + bold. (This replaces the old
    /// `heavy_glyph_present_when_focus_adjacent`: heavy glyphs forced
    /// mixed-weight junctions that most terminal fonts cannot draw.)
    #[test]
    fn focus_is_painted_not_thickened() {
        let content = railed(80, 24);
        let layout = split_layout(content);
        let s = render(&layout, content, None);
        assert!(
            !s.contains('\u{2503}') && !s.contains('\u{2501}'),
            "the grid must be uniformly light"
        );
        assert!(s.contains('\u{2502}'), "expected the light │");
        assert!(
            s.contains(&sgr_fg(theme().divider_focus)),
            "expected the focus tint in {s:?}"
        );
        assert!(
            s.contains("\x1b[1m"),
            "expected the focused rule to be bold"
        );
    }

    /// An unfocused rule recedes to `theme.divider` and is not bold.
    #[test]
    fn an_unfocused_rule_uses_the_recessive_tone() {
        let content = railed(80, 24);
        // Focus sits in pane 1, so the divider between 2 and 3 is off
        // the focused frame.
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let tree = split_at(&tree, &t(2), &t(3), SplitDir::Horizontal, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        };
        let layout = compute_layout_in(&state, content, (80, 24));
        let s = render(&layout, content, None);
        assert!(
            s.contains(&sgr_fg(theme().divider)),
            "expected the recessive rule tone in {s:?}"
        );
    }

    /// phux-l96p.8 regression: emphasis follows the CLIENT's focused
    /// pane, not the layout tree's remembered focus. The two diverge
    /// routinely — a focus move updates the client immediately and the
    /// tree only when the layout is next persisted — and while the
    /// rasterizer owned emphasis the accent sat on the pane you had just
    /// left.
    #[test]
    fn emphasis_follows_the_passed_focus_not_the_layout_tree() {
        let content = railed(80, 24);
        // The tree remembers pane 1; the client is on pane 2.
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        };
        let layout = compute_layout_in(&state, content, (80, 24));
        let one = layout.rects.get(&t(1)).copied().expect("pane 1");
        let two = layout.rects.get(&t(2)).copied().expect("pane 2");
        let rail_y = rail_row(content).expect("a rail");

        let focus_two = styled_cells(&render_with_focus(&layout, content, Some(&t(2)), None));
        // Pane 2's own top rail is accented; pane 1's is not.
        assert!(
            focus_two[&(two.x + 2, rail_y)].1,
            "the focused pane's rail must be accented"
        );
        assert!(
            !focus_two[&(one.x + 2, rail_y)].1,
            "an unfocused pane's rail must recede"
        );

        // Flip the client's focus and the emphasis flips with it, even
        // though the layout tree never changed.
        let focus_one = styled_cells(&render_with_focus(&layout, content, Some(&t(1)), None));
        assert!(focus_one[&(one.x + 2, rail_y)].1);
        assert!(!focus_one[&(two.x + 2, rail_y)].1);
    }

    /// The rail closes the grid across the top of the pane area and
    /// turns into a `┬` wherever a vertical rule drops out of it.
    #[test]
    fn the_rail_tees_where_a_vertical_rule_meets_it() {
        let content = railed(80, 24);
        let layout = split_layout(content);
        let cells: HashMap<(u16, u16), String> = painted_cells(&render(&layout, content, None))
            .into_iter()
            .map(|(x, y, sym)| ((x, y), sym))
            .collect();
        let rail_y = rail_row(content).expect("a rail row");
        assert_eq!(rail_y, 0);
        // The divider column, read off the layout itself.
        let col = layout.dividers.first().expect("a divider").x;
        assert_eq!(cells.get(&(col, rail_y)).map(String::as_str), Some("┬"));
        // Its neighbours are plain rule.
        assert_eq!(cells.get(&(0, rail_y)).map(String::as_str), Some("─"));
        assert_eq!(cells.get(&(79, rail_y)).map(String::as_str), Some("─"));
    }

    /// With no row reserved above the pane area there is no rail — the
    /// composer must not invent one over pane content.
    #[test]
    fn no_reserved_row_means_no_rail() {
        let content = full(80, 24);
        assert_eq!(rail_row(content), None);
        let layout = split_layout(content);
        for (_, y, _) in painted_cells(&render(&layout, content, None)) {
            assert!(y > 0 || layout.dividers.iter().any(|d| d.y == 0));
        }
    }

    /// A pane's title is inset one cell into the rule above it, wrapped
    /// in single spaces so the rule reads as broken FOR the label.
    #[test]
    fn a_title_is_inset_into_the_rule_above_its_pane() {
        let content = railed(80, 24);
        let layout = split_layout(content);
        let s = render(&layout, content, Some("editor"));
        assert!(s.contains("editor"), "expected the label in {s:?}");
        let cells: HashMap<(u16, u16), String> = painted_cells(&s)
            .into_iter()
            .map(|(x, y, sym)| ((x, y), sym))
            .collect();
        let rail_y = rail_row(content).unwrap();
        // Pane at x = 0: rule cell, space, then the label.
        assert_eq!(cells.get(&(0, rail_y)).map(String::as_str), Some("─"));
        assert_eq!(cells.get(&(1, rail_y)).map(String::as_str), Some(" "));
        assert_eq!(cells.get(&(2, rail_y)).map(String::as_str), Some("e"));
        assert_eq!(cells.get(&(8, rail_y)).map(String::as_str), Some(" "));
    }

    /// The focused pane's title rides `pane_title_focus`; an unfocused
    /// one recedes. Both are drawn from the same one focus fact the
    /// rules use, so a title can never disagree with its own frame.
    #[test]
    fn the_focused_title_is_accented() {
        let content = railed(80, 24);
        let layout = split_layout(content);
        let s = render(&layout, content, Some("editor"));
        assert!(s.contains(&sgr_fg(theme().pane_title_focus)));
        assert!(s.contains(&sgr_fg(theme().pane_title)));
    }

    /// A title too long for its pane is cut with the shared ellipsis and
    /// never runs past the rule that closes it.
    #[test]
    fn a_long_title_is_clipped_to_the_pane() {
        let content = railed(24, 6);
        let state = LayoutState {
            tree: Some(leaf(1)),
            focus: Some(t(1)),
        };
        let layout = compute_layout_in(&state, content, (24, 6));
        let s = render(&layout, content, Some("a-very-long-pane-title-indeed"));
        assert!(s.contains(ELLIPSIS), "expected an ellipsis in {s:?}");
        let rail_y = rail_row(content).unwrap();
        let width: u16 = painted_cells(&s)
            .into_iter()
            .filter(|(_, y, _)| *y == rail_y)
            .map(|(x, _, _)| x)
            .max()
            .unwrap();
        assert!(width < 24, "the rail overran the viewport: {width}");
    }

    /// Display cells, not chars. A wide (CJK) title must be measured by
    /// the cells it occupies or it overwrites the rule closing it.
    #[test]
    fn a_wide_title_is_clipped_by_display_width() {
        let content = railed(24, 6);
        let state = LayoutState {
            tree: Some(leaf(1)),
            focus: Some(t(1)),
        };
        let layout = compute_layout_in(&state, content, (24, 6));
        let s = render(&layout, content, Some("編集器編集器編集器編集器編集器"));
        let rail_y = rail_row(content).unwrap();
        let max_x: u16 = painted_cells(&s)
            .into_iter()
            .filter(|(_, y, _)| *y == rail_y)
            .map(|(x, _, _)| x)
            .max()
            .unwrap();
        assert!(
            max_x < 24,
            "a double-width title escaped its pane (max_x = {max_x})"
        );
    }

    /// A pane whose program never set a title gets no label — phux does
    /// not invent a name for a pane it did not name.
    #[test]
    fn an_empty_title_draws_nothing() {
        let content = railed(80, 24);
        let layout = split_layout(content);
        let with = render(&layout, content, Some(""));
        let without = render(&layout, content, None);
        assert_eq!(with, without);
    }

    /// A pane that has asked for a human badges on its own title, in the
    /// same tone the sidebar's row uses.
    #[test]
    fn an_asking_pane_badges_on_its_title() {
        let content = railed(80, 24);
        let layout = split_layout(content);
        let mut bytes: Vec<u8> = Vec::new();
        render_dividers(
            &mut bytes,
            &layout,
            content,
            rail_row(content),
            Some(&t(1)),
            &theme(),
            |_| {
                Some(PaneLabel {
                    text: "claude",
                    agent: None,
                    attention: true,
                    seen: true,
                })
            },
        )
        .unwrap();
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains('●'), "expected the attention badge in {s:?}");
        assert!(s.contains(&sgr_fg(theme().attention)));
    }

    /// The badge vocabulary is shared with the sidebar: a `working` pane
    /// draws the same glyph in both places or the two surfaces are
    /// describing different machines.
    #[test]
    fn the_pane_badge_matches_the_sidebar_vocabulary() {
        let th = theme();
        for state in [
            AgentMetaState::Working,
            AgentMetaState::Blocked,
            AgentMetaState::Done,
            AgentMetaState::Idle,
        ] {
            let label = PaneLabel {
                text: "claude",
                agent: Some(state),
                attention: false,
                seen: true,
            };
            let badge = label.badge(&th).expect("an agent pane badges");
            assert_eq!(badge, agent_badge(&th, state, false, true));
        }
    }

    /// A run of same-styled cells costs ONE CUP, not one per cell: the
    /// rail is 80 cells wide and must not cost 80 escape sequences.
    #[test]
    fn a_styled_run_emits_one_cup() {
        let content = railed(80, 6);
        let state = LayoutState {
            tree: Some(leaf(1)),
            focus: Some(t(1)),
        };
        let layout = compute_layout_in(&state, content, (80, 6));
        let s = render(&layout, content, None);
        let cups = extract_cups(&s);
        assert_eq!(
            cups.len(),
            1,
            "an unbroken 80-cell rail should cost one CUP, got {}",
            cups.len()
        );
        assert!(
            s.len() < 80 * 4,
            "per-cell escapes crept back in: {} bytes",
            s.len()
        );
    }

    /// phux-l96p.8 fix pass, the load-bearing one. A double-width glyph
    /// advances the terminal's cursor TWO columns; the emitter used to
    /// advance its own idea of the cursor by one per cell and write a
    /// literal space into the trailing half, so every later cell in the
    /// run slid one column right per wide character. With a CJK title on
    /// a narrow pane the run walked past the pane's right edge and, with
    /// DECAWM on, wrapped into the pane's FIRST CONTENT ROW — cells
    /// libghostty owns (ADR-0020).
    ///
    /// Asserted on the decoded cursor positions, not on the glyphs: the
    /// question is where the terminal puts them.
    #[test]
    fn a_wide_title_never_walks_out_of_its_pane() {
        let content = railed(40, 10);
        let layout = split_layout(content);
        let s = render(&layout, content, Some("日本語のペイン名前"));
        let rail_y = rail_row(content).expect("a rail");
        let right_edge = content.x.saturating_add(content.w);
        for (x, y, sym) in painted_cells(&s) {
            assert!(
                x < right_edge,
                "chrome cell {sym:?} landed at column {x}, past the pane area's \
                 right edge {right_edge} — it would wrap into a pane row"
            );
            assert!(
                y == rail_y || layout.dividers.iter().any(|d| d.y == y),
                "chrome cell {sym:?} landed on row {y}, which is neither the \
                 rail nor a divider row"
            );
        }
        // And nothing at all reaches a pane interior.
        for (x, y, sym) in painted_cells(&s) {
            for r in layout.rects.values() {
                assert!(
                    !rect_contains(*r, x, y),
                    "painted {sym:?} at ({x}, {y}) inside pane rect {r:?}"
                );
            }
        }
    }

    /// The mechanism behind the fix: a wide glyph's trailing cell is
    /// reserved but paints nothing, which breaks the run so the next
    /// cell re-anchors with its own CUP instead of trusting the
    /// emitter's column arithmetic.
    #[test]
    fn a_wide_glyph_breaks_the_run_and_the_next_cell_reanchors() {
        let content = railed(40, 10);
        let layout = split_layout(content);
        let s = render(&layout, content, Some("日x"));
        let cells = painted_cells(&s);
        let wide = cells
            .iter()
            .find(|(_, _, sym)| sym == "日")
            .expect("the wide glyph is painted");
        let after = cells
            .iter()
            .find(|(_, _, sym)| sym == "x")
            .expect("the glyph after it is painted");
        assert_eq!(
            after.0,
            wide.0 + 2,
            "the character after a double-width glyph belongs two columns on"
        );
        // Nothing is painted in the trailing half.
        assert!(
            !cells
                .iter()
                .any(|(x, y, _)| *x == wide.0 + 1 && *y == wide.1),
            "the trailing half of a wide glyph must stay unpainted"
        );
        // That column boundary is a real CUP, not an assumed advance.
        assert!(
            extract_cups(&s).contains(&(after.1 + 1, after.0 + 1)),
            "the cell after a wide glyph must re-anchor with its own CUP"
        );
    }

    /// A badge is followed by its own padding space in a different
    /// style, so an ambiguous-width badge glyph cannot drift the title
    /// after it: the style change re-anchors the run. Asserted so a
    /// future restyle that makes badge and pad share a style has to
    /// think about it.
    #[test]
    fn a_badge_is_followed_by_its_own_cup() {
        let content = railed(60, 10);
        let layout = split_layout(content);
        let mut bytes: Vec<u8> = Vec::new();
        render_dividers(
            &mut bytes,
            &layout,
            content,
            rail_row(content),
            Some(&t(1)),
            &theme(),
            |_| {
                Some(PaneLabel {
                    text: "claude",
                    agent: Some(AgentMetaState::Working),
                    attention: false,
                    seen: true,
                })
            },
        )
        .unwrap();
        let s = String::from_utf8(bytes).unwrap();
        let cells = painted_cells(&s);
        let badge = cells
            .iter()
            .find(|(_, _, sym)| sym == "◐")
            .expect("the badge is painted");
        assert!(
            extract_cups(&s).contains(&(badge.1 + 1, badge.0 + 2)),
            "the cell after the badge must carry its own CUP"
        );
    }

    /// Zero-width characters ride with the character they belong to.
    /// Breaking at the first one truncated `"cafe\u{301}/src"` to `"cafe"` and
    /// `"\u{2705}\u{fe0f} build"` to the emoji alone — silently, because a
    /// zero-width character costs no budget and so never triggered the
    /// ellipsis that marks a cut.
    #[test]
    fn zero_width_marks_do_not_truncate_a_title() {
        let content = railed(60, 10);
        let layout = split_layout(content);
        for (title, tail) in [
            ("cafe\u{301}/src", "/src"),
            ("\u{2705}\u{fe0f} build", "build"),
        ] {
            let s = render(&layout, content, Some(title));
            assert!(
                s.contains(tail),
                "{title:?} lost its tail {tail:?}; got {s:?}"
            );
            assert!(
                !s.contains(ELLIPSIS),
                "{title:?} fits, so nothing should be marked as cut"
            );
        }
    }

    /// A combining mark stays in its base character's cell rather than
    /// claiming a column of its own.
    #[test]
    fn a_combining_mark_shares_its_base_cell() {
        let content = railed(60, 10);
        let layout = split_layout(content);
        let s = render(&layout, content, Some("e\u{301}x"));
        let cells = painted_cells(&s);
        let base = cells
            .iter()
            .find(|(_, _, sym)| sym.starts_with('e'))
            .expect("the base character is painted");
        assert_eq!(base.2, "e\u{301}", "the mark rides with its base");
        let next = cells
            .iter()
            .find(|(_, _, sym)| sym == "x")
            .expect("the next character is painted");
        assert_eq!(next.0, base.0 + 1, "the mark consumed no column");
    }

    /// A pane names ITSELF, via OSC 2, so a title is untrusted input on
    /// a path that writes escape sequences. No control byte from a title
    /// may reach the wire, or a pane could inject VT into phux's chrome.
    #[test]
    fn a_title_can_never_inject_an_escape_sequence() {
        let content = railed(60, 10);
        let layout = split_layout(content);
        let s = render(&layout, content, Some("a\u{1b}]0;pwned\u{7}b\u{9b}c"));
        // Every ESC in the output belongs to the emitter's own SGR/CUP...
        for seq in s.split('\u{1b}').skip(1) {
            assert!(
                seq.starts_with('['),
                "a non-CSI escape reached the wire: {seq:?}"
            );
        }
        // ...and no other control byte survives at all, C1 included, so
        // the title's payload is left as inert printable text.
        assert!(
            s.chars().all(|c| c == '\u{1b}' || !c.is_control()),
            "a control character reached the wire: {s:?}"
        );
        assert!(
            s.contains("a]0;pwnedbc"),
            "the inert payload should remain: {s:?}"
        );
    }

    /// phux-l96p.8 fix pass: the rail row is REPORTED by the layout, not
    /// inferred from `content.y`. A top-docked status bar also puts the
    /// content at row 1, so on a viewport too short to reserve a rail the
    /// inference painted a rule straight across the bar's own row — over
    /// a bar that, being unchanged, never repaints to correct it.
    #[test]
    fn a_top_bar_without_a_rail_is_never_painted_over() {
        // Two rows: the bar takes row 0, and the single remaining row
        // goes to the pane rather than to chrome.
        let content = Rect {
            x: 0,
            y: 1,
            w: 40,
            h: 1,
        };
        let state = LayoutState {
            tree: Some(leaf(1)),
            focus: Some(t(1)),
        };
        let layout = compute_layout_in(&state, content, (40, 2));
        // `rail: None` is what `content_layout` reports here.
        let s = render_full(&layout, content, None, Some(&t(1)), Some("shell"));
        assert!(
            painted_cells(&s).iter().all(|(_, y, _)| *y != 0),
            "the status-bar row must stay untouched; got {s:?}"
        );
    }

    /// A zero-height leaf has no pane to label, and shares its `y` with
    /// the leaf below it — so labelling it would make which title
    /// survives depend on `HashMap` iteration order.
    #[test]
    fn a_zero_height_leaf_is_not_labelled() {
        let content = railed(40, 6);
        let mut layout = split_layout(content);
        // Collapse pane 2 onto pane 1's row.
        let one = layout.rects.get(&t(1)).copied().expect("pane 1");
        layout.rects.insert(
            t(2),
            Rect {
                x: one.x,
                y: one.y,
                w: one.w,
                h: 0,
            },
        );
        let a = render(&layout, content, Some("Pane"));
        let b = render(&layout, content, Some("Pane"));
        assert_eq!(a, b, "a zero-height leaf must not make the frame unstable");
        // Exactly one title on that rule: pane 1's. `P` occurs once per
        // label, so counting it counts labels.
        let rail_y = rail_row(content).expect("a rail");
        let titles = painted_cells(&a)
            .iter()
            .filter(|(_, y, sym)| *y == rail_y && sym == "P")
            .count();
        assert_eq!(titles, 1, "expected exactly one label on the rail");
    }

    /// phux-l96p.8 fix pass II: a pane title is an untrusted OSC-2
    /// string, and explicit bidi overrides are zero-width — they cost no
    /// budget, so no width check notices them, and they reorder
    /// everything drawn after them. A pane could title itself so its own
    /// rail label reads as its neighbour's.
    #[test]
    fn a_title_can_never_carry_a_bidi_override() {
        let content = railed(60, 10);
        let layout = split_layout(content);
        // U+202E RIGHT-TO-LEFT OVERRIDE around a payload, plus an
        // isolate and a standalone mark.
        let s = render(
            &layout,
            content,
            Some("run\u{202e}gpj.exe\u{202c} \u{2066}x\u{2069}\u{200f}"),
        );
        for bad in [
            '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}', '\u{2067}',
            '\u{2068}', '\u{2069}', '\u{200e}', '\u{200f}', '\u{061c}',
        ] {
            assert!(
                !s.contains(bad),
                "bidi control U+{:04X} reached the rail: {s:?}",
                bad as u32
            );
        }
        // The inert payload still shows, so the label is not silently
        // emptied either.
        assert!(s.contains("gpj.exe"), "the payload should survive: {s:?}");
    }

    /// A bidi override costs no columns, so it must not consume title
    /// budget or shift where the closing rule lands.
    #[test]
    fn a_bidi_override_costs_no_title_budget() {
        let content = railed(60, 10);
        let layout = split_layout(content);
        let plain = render(&layout, content, Some("abcdef"));
        let spiked = render(&layout, content, Some("abc\u{202e}def"));
        assert_eq!(
            plain, spiked,
            "a zero-width override must compose byte-identically"
        );
    }

    /// The badge's reserved width and the width it is actually written
    /// at come from one measurement. Reserving a flat two columns while
    /// `put_measured` wrote the glyph at its real width would spend a
    /// column the title budget still believed it had.
    #[test]
    fn the_badge_reservation_matches_what_is_written() {
        let content = railed(40, 10);
        let layout = split_layout(content);
        let mut bytes: Vec<u8> = Vec::new();
        render_dividers(
            &mut bytes,
            &layout,
            content,
            rail_row(content),
            Some(&t(1)),
            &theme(),
            |_| {
                Some(PaneLabel {
                    text: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    agent: Some(AgentMetaState::Working),
                    attention: false,
                    seen: true,
                })
            },
        )
        .unwrap();
        let s = String::from_utf8(bytes).unwrap();
        let right_edge = content.x.saturating_add(content.w);
        for (x, y, sym) in painted_cells(&s) {
            assert!(
                x < right_edge,
                "badge + title cell {sym:?} landed at column {x}, past {right_edge}"
            );
            for r in layout.rects.values() {
                assert!(
                    !rect_contains(*r, x, y),
                    "painted {sym:?} at ({x}, {y}) inside pane rect {r:?}"
                );
            }
        }
        // The badge's own reservation is `text_columns(glyph) + 1`, the
        // same arithmetic `put_measured` writes it with.
        assert_eq!(text_columns("\u{25d0}") + 1, 2);
        assert_eq!(text_columns("\u{65e5}") + 1, 3);
    }

    /// Buffer-level introspection: every cell inside a pane Rect has
    /// `skip = true` after compose; every divider cell has `skip =
    /// false` and the right symbol. This is the structural twin of
    /// `skip_cells_never_emit_vt` — if compose breaks, this catches it
    /// before emit even runs.
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
            unlabelled,
        );
        for r in layout.rects.values() {
            for y in r.y..r.y + r.h {
                for x in r.x..r.x + r.w {
                    let cell = buf.cell((x, y)).expect("in-bounds");
                    assert!(
                        cell.diff_option == CellDiffOption::Skip,
                        "pane interior cell ({x}, {y}) in {r:?} not marked skip"
                    );
                }
            }
        }
        for d in &layout.dividers {
            let cell = buf.cell((d.x, d.y)).expect("in-bounds");
            assert!(
                cell.diff_option != CellDiffOption::Skip,
                "divider cell ({}, {}) marked skip",
                d.x,
                d.y
            );
            assert_eq!(
                cell.symbol().chars().next(),
                Some(d.ch),
                "divider cell at ({}, {}) symbol mismatch",
                d.x,
                d.y
            );
        }
    }

    /// phux-i0e8.10.3: the help table advertises drag-to-resize, so the
    /// drag targets must actually exist — a split layout yields the
    /// `divider_hits` the dispatcher's ADR-0048 grab resolves against
    /// (the drag behavior itself is held by `input_dispatch`'s ADR-0048
    /// tests). If dividers stop producing hit targets, the advertised
    /// gesture would be dead and this fails.
    #[test]
    fn help_table_targets_exist() {
        assert!(
            HELP_BINDINGS.iter().any(|b| b.chord.contains("drag")),
            "the table must advertise the drag gesture"
        );
        let layout = split_layout(railed(80, 24));
        assert!(
            !layout.divider_hits.is_empty(),
            "a split layout must yield divider grab targets"
        );
    }

    // -- helpers ---------------------------------------------------------------

    fn split_layout(content: Rect) -> PaneLayout {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        };
        compute_layout_in(&state, content, (content.w, content.y + content.h))
    }

    fn cross_layout(content: Rect, focus: ResourceId) -> PaneLayout {
        let t1 = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let t2 = split_at(&t1, &t(1), &t(3), SplitDir::Vertical, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(t2),
            focus: Some(focus),
        };
        compute_layout_in(&state, content, (content.w, content.y + content.h))
    }

    /// `compute_layout` is still the whole-viewport convenience wrapper.
    #[test]
    fn compute_layout_covers_the_whole_viewport() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        };
        let layout = compute_layout(&state, (80, 24));
        assert_eq!(layout.viewport, (80, 24));
    }

    /// Half-open rect-contains, matching `multi_pane::rect_contains`.
    fn rect_contains(r: Rect, x: u16, y: u16) -> bool {
        x >= r.x && y >= r.y && x < r.x.saturating_add(r.w) && y < r.y.saturating_add(r.h)
    }

    /// Map each painted cell to `(symbol, accented)`, where `accented`
    /// means the cell carried the focus style (bold + `divider_focus`).
    ///
    /// Width-aware for the same reason [`painted_cells`] is.
    fn styled_cells(s: &str) -> HashMap<(u16, u16), (String, bool)> {
        let focus_sgr = format!("\x1b[1m{}", sgr_fg(theme().divider_focus));
        let mut out = HashMap::new();
        let (mut x, mut y) = (0u16, 0u16);
        let mut accented = false;
        let mut rest = s;
        while let Some(i) = rest.find('\x1b') {
            for c in rest[..i].chars() {
                let w = u16::try_from(crate::render::cell_width(c).unwrap_or(0)).unwrap_or(1);
                if w == 0 {
                    continue;
                }
                out.insert((x, y), (c.to_string(), accented));
                x = x.saturating_add(w);
            }
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
        for c in rest.chars() {
            let w = u16::try_from(crate::render::cell_width(c).unwrap_or(0)).unwrap_or(1);
            if w == 0 {
                continue;
            }
            out.insert((x, y), (c.to_string(), accented));
            x = x.saturating_add(w);
        }
        out
    }

    /// The foreground SGR this layer emits for `color`.
    fn sgr_fg(color: Color) -> String {
        let mut out: Vec<u8> = Vec::new();
        crate::render::sgr::write_sgr_color(&mut out, color, true).unwrap();
        String::from_utf8(out).unwrap()
    }

    /// Decode the emitted stream into `(x, y, symbol)` per painted cell,
    /// tracking the cursor across a run so cells written without their
    /// own CUP are still located.
    ///
    /// The cursor advances by each glyph's DISPLAY WIDTH, exactly as a
    /// terminal does. A harness that advanced one column per character
    /// would share the very bug this decoder exists to catch — it would
    /// place a run's cells where the emitter *thinks* they land rather
    /// than where they actually land.
    fn painted_cells(s: &str) -> Vec<(u16, u16, String)> {
        let mut out = Vec::new();
        let (mut x, mut y) = (0u16, 0u16);
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Consume `[`, then the parameter bytes, then the final.
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
                // A combining mark joins the cell already emitted.
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

    /// Extract every CUP target `(row_1b, col_1b)` from a VT byte stream.
    fn extract_cups(s: &str) -> Vec<(u16, u16)> {
        let mut out = Vec::new();
        let bytes = s.as_bytes();
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i] == 0x1b && bytes[i + 1] == b'[' {
                // Find the terminator letter.
                let start = i + 2;
                let mut j = start;
                while j < bytes.len() && !bytes[j].is_ascii_alphabetic() {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'H' {
                    let body = std::str::from_utf8(&bytes[start..j]).unwrap_or("");
                    if let Some((r, c)) = body.split_once(';')
                        && let (Ok(rn), Ok(cn)) = (r.parse::<u16>(), c.parse::<u16>())
                    {
                        out.push((rn, cn));
                    }
                }
                i = j.saturating_add(1);
            } else {
                i += 1;
            }
        }
        out
    }
}
