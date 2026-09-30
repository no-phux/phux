//! Render the client's local `libghostty_vt::Terminal` to the outer
//! terminal as VT (ADR-0013: one replica `Terminal` per attached pane).
//!
//! The paint is a cell diff over dirty rows: rows `RenderState` reports dirty
//! are compared against the pane's `FrontBuffer` (what this renderer last
//! wrote at those cells) and only changed spans are emitted, each positioned
//! with a `CUP` or bridged by rewriting a short unchanged gap. A row the front
//! buffer does not know is emitted whole. SGR is emitted only on a pen change.
//!
//! Frame-level contracts: a painted frame is one DEC 2026 transaction
//! (`SyncOutput`, which nests, so the frame-level block around several panes
//! is never truncated) and a clean frame emits nothing; the renderer never
//! flushes (the end-of-frame cursor is the one flush authority, ADR-0029).
//! The cell loop allocates nothing (see `CellScratch`). Raw mode and the alt
//! screen belong to [`super::driver`].

use std::io::{self, Write};

use libghostty_vt::{
    Terminal as GhosttyTerminal,
    render::{CellIterator, CursorVisualStyle, Dirty, RowIteration, Snapshot},
    screen::CellWide,
    style::{RgbColor, Style, StyleColor, Underline},
};
use phux_core::screen::{CellColor, CellStyle, CursorState, RenderedFrame};
use phux_protocol::{
    kitty_replay,
    render_pool::{RenderPool, RenderWalk, TerminalGeneration},
    sgr::write_reset_and_sgr,
};

/// Errors the renderer can surface.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// libghostty surfaced an error from a render-state operation.
    #[error("libghostty: {0}")]
    Ghostty(#[from] libghostty_vt::Error),
    /// stdout (or the test buffer) returned an I/O error.
    #[error("io: {0}")]
    Io(#[from] io::Error),
    /// Kitty graphics replay failed while projecting libghostty image state.
    #[error("kitty replay: {0}")]
    KittyReplay(#[from] kitty_replay::KittyReplayError),
    /// A row cell the renderer could not decode. Surfaced rather than skipped,
    /// because a skipped column shifts every later cell left.
    #[error("row cell at column {col} could not be read from the batched row")]
    UnreadableCell {
        /// The pane-local column whose cell could not be decoded.
        col: u16,
    },
}

/// Per-cell snapshot of one row, filled from libghostty's cell iterator.
#[derive(Debug, Default)]
struct RowBuf {
    cells: Vec<OwnedRowCell>,
    styles: Vec<Style>,
}

#[derive(Debug, Clone)]
struct OwnedRowCell {
    text: String,
    style_index: u32,
    fg: Option<RgbColor>,
    bg: Option<RgbColor>,
    wide: CellWide,
}

/// Borrowed view of a [`RowBuf`] for one paint/project walk.
#[derive(Debug, Clone, Copy)]
struct RowCells<'buf> {
    cells: &'buf [OwnedRowCell],
    styles: &'buf [Style],
}

/// One cell in a [`RowCells`] walk.
#[derive(Debug, Clone, Copy)]
struct RowCell<'buf> {
    text: &'buf str,
    style_index: u32,
    fg: Option<RgbColor>,
    bg: Option<RgbColor>,
    wide: CellWide,
}

impl<'buf> RowCells<'buf> {
    const fn len(self) -> usize {
        self.cells.len()
    }

    fn get(self, col: usize) -> Option<RowCell<'buf>> {
        let cell = self.cells.get(col)?;
        Some(RowCell {
            text: cell.text.as_str(),
            style_index: cell.style_index,
            fg: cell.fg,
            bg: cell.bg,
            wide: cell.wide,
        })
    }

    fn style(self, index: u32) -> Result<Style, RenderError> {
        self.styles
            .get(index as usize)
            .copied()
            .ok_or(RenderError::UnreadableCell { col: 0 })
    }
}

fn read_row<'alloc, 'buf>(
    cells: &mut CellIterator<'alloc>,
    row: &RowIteration<'alloc, '_>,
    buf: &'buf mut RowBuf,
) -> Result<RowCells<'buf>, RenderError> {
    buf.cells.clear();
    buf.styles.clear();
    let mut iter = cells.update(row)?;
    while let Some(cell) = iter.next() {
        let style = cell.style()?;
        let style_index = u32::try_from(buf.styles.len()).unwrap_or(u32::MAX);
        buf.styles.push(style);
        let mut text = String::new();
        cell.graphemes_utf8(&mut text)?;
        buf.cells.push(OwnedRowCell {
            text,
            style_index,
            fg: cell.fg_color()?,
            bg: cell.bg_color()?,
            wide: cell.raw_cell()?.wide()?,
        });
    }
    Ok(RowCells {
        cells: &buf.cells,
        styles: &buf.styles,
    })
}

/// The copy-mode selection the renderer reverse-videos while painting. Owned
/// by `render::overlay::selection` (ADR-0045) so the renderer and the copy
/// UX agree on what a selection covers.
pub use crate::render::overlay::selection::SelectionRect;
use crate::render::overlay::selection::{CellMark, CopyMarks};

/// One pane's published replica `Terminal` paired with its generation token.
///
/// The kernel REPLACES a pane's `Terminal` on republish, and the pooled
/// render state discards its cache exactly when this token changes, even at
/// unchanged geometry. Carrying both as one value (built only by
/// `pane_state::published_replica`) makes a mismatch unrepresentable. Paths
/// that only inspect the terminal use `pane_state::published_terminal`.
#[derive(Debug, Clone, Copy)]
pub struct ReplicaWalk<'a, 'alloc, 'cb> {
    pub(super) terminal: &'a GhosttyTerminal<'alloc, 'cb>,
    pub(super) generation: TerminalGeneration,
}

#[cfg(any(test, feature = "testkit"))]
impl<'a, 'alloc, 'cb> ReplicaWalk<'a, 'alloc, 'cb> {
    /// Pair a test-owned terminal with a fixed token ("a generation that never
    /// changes").
    #[must_use]
    pub const fn for_test(terminal: &'a GhosttyTerminal<'alloc, 'cb>) -> Self {
        Self {
            terminal,
            generation: 1,
        }
    }
}

/// Per-pane render scaffolding.
///
/// A [`RenderPool`] (the libghostty render trio, rebuilt on geometry or generation change) plus the front buffer. Walks
/// take a [`ReplicaWalk`] so the generation token always arrives with the
/// terminal. The dirty policy (clear each drawn row, then the snapshot bit)
/// is this renderer's own (ADR-0086).
#[derive(Debug)]
pub struct TerminalRenderer<'alloc> {
    /// Pooled render state + row/cell iterators.
    pool: RenderPool<'alloc>,
    kitty_placements: libghostty_vt::kitty::graphics::PlacementIterator<'alloc>,
    /// Last-seen authoritative cursor position (outer-viewport coords:
    /// pane-local cursor plus [`Self::last_origin`]). Updated at the end of
    /// [`Self::render`]. The host-cursor restore paths read this. `None`
    /// while the cursor is hidden.
    last_cursor: Option<(u16, u16)>,
    /// Pane-local cursor `(row, col)` as of the last render, before
    /// [`Self::last_origin`] is added: the predictive-echo anchor. Feeding
    /// predict the outer cursor dragged a lower pane's echo mid-screen.
    /// `None` while hidden.
    last_cursor_local: Option<(u16, u16)>,
    /// Outer-viewport origin `(x, y)` of the last paint, added to pane-local
    /// predictions by the echo overlay.
    last_origin: (u16, u16),
    /// Copy-mode marks (selection and search hits) for the next render; set
    /// just before a copy-mode repaint and cleared right after.
    marks: CopyMarks,
    /// Per-frame emission buffers (see [`CellScratch`]).
    scratch: CellScratch,
    /// What this pane last emitted, cell by cell (see [`FrontBuffer`]).
    front: FrontBuffer,
}

/// The outer terminal's contents at this pane's cells, as this renderer last
/// wrote them.
///
/// libghostty tracks dirt per ROW, so a full-screen animation dirties every
/// row every frame; diffing dirty rows against this turns row dirt into cell
/// dirt (~25x fewer bytes than whole-row repaints for cmatrix-like output).
///
/// A KNOWN row claims the outer cells hold exactly the recorded clusters and
/// pens. Anything writing those cells behind the renderer's back must
/// invalidate, and an unknown row falls back to the whole-row painter, so an
/// over-eager invalidation costs bandwidth, never correctness.
///
/// The renderer invalidates itself on a forced paint, a moved origin or
/// extent, a generation change, a primary/alternate screen switch, a
/// selection change, and a kitty-graphics replay. Outside writers call
/// [`TerminalRenderer::invalidate_front`] / `invalidate_front_rows`: the
/// predictive-echo overlay, the full-frame and SIGWINCH clears, a stdout
/// resync, an unshipped frame, modal overlays, and the copy-mode strip.
///
/// A forced paint records nothing, and a row becomes known only after its
/// bytes reach the sink, so a failed frame cannot leave a false claim.
#[derive(Debug, Default)]
struct FrontBuffer {
    /// The paint identity the rows were recorded under. A paint under any
    /// other identity cannot trust them.
    key: Option<FrontKey>,
    /// One entry per painted row, pane-local.
    rows: Vec<FrontRow>,
}

/// Everything that decides WHERE a pane's cells land and WHICH grid they come
/// from. The front rows are valid only under the key they were recorded with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FrontKey {
    /// Outer-viewport origin of the paint.
    origin: (u16, u16),
    /// Clipped painted extent `(cols, rows)`.
    extent: (u16, u16),
    /// Replica walk identity.
    generation: TerminalGeneration,
    /// Whether the alternate screen was active.
    alt_screen: bool,
}

impl FrontBuffer {
    /// Forget every recorded row.
    fn invalidate_all(&mut self) {
        for row in &mut self.rows {
            row.known = false;
        }
    }

    /// Forget the recorded rows in `rows` (pane-local, clamped to the buffer).
    fn invalidate_rows(&mut self, rows: std::ops::Range<u16>) {
        let end = usize::from(rows.end).min(self.rows.len());
        let start = usize::from(rows.start).min(end);
        for row in &mut self.rows[start..end] {
            row.known = false;
        }
    }

    /// Ready the buffer for a paint under `key`: a forced paint or a moved key
    /// forgets everything, and the row count follows the painted extent.
    fn prepare(&mut self, key: FrontKey, force_full: bool) {
        if force_full || self.key != Some(key) {
            self.invalidate_all();
            self.key = Some(key);
        }
        self.rows
            .resize_with(usize::from(key.extent.1), FrontRow::default);
    }
}

/// One row as last emitted (or, in [`CellScratch::next`], about to be):
/// clusters back to back in `text`, pens deduplicated per style run.
#[derive(Debug, Default)]
struct FrontRow {
    /// Whether the outer terminal is known to hold this row's cells. `false`
    /// until the row is first painted and after any invalidation.
    known: bool,
    /// One record per painted column.
    cells: Vec<FrontCell>,
    /// Every cell's cluster, UTF-8, back to back.
    text: Vec<u8>,
    /// The emitted pen of each style run in the row: the cell's [`Style`]
    /// with the copy-mode inversion already applied, plus the resolved
    /// colours — exactly the `(style, fg, bg)` [`emit_sgr_if_changed`] is
    /// handed, so two equal entries emit identical SGR.
    pens: Vec<EmittedStyle>,
}

/// One painted column of a [`FrontRow`].
#[derive(Clone, Copy, Debug)]
struct FrontCell {
    /// Byte offset of the cluster in [`FrontRow::text`].
    text_start: u32,
    /// Byte length of the cluster; `0` is a blank (emitted as a space).
    text_len: u32,
    /// Index into [`FrontRow::pens`]. A spacer tail borrows its base's pen: it
    /// emits nothing, so its own style can never reach the terminal.
    pen: u16,
    /// Wide-glyph role. Compared like the cluster: a cell that changes role
    /// changes what the outer terminal shows around it.
    wide: CellWide,
}

impl FrontRow {
    /// Empty the row for re-recording, keeping its capacity.
    fn clear(&mut self) {
        self.known = false;
        self.cells.clear();
        self.text.clear();
        self.pens.clear();
    }

    /// The cluster bytes of `cell`.
    fn text_of(&self, cell: FrontCell) -> &[u8] {
        let start = cell.text_start as usize;
        &self.text[start..start + cell.text_len as usize]
    }

    /// Whether column `col` is a wide glyph's spacer tail.
    fn is_tail(&self, col: usize) -> bool {
        self.cells
            .get(col)
            .is_some_and(|cell| matches!(cell.wide, CellWide::SpacerTail))
    }
}

/// The renderer's reusable per-frame emission buffers. The cell loop is the
/// tightest in the product (12k cells on a full-dirty 200x60 frame), so the
/// steady state must be allocation-free.
#[derive(Debug, Default)]
struct CellScratch {
    /// One painted row's VT bytes, handed to the sink in a single
    /// `write_all` instead of one call per cell.
    row: Vec<u8>,
    /// The current cell's grapheme cluster, encoded in place (point reads for
    /// the predictive-echo reconcile; the paint reads whole rows via
    /// [`Self::rowbuf`]).
    cluster: String,
    /// One row's cells: a compact record per cell, styles deduplicated into
    /// runs, and every cluster's bytes back to back. Grows to the widest row
    /// seen, then never allocates.
    rowbuf: RowBuf,
    /// The row being painted, recorded from [`Self::rowbuf`] before any byte
    /// is emitted; swapped with the front row afterwards so buffers recycle.
    next: FrontRow,
}

impl<'alloc> TerminalRenderer<'alloc> {
    /// Allocate render scaffolding for one pane. Do this once per pane,
    /// not per frame.
    pub fn new() -> Result<Self, RenderError> {
        Ok(Self {
            pool: RenderPool::new()?,
            kitty_placements: libghostty_vt::kitty::graphics::PlacementIterator::new()?,
            last_cursor: None,
            last_cursor_local: None,
            last_origin: (0, 0),
            marks: CopyMarks::default(),
            scratch: CellScratch::default(),
            front: FrontBuffer::default(),
        })
    }

    /// Set (or clear) the copy-mode selection for the next render. A change
    /// forgets the front buffer.
    pub fn set_selection(&mut self, selection: Option<SelectionRect>) {
        self.set_copy_marks(CopyMarks::selection(selection));
    }

    /// Set the copy-mode selection and search hits for the next render. A
    /// change forgets the front buffer.
    pub fn set_copy_marks(&mut self, marks: CopyMarks) {
        if self.marks != marks {
            self.front.invalidate_all();
        }
        self.marks = marks;
    }

    /// Forget what this pane last emitted, so each row's next paint rewrites
    /// it whole. Call after writing over the pane's cells outside the renderer
    /// without a following forced repaint.
    pub fn invalidate_front(&mut self) {
        self.front.invalidate_all();
    }

    /// Forget the pane-local `rows` only (the predictive-echo overlay knows
    /// which rows it painted).
    pub fn invalidate_front_rows(&mut self, rows: std::ops::Range<u16>) {
        self.front.invalidate_rows(rows);
    }

    /// Cursor (row, col) as of the most recent [`Self::render_at`] call.
    /// Returns `None` if the cursor was hidden or no render has yet
    /// occurred. The predictive-echo layer reads this to re-anchor its
    /// cursor estimate after a server frame.
    #[must_use]
    pub const fn last_cursor(&self) -> Option<(u16, u16)> {
        self.last_cursor
    }

    /// Pane-local cursor `(row, col)` as of the last render (the predict
    /// anchor); `None` if hidden or never rendered.
    #[must_use]
    pub const fn last_cursor_local(&self) -> Option<(u16, u16)> {
        self.last_cursor_local
    }

    /// Outer-viewport origin `(x, y)` of the most recent `render_at` paint.
    /// The predictive-echo overlay adds this to each pane-local prediction
    /// to position it over the focused pane's cells.
    #[must_use]
    pub const fn last_origin(&self) -> (u16, u16) {
        self.last_origin
    }

    /// The base grapheme at `(row, col)`, or `None` for a blank, wide-tail, or
    /// out-of-range cell (a space yields `Some(' ')`). Takes a fresh snapshot;
    /// used by the predict-layer reconcile.
    pub fn read_grapheme_at(
        &mut self,
        walk: ReplicaWalk<'_, 'alloc, '_>,
        row: u16,
        col: u16,
    ) -> Result<Option<char>, RenderError> {
        if !self.read_cell_cluster(walk, row, col)? {
            return Ok(None);
        }
        Ok(self.scratch.cluster.chars().next())
    }

    /// The whole grapheme cluster at `(row, col)` (multi-codepoint clusters
    /// kept), or `None` for a blank, wide-tail, or out-of-range cell.
    pub fn read_grapheme_string_at(
        &mut self,
        walk: ReplicaWalk<'_, 'alloc, '_>,
        row: u16,
        col: u16,
    ) -> Result<Option<String>, RenderError> {
        if !self.read_cell_cluster(walk, row, col)? {
            return Ok(None);
        }
        if self.scratch.cluster.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.scratch.cluster.clone()))
    }

    /// Load the cell's cluster into [`CellScratch::cluster`] (allocation-free)
    /// and return whether `(row, col)` was in range. Row seeking is linear:
    /// libghostty's `RowIterator` has no `select`.
    fn read_cell_cluster(
        &mut self,
        walk: ReplicaWalk<'_, 'alloc, '_>,
        row: u16,
        col: u16,
    ) -> Result<bool, RenderError> {
        let RenderWalk {
            snapshot,
            rows,
            cells,
        } = self.pool.begin(walk.terminal, walk.generation)?;
        let rows_total = snapshot.rows()?;
        let cols_total = snapshot.cols()?;
        self.scratch.cluster.clear();
        if row >= rows_total || col >= cols_total {
            return Ok(false);
        }
        let mut row_iter = rows.update(&snapshot)?;
        let mut row_index: u16 = 0;
        while let Some(this_row) = row_iter.next() {
            if row_index == row {
                let mut cell_iter = cells.update(this_row)?;
                cell_iter.select(col)?;
                cell_iter.graphemes_utf8(&mut self.scratch.cluster)?;
                return Ok(true);
            }
            row_index = row_index.saturating_add(1);
            if row_index >= rows_total {
                break;
            }
        }
        Ok(false)
    }

    /// Render dirty rows of `terminal` at origin `(0, 0)`, unclipped.
    #[cfg(test)]
    pub fn render(
        &mut self,
        walk: ReplicaWalk<'_, 'alloc, '_>,
        out: &mut impl Write,
    ) -> Result<Dirty, RenderError> {
        // No pane rect to clip against; the terminal's own grid defines the
        // extent (`u16::MAX` clamps to the grid size on both axes).
        self.render_at(walk, out, (0, 0), (u16::MAX, u16::MAX))
    }

    /// Render `terminal` with its top-left at `origin = (x, y)`, clipped to
    /// `clip = (cols, rows)`.
    ///
    /// The painted extent is `min(grid, clip)`: the server-authoritative
    /// mirror may exceed the layout rect during a resize handshake and must
    /// not spill into a divider or neighbour. The cached cursor
    /// ([`Self::last_cursor`]) is outer-absolute.
    pub fn render_at(
        &mut self,
        walk: ReplicaWalk<'_, 'alloc, '_>,
        out: &mut impl Write,
        origin: (u16, u16),
        clip: (u16, u16),
    ) -> Result<Dirty, RenderError> {
        self.render_at_inner(walk, out, origin, clip, false)
    }

    /// Like [`Self::render_at`] but repaints every row. The full-frame path
    /// needs it: its `ED2` wipes the screen but leaves an unchanged pane's
    /// dirty bits clean, which would leave that pane blank.
    pub fn render_at_full(
        &mut self,
        walk: ReplicaWalk<'_, 'alloc, '_>,
        out: &mut impl Write,
        origin: (u16, u16),
        clip: (u16, u16),
    ) -> Result<Dirty, RenderError> {
        self.render_at_inner(walk, out, origin, clip, true)
    }

    /// Project this pane into a region of a dense [`RenderedFrame`] instead
    /// of emitting VT, walking the same snapshot and clipping the same way.
    ///
    /// A wide glyph's `SpacerTail` column is the empty grapheme so widths
    /// reconstruct exactly. Selection inversion is not applied. Returns the
    /// frame-absolute cursor, or `None` when off-viewport or clipped.
    pub fn render_at_cells(
        &mut self,
        walk: ReplicaWalk<'_, 'alloc, '_>,
        frame: &mut RenderedFrame,
        origin: (u16, u16),
        clip: (u16, u16),
    ) -> Result<Option<CursorState>, RenderError> {
        let RenderWalk {
            snapshot,
            rows,
            cells,
        } = self.pool.begin(walk.terminal, walk.generation)?;
        // Clip to the render rect, mirroring `render_at_inner`: a
        // server-authoritative mirror may transiently exceed the client's
        // layout rect during a resize handshake; confine the walk so a wider
        // mirror never spills past the rect and a smaller one stays in-grid.
        let extent = clipped_extent(&snapshot, clip)?;
        let (cols_total, rows_total) = extent;

        let rowbuf = &mut self.scratch.rowbuf;
        let mut row_iter = rows.update(&snapshot)?;
        let mut row_index: u16 = 0;
        while let Some(row) = row_iter.next() {
            if row_index >= rows_total {
                break;
            }
            project_row_into_frame(frame, rowbuf, cells, row, row_index, origin, cols_total)?;
            row_index = row_index.saturating_add(1);
        }

        clipped_frame_cursor(&snapshot, origin, extent)
    }

    /// Render into the rect at `rect_origin` spanning `rect_clip`,
    /// **letterboxed**: a mirror smaller than the rect on an axis is centred
    /// with blanked margin bars (floor split, extra cell bottom/right); a
    /// larger or equal one clamps exactly like [`Self::render_at_full`], byte
    /// for byte. ADR-0027's single-view letterbox.
    pub fn render_at_letterboxed(
        &mut self,
        walk: ReplicaWalk<'_, 'alloc, '_>,
        out: &mut impl Write,
        rect_origin: (u16, u16),
        rect_clip: (u16, u16),
        mirror: (u16, u16),
        force_full: bool,
    ) -> Result<Dirty, RenderError> {
        let lb = letterbox_rect(rect_origin, rect_clip, mirror);
        // Bars and content are one transaction when there are bars, so the
        // blanked margins never show a beat before the content.
        let sync = lb.has_pad().then(|| SyncOutput::begin(out)).transpose()?;
        // Blank the four margin bars first so an undersized mirror's
        // surrounding cells are cleared before the centred content paints
        // over the interior. Skipped entirely when there is no pad (the
        // mirror-fills-the-rect / clamp case), keeping that path byte-identical.
        emit_letterbox_margins(out, lb)?;
        let dirty = self.render_at_inner(walk, out, lb.inner_origin, lb.inner_clip, force_full)?;
        if let Some(sync) = sync {
            sync.end(out)?;
        }
        Ok(dirty)
    }

    fn render_at_inner(
        &mut self,
        walk: ReplicaWalk<'_, 'alloc, '_>,
        out: &mut impl Write,
        origin: (u16, u16),
        clip: (u16, u16),
        force_full: bool,
    ) -> Result<Dirty, RenderError> {
        // Record where this pane is anchored before any early-return: the
        // predictive-echo overlay reads `last_origin` to place pane-local
        // echoes, and the pane stays at this origin even on a clean (no-op)
        // render.
        self.last_origin = origin;
        let RenderWalk {
            snapshot,
            rows,
            cells,
        } = self.pool.begin(walk.terminal, walk.generation)?;
        let dirty = frame_dirty(&snapshot, force_full)?;

        if matches!(dirty, Dirty::Clean) {
            let emitted_kitty = replay_kitty(
                walk.terminal,
                &mut self.kitty_placements,
                &mut self.front,
                out,
                (origin, clip),
            )?;
            render_clean_frame_cursor(
                &snapshot,
                out,
                origin,
                emitted_kitty,
                &mut self.last_cursor,
                &mut self.last_cursor_local,
            )?;
            return Ok(dirty);
        }
        // Every incremental paint is a transaction (the guard nests inside a
        // frame-level block).
        let sync = SyncOutput::begin(out)?;
        out.write_all(b"\x1b[?25l")?;

        let extent = clipped_extent(&snapshot, clip)?;
        // Anything that moves where this pane's cells land, or which grid
        // they come from, voids what the front buffer says is on screen.
        self.front.prepare(
            FrontKey {
                origin,
                extent,
                generation: walk.generation,
                alt_screen: super::input_dispatch::terminal_in_alt_screen(walk.terminal),
            },
            force_full,
        );
        let mut row_iter = rows.update(&snapshot)?;
        paint_dirty_rows(
            out,
            &mut self.scratch,
            &mut self.front,
            &mut row_iter,
            cells,
            dirty,
            origin,
            extent,
            &self.marks,
            !force_full,
        )?;

        let _ = replay_kitty(
            walk.terminal,
            &mut self.kitty_placements,
            &mut self.front,
            out,
            (origin, clip),
        )?;

        emit_frame_epilogue(
            &snapshot,
            out,
            origin,
            &mut self.last_cursor,
            &mut self.last_cursor_local,
        )?;
        sync.end(out)?;
        Ok(dirty)
    }
}

/// Replay the pane's kitty graphics over its cells, returning whether any
/// placement was emitted (which forgets the front buffer).
fn replay_kitty<'alloc>(
    terminal: &GhosttyTerminal<'alloc, '_>,
    placements: &mut libghostty_vt::kitty::graphics::PlacementIterator<'alloc>,
    front: &mut FrontBuffer,
    out: &mut impl Write,
    at: ((u16, u16), (u16, u16)),
) -> Result<bool, RenderError> {
    let (origin, clip) = at;
    let emitted =
        kitty_replay::emit_kitty_graphics_replay(terminal, placements, out, origin, clip)?;
    if emitted {
        front.invalidate_all();
    }
    Ok(emitted)
}

/// The frame-level dirty verdict this paint acts on.
///
/// `force_full` — the full-frame path's forced redraw after its `ED2` — wins
/// over the snapshot's own incremental tracking.
fn frame_dirty(snapshot: &Snapshot<'_, '_>, force_full: bool) -> Result<Dirty, RenderError> {
    if force_full {
        return Ok(Dirty::Full);
    }
    Ok(snapshot.dirty()?)
}

/// The painted extent `(cols, rows)`: the grid clipped to the render rect.
fn clipped_extent(
    snapshot: &Snapshot<'_, '_>,
    clip: (u16, u16),
) -> Result<(u16, u16), RenderError> {
    let (clip_cols, clip_rows) = clip;
    let rows_total = snapshot.rows()?.min(clip_rows);
    let cols_total = snapshot.cols()?.min(clip_cols);
    Ok((cols_total, rows_total))
}

/// Walk rows, painting each that needs redrawing (every row under
/// `Dirty::Full`, dirty ones under `Partial`). What a visited row emits is
/// decided against the front buffer in [`paint_row`]; `record` is `false`
/// for a forced paint, which leaves rows unknown.
#[allow(
    clippy::too_many_arguments,
    reason = "one row-walk context: sink, scratch, front buffer, the libghostty trio, and the clip/selection/record policy"
)]
fn paint_dirty_rows<'alloc>(
    out: &mut impl Write,
    scratch: &mut CellScratch,
    front: &mut FrontBuffer,
    row_iter: &mut RowIteration<'alloc, '_>,
    cells: &mut CellIterator<'alloc>,
    dirty: Dirty,
    origin: (u16, u16),
    extent: (u16, u16),
    marks: &CopyMarks,
    record: bool,
) -> Result<(), RenderError> {
    let (cols_total, rows_total) = extent;
    // The outer pen is unknown at the start of every pane paint: chrome,
    // another pane, or an overlay may have written anything since this pane
    // last emitted. From here on nothing else writes until the paint ends, so
    // the pen each span leaves carries to the next, across jumps and rows.
    let mut pass = PaintPass {
        marks,
        record,
        pen: SpanPen::UNKNOWN,
    };
    let mut row_index: u16 = 0;
    while let Some(row) = row_iter.next() {
        if row_index >= rows_total {
            break;
        }
        if matches!(dirty, Dirty::Full) || row.dirty()? {
            let Some(front_row) = front.rows.get_mut(usize::from(row_index)) else {
                break;
            };
            let at = RowAt {
                row_index,
                origin,
                cols_total,
            };
            paint_row(out, scratch, front_row, row, cells, at, &mut pass)?;
        }
        row_index += 1;
    }
    Ok(())
}

/// What one pane paint threads across its rows: the copy-mode marks,
/// whether rows are recorded into the front buffer, and the outer terminal's
/// pen as the paint has left it so far.
#[derive(Debug)]
struct PaintPass<'m> {
    marks: &'m CopyMarks,
    record: bool,
    pen: SpanPen,
}

/// Where one painted row lands: its pane-local index, the pane's
/// outer-viewport origin, and the clipped column count.
#[derive(Clone, Copy, Debug)]
struct RowAt {
    row_index: u16,
    origin: (u16, u16),
    cols_total: u16,
}

impl RowAt {
    /// The row's outer-viewport row.
    const fn outer_row(self) -> u16 {
        self.row_index.saturating_add(self.origin.1)
    }

    /// The outer-viewport column of pane-local column `col`.
    fn outer_col(self, col: usize) -> u16 {
        u16::try_from(col)
            .unwrap_or(u16::MAX)
            .saturating_add(self.origin.0)
    }
}

/// Paint one row (composed into [`CellScratch::row`], one `write_all`), then
/// clear its dirty bit. A known, unchanged row emits nothing.
fn paint_row<'alloc>(
    out: &mut impl Write,
    scratch: &mut CellScratch,
    front_row: &mut FrontRow,
    row: &RowIteration<'alloc, '_>,
    cells: &mut CellIterator<'alloc>,
    at: RowAt,
    pass: &mut PaintPass<'_>,
) -> Result<(), RenderError> {
    let CellScratch {
        row: buf,
        rowbuf,
        next,
        ..
    } = scratch;
    buf.clear();

    // Every cell, a spacer tail included, consumes one column, so walking
    // columns in step with cells clips at `cols_total`.
    let batch = read_row(cells, row, rowbuf)?;
    let recorded = emit_and_record(buf, front_row, next, &batch, at, pass)?;

    if !buf.is_empty() {
        out.write_all(buf)?;
    }
    // The recording is a claim about the terminal, so it becomes known only
    // once its bytes have been handed to the sink: a failed write returns
    // above with the row still unknown (and its dirty bit still set).
    front_row.known = recorded;
    // Reset per-row dirty bit after drawing, per the libghostty
    // contract.
    row.set_dirty(false)?;
    Ok(())
}

/// Emit one row into `buf` and make its recording the front row: a KNOWN row
/// emits only differing cells ([`emit_row_diff`]); otherwise the whole row is
/// emitted while recorded. The row stays unknown until [`paint_row`] sees its
/// bytes reach the sink.
fn emit_and_record(
    buf: &mut Vec<u8>,
    front_row: &mut FrontRow,
    next: &mut FrontRow,
    batch: &RowCells<'_>,
    at: RowAt,
    pass: &mut PaintPass<'_>,
) -> Result<bool, RenderError> {
    // Run indices belong to one row read, so the run memory starts empty on
    // every row; the outer pen itself carries over.
    pass.pen.last_pen = None;
    if !pass.record {
        // A forced paint rewrites every row onto a cleared screen, the path
        // that can least use a recording: emit straight from the batch and
        // leave the row unknown, to be recorded by its next incremental paint.
        begin_full_row(buf, at, &mut pass.pen)?;
        front_row.known = false;
        emit_unrecorded_row(buf, batch, at, pass)?;
        return Ok(false);
    }
    let painted = batch.len().min(usize::from(at.cols_total));
    if front_row.known && front_row.cells.len() == painted {
        record_row(next, batch, at, pass.marks, None)?;
        emit_row_diff(buf, front_row, next, at, &mut pass.pen)?;
    } else {
        begin_full_row(buf, at, &mut pass.pen)?;
        record_row(next, batch, at, pass.marks, Some((buf, &mut pass.pen)))?;
    }
    std::mem::swap(front_row, next);
    front_row.known = false;
    Ok(true)
}

/// Emit a whole row straight from its batched read, recording nothing (the
/// forced-paint path). A cell sharing its predecessor's [`PenKey`] skips to
/// its glyphs; a spacer tail writes nothing.
///
/// Must stay byte-identical to [`record_row`]'s emitting mode;
/// `whole_row_paints_agree_and_reconstruct_the_grid` holds the two together.
fn emit_unrecorded_row(
    buf: &mut Vec<u8>,
    batch: &RowCells<'_>,
    at: RowAt,
    pass: &mut PaintPass<'_>,
) -> Result<(), RenderError> {
    let mut prev: Option<PenKey> = None;
    walk_row_cells(batch, at.cols_total, |col, cell| {
        if matches!(cell.wide, CellWide::SpacerTail) {
            return Ok(());
        }
        let mark = cell_mark(pass.marks, at.row_index, col, cell.wide);
        let key = PenKey {
            style_index: cell.style_index,
            fg: cell.fg,
            bg: cell.bg,
            mark,
        };
        if prev != Some(key) {
            let mut style = batch.style(cell.style_index)?;
            apply_mark(&mut style, mark);
            emit_sgr_if_changed(buf, &mut pass.pen.emitted, style, cell.fg, cell.bg);
            prev = Some(key);
        }
        emit_cell_glyphs(buf, cell.text.as_bytes());
        Ok(())
    })
}

/// Record one row's cells into `next`, clipped to `at.cols_total`.
///
/// Each pen is resolved exactly as it will be emitted (selection inversion
/// applied). Cells sharing a [`PenKey`] with their predecessor share its pen
/// entry, so the 72-byte [`Style`] is materialised once per run. With `emit`
/// the row is also written as it is recorded (the whole-row paint in one
/// pass); the caller has already written the prologue ([`begin_full_row`]).
fn record_row(
    next: &mut FrontRow,
    batch: &RowCells<'_>,
    at: RowAt,
    marks: &CopyMarks,
    mut emit: Option<(&mut Vec<u8>, &mut SpanPen)>,
) -> Result<(), RenderError> {
    next.clear();
    let mut prev_pen: Option<PenKey> = None;
    walk_row_cells(batch, at.cols_total, |col, cell| {
        let run = record_pen(next, batch, cell, (at.row_index, col), marks, &mut prev_pen)?;
        let tail = matches!(cell.wide, CellWide::SpacerTail);
        if !tail && let Some((buf, pen)) = &mut emit {
            if let Some((style, fg, bg)) = run {
                emit_sgr_if_changed(buf, &mut pen.emitted, style, fg, bg);
            }
            emit_cell_glyphs(buf, cell.text.as_bytes());
        }
        record_cell(next, cell);
        Ok(())
    })
}

/// Settle `cell`'s pen entry in `next`, returning the pen when the cell opens
/// a new style run. A spacer tail borrows the base's pen (it emits nothing).
fn record_pen(
    next: &mut FrontRow,
    batch: &RowCells<'_>,
    cell: &RowCell<'_>,
    at: (u16, u16),
    marks: &CopyMarks,
    prev: &mut Option<PenKey>,
) -> Result<Option<EmittedStyle>, RenderError> {
    let tail = matches!(cell.wide, CellWide::SpacerTail);
    if tail && !next.pens.is_empty() {
        return Ok(None);
    }
    let mark = if tail {
        CellMark::None
    } else {
        cell_mark(marks, at.0, at.1, cell.wide)
    };
    let key = PenKey {
        style_index: cell.style_index,
        fg: cell.fg,
        bg: cell.bg,
        mark,
    };
    let opens_run = *prev != Some(key) || next.pens.is_empty();
    if !tail {
        *prev = Some(key);
    }
    if !opens_run {
        return Ok(None);
    }
    let mut style = batch.style(cell.style_index)?;
    apply_mark(&mut style, mark);
    let pen = (style, cell.fg, cell.bg);
    next.pens.push(pen);
    Ok(Some(pen))
}

/// Append `cell`'s cluster and record to `next`, under the pen entry
/// [`record_pen`] just settled.
fn record_cell(next: &mut FrontRow, cell: &RowCell<'_>) {
    let text_start = u32::try_from(next.text.len()).unwrap_or(u32::MAX);
    next.text.extend_from_slice(cell.text.as_bytes());
    next.cells.push(FrontCell {
        text_start,
        text_len: u32::try_from(cell.text.len()).unwrap_or(u32::MAX),
        pen: u16::try_from(next.pens.len().saturating_sub(1)).unwrap_or(u16::MAX),
        wide: cell.wide,
    });
}

/// Open a whole-row paint: `CUP` to the row start and reset the pen;
/// [`record_row`] then writes every cell. This is the path an unknown front
/// row takes.
fn begin_full_row(buf: &mut Vec<u8>, at: RowAt, pen: &mut SpanPen) -> io::Result<()> {
    write_cup(buf, at.outer_row(), at.origin.0)?;
    // Force a reset at row start so the previous row's tail style can't leak
    // into the current row. After this the active outer-terminal SGR state is
    // the default style.
    buf.extend_from_slice(b"\x1b[0m");
    *pen = SpanPen::DEFAULT;
    Ok(())
}

/// Emit only the cells of `next` that differ from `front`, as positioned
/// spans. A span never half-writes a wide glyph (it backs onto a base and
/// runs through a trailing tail). Between spans the cursor jumps (`CUP`) or
/// the unchanged gap is rewritten, whichever is fewer bytes ([`bridge_gap`]).
/// A `CUP` leaves the pen alone, so `pen` carries across jumps; only a pane
/// paint's first cell emits a complete SGR.
fn emit_row_diff(
    buf: &mut Vec<u8>,
    front: &FrontRow,
    next: &FrontRow,
    at: RowAt,
    pen: &mut SpanPen,
) -> io::Result<()> {
    let len = next.cells.len();
    let mut memo = PenMemo::default();
    // Pane-local column the outer cursor sits at after the last span, or
    // `None` before the first span on this row.
    let mut cursor: Option<usize> = None;
    let mut col = 0;
    while let Some(changed) = (col..len).find(|&c| cell_changed(front, next, c, &mut memo)) {
        let start = span_start(front, next, changed).max(col);
        let end = span_end(front, next, changed, &mut memo);
        let bridged = cursor.is_some_and(|from| bridge_gap(buf, next, from, start, at, pen));
        if !bridged {
            write_cup(buf, at.outer_row(), at.outer_col(start))?;
        }
        emit_cells(buf, next, start..end, pen);
        cursor = Some(end);
        col = end;
    }
    Ok(())
}

/// Whether column `c` shows something different in `next` than in `front`:
/// a different cluster, a different wide-glyph role, or a different pen.
fn cell_changed(front: &FrontRow, next: &FrontRow, c: usize, memo: &mut PenMemo) -> bool {
    let (was, now) = (front.cells[c], next.cells[c]);
    let (shown, wanted) = (front.text_of(was), next.text_of(now));
    was.wide != now.wide || shown != wanted || memo.differs(front, was.pen, next, now.pen)
}

/// The first column of the span whose first changed column is `changed`,
/// backed onto a wide glyph's base when `changed` is its tail in either row
/// (a tail can only be redrawn by writing its base).
fn span_start(front: &FrontRow, next: &FrontRow, changed: usize) -> usize {
    let mut start = changed;
    while start > 0 && (front.is_tail(start) || next.is_tail(start)) {
        start -= 1;
    }
    start
}

/// One past the last column of the span whose first changed column is
/// `changed`: it runs while columns keep changing, and on through any spacer
/// tail (old or new) that follows, because writing the column before a tail
/// rewrites — or erases — the glyph that tail belongs to.
fn span_end(front: &FrontRow, next: &FrontRow, changed: usize, memo: &mut PenMemo) -> usize {
    let len = next.cells.len();
    let mut end = changed + 1;
    while end < len
        && (front.is_tail(end) || next.is_tail(end) || cell_changed(front, next, end, memo))
    {
        end += 1;
    }
    end
}

/// Bridge `from..to` (same row) by rewriting the unchanged cells if that
/// costs no more than a `CUP`. Tried and rolled back rather than estimated;
/// on `false` nothing was written and `pen` is untouched.
fn bridge_gap(
    buf: &mut Vec<u8>,
    next: &FrontRow,
    from: usize,
    to: usize,
    at: RowAt,
    pen: &mut SpanPen,
) -> bool {
    let jump = cup_len(at.outer_row(), at.outer_col(to));
    if to < from || to - from > jump {
        return false;
    }
    let mark = buf.len();
    let saved = *pen;
    emit_cells(buf, next, from..to, pen);
    if buf.len() - mark <= jump {
        return true;
    }
    buf.truncate(mark);
    *pen = saved;
    false
}

/// Bytes [`write_cup`] spends to reach 0-based `(row, col)`.
fn cup_len(row: u16, col: u16) -> usize {
    const fn digits(n: u32) -> usize {
        match n {
            0..=9 => 1,
            10..=99 => 2,
            100..=999 => 3,
            1_000..=9_999 => 4,
            _ => 5,
        }
    }
    // ESC [ row ; col H
    4 + digits(u32::from(row) + 1) + digits(u32::from(col) + 1)
}

/// Pen comparisons between two recorded rows, memoised on the last pair
/// (cells come in runs, so this makes the comparison a per-run cost).
#[derive(Debug, Default)]
struct PenMemo {
    last: Option<(u16, u16, bool)>,
}

impl PenMemo {
    fn differs(&mut self, front: &FrontRow, was: u16, next: &FrontRow, now: u16) -> bool {
        if let Some((a, b, differs)) = self.last
            && a == was
            && b == now
        {
            return differs;
        }
        let differs = front.pens[usize::from(was)] != next.pens[usize::from(now)];
        self.last = Some((was, now, differs));
        differs
    }
}

/// The outer terminal's pen as a pane paint's emission has left it.
#[derive(Clone, Copy, Debug)]
struct SpanPen {
    /// Whether the outer pen is known at all. `false` at the start of a pane
    /// paint, until the paint emits its first SGR or row prologue: no paint
    /// trusts state another writer left.
    known: bool,
    /// The active pen when `known`; `None` is the default style. This is the
    /// coalescing state [`emit_sgr_if_changed`] maintains.
    emitted: Option<EmittedStyle>,
    /// The recorded pen index of the last cell that settled the SGR state. A
    /// cell sharing it is another member of the same run and cannot change
    /// the emitted sequence.
    last_pen: Option<u16>,
}

impl SpanPen {
    /// Just after a row-leading `\x1b[0m`: the default style is active.
    const DEFAULT: Self = Self {
        known: true,
        emitted: None,
        last_pen: None,
    };
    /// At the start of a pane paint: nothing is assumed.
    const UNKNOWN: Self = Self {
        known: false,
        emitted: None,
        last_pen: None,
    };
}

/// Emit the recorded cells `cols` of `row`, settling the pen at each run
/// change. A wide glyph's spacer tail writes nothing at all.
fn emit_cells(buf: &mut Vec<u8>, row: &FrontRow, cols: std::ops::Range<usize>, pen: &mut SpanPen) {
    for cell in &row.cells[cols] {
        if matches!(cell.wide, CellWide::SpacerTail) {
            // Account for the tail column, but emit no overwrite. The active
            // outer-terminal state is untouched, so the run identity carries
            // across the tail to the next real cell.
            continue;
        }
        if pen.last_pen != Some(cell.pen) {
            let (style, fg, bg) = row.pens[usize::from(cell.pen)];
            if pen.known {
                emit_sgr_if_changed(buf, &mut pen.emitted, style, fg, bg);
            } else {
                emit_sgr_absolute(buf, &mut pen.emitted, style, fg, bg);
                pen.known = true;
            }
            pen.last_pen = Some(cell.pen);
        }
        emit_cell_glyphs(buf, row.text_of(*cell));
    }
}

/// Settle the outer pen to `(style, fg, bg)` from an UNKNOWN state: a bare
/// reset for the default pen, otherwise the full reset-and-set.
fn emit_sgr_absolute(
    out: &mut Vec<u8>,
    emitted: &mut Option<EmittedStyle>,
    style: Style,
    fg: Option<RgbColor>,
    bg: Option<RgbColor>,
) {
    if is_default_render(&style, fg, bg) {
        out.extend_from_slice(b"\x1b[0m");
        *emitted = None;
        return;
    }
    emit_sgr_set(out, &style, fg, bg);
    *emitted = Some((style, fg, bg));
}

/// The per-column cell source a row walk reads. [`RowCells`] is the only
/// production implementation; the trait lets tests stage an unreadable cell.
trait RowCellSource<'buf> {
    /// How many cells the row holds.
    fn cell_count(&self) -> usize;
    /// The cell at `col`, or `None` if the read cannot produce one.
    fn cell_at(&self, col: usize) -> Option<RowCell<'buf>>;
}

impl<'buf> RowCellSource<'buf> for RowCells<'buf> {
    #[inline]
    fn cell_count(&self) -> usize {
        self.len()
    }
    #[inline]
    fn cell_at(&self, col: usize) -> Option<RowCell<'buf>> {
        self.get(col)
    }
}

/// Hand every column of a row to `visit`, in order, refusing to skip one.
///
/// `(0..cols).zip(source.iter())` would be a trap: `iter` is a `filter_map`,
/// so one unreadable cell would shift the rest of the row left silently. An
/// unreadable column is a [`RenderError::UnreadableCell`] instead. The
/// `let ... else` keeps the error construction off the hot path (`ok_or`
/// cost ~20% of a full-dirty frame).
#[inline]
fn walk_row_cells<'buf, S, F>(source: &S, cols_total: u16, mut visit: F) -> Result<(), RenderError>
where
    S: RowCellSource<'buf>,
    F: FnMut(u16, &RowCell<'buf>) -> Result<(), RenderError>,
{
    let painted = u16::try_from(source.cell_count())
        .unwrap_or(u16::MAX)
        .min(cols_total);
    for col in 0..painted {
        let Some(cell) = source.cell_at(usize::from(col)) else {
            return Err(RenderError::UnreadableCell { col });
        };
        visit(col, &cell)?;
    }
    Ok(())
}

/// Write one cell's glyphs, after [`emit_cells`] has settled the style.
fn emit_cell_glyphs(out: &mut Vec<u8>, cluster: &[u8]) {
    if cluster.is_empty() {
        // A regular blank advances one column with a space.
        out.push(b' ');
        return;
    }
    // Already UTF-8: libghostty encoded the cluster into the row buffer.
    out.extend_from_slice(cluster);
}

/// Close out a painted frame: reset SGR, place and cache the cursor, apply the
/// cursor style, and clear the global dirty bit. It does NOT flush:
/// `paint::end_of_frame_cursor` is the single flush authority (ADR-0029).
fn emit_frame_epilogue(
    snapshot: &Snapshot<'_, '_>,
    out: &mut impl Write,
    origin: (u16, u16),
    last_cursor: &mut Option<(u16, u16)>,
    last_cursor_local: &mut Option<(u16, u16)>,
) -> Result<(), RenderError> {
    // Reset SGR before the final cursor placement so the visual
    // cursor isn't tainted by the last cell's attributes.
    out.write_all(b"\x1b[0m")?;
    cache_and_render_cursor(snapshot, out, origin, last_cursor, last_cursor_local)?;
    // Optional cursor style — best-effort.
    emit_cursor_style(
        out,
        snapshot.cursor_visual_style()?,
        snapshot.cursor_blinking()?,
    )?;

    // Clear the global dirty bit. Per-row bits were cleared inline.
    snapshot.set_dirty(Dirty::Clean)?;
    Ok(())
}

/// Project one row's cells into `frame`, clipped to `cols_total` columns.
///
/// Reads the row in one crossing, exactly as [`paint_row`] does.
fn project_row_into_frame<'alloc>(
    frame: &mut RenderedFrame,
    rowbuf: &mut RowBuf,
    cells: &mut CellIterator<'alloc>,
    row: &RowIteration<'alloc, '_>,
    row_index: u16,
    origin: (u16, u16),
    cols_total: u16,
) -> Result<(), RenderError> {
    let (ox, oy) = origin;
    let batch = read_row(cells, row, rowbuf)?;
    walk_row_cells(&batch, cols_total, |col, cell| {
        project_cell_into_frame(
            frame,
            &batch,
            cell,
            row_index.saturating_add(oy),
            col.saturating_add(ox),
        )
    })
}

/// Write one cell's grapheme + resolved style into `frame` at `(row, col)`,
/// leaving the frame untouched when that coordinate is out of range.
fn project_cell_into_frame(
    frame: &mut RenderedFrame,
    batch: &RowCells<'_>,
    cell: &RowCell<'_>,
    row: u16,
    col: u16,
) -> Result<(), RenderError> {
    let style = to_cell_style(&batch.style(cell.style_index)?, cell.fg, cell.bg);
    if let Some(dst) = frame.cell_mut(row, col) {
        // Overwrite in place so the destination cell's `String` keeps its
        // buffer across frames instead of being dropped and re-allocated.
        dst.grapheme.clear();
        dst.grapheme.push_str(frame_grapheme(cell));
        dst.style = style;
    }
    Ok(())
}

/// The grapheme a cell contributes to a [`RenderedFrame`]: a space for a
/// blank, the empty string for a spacer tail, else the cell's cluster.
const fn frame_grapheme<'buf>(cell: &RowCell<'buf>) -> &'buf str {
    if !cell.text.is_empty() {
        return cell.text;
    }
    if matches!(cell.wide, CellWide::SpacerTail) {
        ""
    } else {
        " "
    }
}

/// The pane's cursor, shifted into frame-absolute coords, dropped when it sits
/// outside the painted (clipped) region.
fn clipped_frame_cursor(
    snapshot: &Snapshot<'_, '_>,
    origin: (u16, u16),
    extent: (u16, u16),
) -> Result<Option<CursorState>, RenderError> {
    let (ox, oy) = origin;
    let (cols_total, rows_total) = extent;
    let cursor = match snapshot.cursor_viewport()? {
        Some(v) if v.y < rows_total && v.x < cols_total => Some(CursorState {
            x: v.x.saturating_add(ox),
            y: v.y.saturating_add(oy),
            visible: snapshot.cursor_visible()?,
        }),
        _ => None,
    };
    Ok(cursor)
}

fn cache_and_render_cursor(
    snapshot: &Snapshot<'_, '_>,
    out: &mut impl Write,
    origin: (u16, u16),
    last_cursor: &mut Option<(u16, u16)>,
    last_cursor_local: &mut Option<(u16, u16)>,
) -> Result<(), RenderError> {
    let visible = snapshot.cursor_visible()?;
    let viewport = visible
        .then(|| snapshot.cursor_viewport())
        .transpose()?
        .flatten();
    if let Some(viewport) = viewport {
        let absolute = (
            viewport.y.saturating_add(origin.1),
            viewport.x.saturating_add(origin.0),
        );
        write_cup(out, absolute.0, absolute.1)?;
        *last_cursor = Some(absolute);
        *last_cursor_local = Some((viewport.y, viewport.x));
        out.write_all(b"\x1b[?25h")?;
    } else {
        *last_cursor = None;
        *last_cursor_local = None;
        out.write_all(b"\x1b[?25l")?;
    }
    Ok(())
}

fn render_clean_frame_cursor(
    snapshot: &Snapshot<'_, '_>,
    out: &mut impl Write,
    origin: (u16, u16),
    emitted_kitty: bool,
    last_cursor: &mut Option<(u16, u16)>,
    last_cursor_local: &mut Option<(u16, u16)>,
) -> Result<(), RenderError> {
    // No row content changed, but the cursor may have MOVED — a pure cursor
    // advance. Reposition + refresh the cached cursor when it changed.
    let (ox, oy) = origin;
    let cursor_visible = snapshot.cursor_visible()?;
    let new_local = cursor_visible
        .then(|| snapshot.cursor_viewport())
        .transpose()?
        .flatten()
        .map(|v| (v.y, v.x));
    let new_abs = new_local.map(|(y, x)| (y.saturating_add(oy), x.saturating_add(ox)));
    if new_abs == *last_cursor && !emitted_kitty {
        return Ok(());
    }

    // No flush here either: the frame's one flush belongs to
    // `paint::end_of_frame_cursor` (see [`emit_frame_epilogue`]).
    if let Some((abs_y, abs_x)) = new_abs {
        write_cup(out, abs_y, abs_x)?;
        out.write_all(b"\x1b[?25h")?;
    } else if *last_cursor != new_abs {
        out.write_all(b"\x1b[?25l")?;
    }
    *last_cursor = new_abs;
    *last_cursor_local = new_local;
    Ok(())
}

/// Project a cell's `(Style, fg, bg)` into a plain [`CellStyle`] for the
/// rendered-frame path. Mirrors the server synthesizer's `collect_cell` and
/// `phux-record`'s `replay::project_cell`; the cell-projection conformance
/// test in `crates/phux/tests/conformance` holds all three to one corpus.
fn to_cell_style(style: &Style, fg: Option<RgbColor>, bg: Option<RgbColor>) -> CellStyle {
    CellStyle {
        bold: style.bold,
        faint: style.faint,
        italic: style.italic,
        underline: !matches!(style.underline, Underline::None),
        blink: style.blink,
        inverse: style.inverse,
        invisible: style.invisible,
        strikethrough: style.strikethrough,
        overline: style.overline,
        fg: cell_color(fg, style.fg_color),
        bg: cell_color(bg, style.bg_color),
    }
}

/// Project a cell color to [`CellColor`], preferring the explicit per-cell
/// [`StyleColor`] so a palette index keeps its identity, and falling back to
/// the iteration's resolved RGB. Mirrors the synthesizer's `cell_color`.
fn cell_color(resolved: Option<RgbColor>, raw: StyleColor) -> CellColor {
    match raw {
        StyleColor::Palette(index) => CellColor::Palette { index: index.0 },
        StyleColor::Rgb(rgb) => CellColor::Rgb {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
        },
        StyleColor::None => resolved.map_or(CellColor::Default, |rgb| CellColor::Rgb {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
        }),
    }
}

/// DEC 2026 "begin synchronized update" — the outer terminal buffers
/// everything until the matching end and presents it in one composite.
pub(super) const SYNC_OUTPUT_BEGIN: &[u8] = b"\x1b[?2026h";
/// DEC 2026 "end synchronized update".
pub(super) const SYNC_OUTPUT_END: &[u8] = b"\x1b[?2026l";

thread_local! {
    /// How many [`SyncOutput`] guards are open on this thread. DEC 2026 is a
    /// mode, not a counter, so only the outermost guard may emit the mode
    /// bytes. Thread-local because the two nesting layers never see each
    /// other's state and the paint path runs on one thread.
    static SYNC_OUTPUT_DEPTH: core::cell::Cell<u32> = const { core::cell::Cell::new(0) };
}

/// An open DEC 2026 synchronized-output block, nestable: only the outermost
/// `begin`/`end` emit `CSI ? 2026 h`/`l`. The depth is released in `Drop`, so
/// an early return cannot strand it (the driver's sync-output watchdog backs
/// up a transaction left open). Closing is explicit because the guard cannot
/// hold the sink across the paint's borrow of `out`.
#[derive(Debug)]
#[must_use = "an opened synchronized-output block must be closed with `end`"]
pub(super) struct SyncOutput {
    /// Whether THIS guard emitted the begin bytes (i.e. it is the outermost).
    outermost: bool,
}

impl SyncOutput {
    /// Open a synchronized-output block around the emits that follow.
    pub(super) fn begin(out: &mut impl Write) -> io::Result<Self> {
        let outermost = SYNC_OUTPUT_DEPTH.with(|depth| {
            let was = depth.get();
            depth.set(was.saturating_add(1));
            was == 0
        });
        // Bind the guard BEFORE the write so a failing sink drops it — and
        // releases the depth it just took — instead of leaking a level that
        // would silently suppress every later block on this thread.
        let guard = Self { outermost };
        if outermost {
            out.write_all(SYNC_OUTPUT_BEGIN)?;
        }
        Ok(guard)
    }

    /// Close the block, emitting the end bytes only if this guard opened it.
    pub(super) fn end(self, out: &mut impl Write) -> io::Result<()> {
        if self.outermost {
            out.write_all(SYNC_OUTPUT_END)?;
        }
        Ok(())
    }
}

impl Drop for SyncOutput {
    fn drop(&mut self) {
        SYNC_OUTPUT_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// Reset SGR and show the cursor, for teardown paths.
pub fn write_reset(out: &mut impl Write) -> io::Result<()> {
    out.write_all(b"\x1b[0m")?;
    out.write_all(b"\x1b[?25h")?;
    out.flush()
}

// CURSOR-AUTHORITY: the canonical CUP formatter. The composite end-of-frame
// emitter (paint::end_of_frame_cursor) and the pane-interior renderer both
// route cursor moves through this one place (ADR-0029); raw `\x1b[..H`
// elsewhere under attach/ is banned.
pub(super) fn write_cup(out: &mut impl Write, row: u16, col: u16) -> io::Result<()> {
    let r = row.saturating_add(1);
    let c = col.saturating_add(1);
    write!(out, "\x1b[{r};{c}H")
}

/// The centred placement of a mirror within a render rect (ADR-0027): the
/// content's `inner_origin`/`inner_clip` for the core paint, and the four
/// margin bars [`emit_letterbox_margins`] blanks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Letterbox {
    /// Centred top-left of the mirror content (rect origin + pad).
    inner_origin: (u16, u16),
    /// Painted extent `min(mirror, rect)` on each axis — the clamp the
    /// existing `render_at` already applies, so a mirror >= the rect is
    /// confined to the rect with no pad.
    inner_clip: (u16, u16),
    /// Left pad width in columns (`= inner_origin.0 - rect_origin.0`).
    margin_left: u16,
    /// Right pad width in columns (the floor split's extra cell lands here).
    margin_right: u16,
    /// Top pad height in rows (`= inner_origin.1 - rect_origin.1`).
    margin_top: u16,
    /// Bottom pad height in rows (the floor split's extra cell lands here).
    margin_bottom: u16,
    /// The rect's outer origin `(x, y)`, retained so the margin emitter can
    /// position the bars without re-deriving it from the pads.
    rect_origin: (u16, u16),
    /// The rect's full extent `(cols, rows)`, retained for the same reason.
    rect_clip: (u16, u16),
}

impl Letterbox {
    /// Whether any margin bar exists (`false` in the clamp case, which paints
    /// byte-identically to [`TerminalRenderer::render_at`]).
    const fn has_pad(self) -> bool {
        self.margin_left > 0
            || self.margin_right > 0
            || self.margin_top > 0
            || self.margin_bottom > 0
    }
}

/// Centre `mirror` within the rect at `rect_origin` spanning `rect_clip`.
/// Per axis a smaller mirror's gap is floor-split (extra cell bottom/right);
/// a larger or equal one gets no pad and a clip clamped to the rect.
fn letterbox_rect(rect_origin: (u16, u16), rect_clip: (u16, u16), mirror: (u16, u16)) -> Letterbox {
    let (rx, ry) = rect_origin;
    let (rect_cols, rect_rows) = rect_clip;
    let (mirror_cols, mirror_rows) = mirror;

    // Per-axis: clamp the painted extent to the rect, then centre the gap with
    // the floor split (extra cell on the trailing edge).
    let inner_cols = mirror_cols.min(rect_cols);
    let inner_rows = mirror_rows.min(rect_rows);
    let gap_x = rect_cols.saturating_sub(mirror_cols);
    let gap_y = rect_rows.saturating_sub(mirror_rows);
    let margin_left = gap_x / 2;
    let margin_right = gap_x - margin_left;
    let margin_top = gap_y / 2;
    let margin_bottom = gap_y - margin_top;

    Letterbox {
        inner_origin: (
            rx.saturating_add(margin_left),
            ry.saturating_add(margin_top),
        ),
        inner_clip: (inner_cols, inner_rows),
        margin_left,
        margin_right,
        margin_top,
        margin_bottom,
        rect_origin,
        rect_clip,
    }
}

/// Blank the margin bars of a [`Letterbox`]: top and bottom span the rect
/// width, left and right only the interior rows. No pad emits nothing.
fn emit_letterbox_margins(out: &mut impl Write, lb: Letterbox) -> io::Result<()> {
    if !lb.has_pad() {
        return Ok(());
    }
    // Reset SGR so the blanks paint in the default (background) style and no
    // prior run's attributes leak into the bars.
    out.write_all(b"\x1b[0m")?;

    let (rx, ry) = lb.rect_origin;
    let (rect_cols, rect_rows) = lb.rect_clip;
    let content_top = ry.saturating_add(lb.margin_top);
    let content_bottom = content_top.saturating_add(lb.inner_clip.1);

    // Top bar: full-width rows above the centred content.
    emit_blank_rows(out, ry, content_top, rx, rect_cols)?;
    // Bottom bar: full-width rows below the centred content.
    emit_blank_rows(
        out,
        content_bottom,
        ry.saturating_add(rect_rows),
        rx,
        rect_cols,
    )?;
    // Left/right bars: only the interior rows (the top/bottom bars already
    // cleared the corners).
    emit_side_margin_bars(out, lb, content_top, content_bottom)
}

/// Blank `cols` cells starting at column `col` on every row in `[first, end)`.
fn emit_blank_rows(
    out: &mut impl Write,
    first: u16,
    end: u16,
    col: u16,
    cols: u16,
) -> io::Result<()> {
    for row in first..end {
        write_cup(out, row, col)?;
        write_blank_run(out, cols)?;
    }
    Ok(())
}

/// Blank the left and right margin bars across the interior rows
/// `[top, bottom)` — the rows the centred content occupies.
fn emit_side_margin_bars(
    out: &mut impl Write,
    lb: Letterbox,
    top: u16,
    bottom: u16,
) -> io::Result<()> {
    let (rx, _) = lb.rect_origin;
    let right_col = rx
        .saturating_add(lb.margin_left)
        .saturating_add(lb.inner_clip.0);
    for row in top..bottom {
        if lb.margin_left > 0 {
            write_cup(out, row, rx)?;
            write_blank_run(out, lb.margin_left)?;
        }
        if lb.margin_right > 0 {
            write_cup(out, row, right_col)?;
            write_blank_run(out, lb.margin_right)?;
        }
    }
    Ok(())
}

/// A read-only run of spaces the blank-fill paths slice, so filling a margin
/// bar costs no allocation. 256 is wider than any realistic terminal column
/// count; [`write_blank_run`] loops for anything wider.
const BLANK_RUN: [u8; 256] = [b' '; 256];

/// Write `n` blank (space) cells — the margin-bar fill.
fn write_blank_run(out: &mut impl Write, n: u16) -> io::Result<()> {
    let mut remaining = usize::from(n);
    while remaining > 0 {
        let chunk = remaining.min(BLANK_RUN.len());
        out.write_all(&BLANK_RUN[..chunk])?;
        remaining -= chunk;
    }
    Ok(())
}

/// The pen active on the outer terminal. `fg`/`bg` are the resolved RGB
/// colours from the cell iterator, not `Style`'s palette fields.
type EmittedStyle = (Style, Option<RgbColor>, Option<RgbColor>);

/// A cell's pen identity within ONE row read, in 20 bytes.
///
/// `style_index` is a style-RUN index (a returned-to style gets a fresh one),
/// so equal indices prove adjacent members of one run and the fast path can
/// only skip redundant work; a different index still does the full
/// `(Style, fg, bg)` comparison. Indices never cross rows; across rows and
/// frames pens compare by value ([`PenMemo`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PenKey {
    /// This cell's run within the row's style table — valid only within the
    /// `read_row` that produced it.
    style_index: u32,
    /// The cell's resolved foreground.
    fg: Option<RgbColor>,
    /// The cell's resolved background.
    bg: Option<RgbColor>,
    /// How copy-mode restyles this cell (selection or search hit).
    mark: CellMark,
}

/// Whether a `(style, fg, bg)` triple renders as the terminal default — no
/// attributes and no explicit colors. Such a run needs only a plain `\x1b[0m`
/// reset (which `emit_sgr_set` already produces), and at row start the active
/// state is already default, so it emits nothing at all.
fn is_default_render(style: &Style, fg: Option<RgbColor>, bg: Option<RgbColor>) -> bool {
    fg.is_none() && bg.is_none() && *style == Style::default()
}

/// Emit SGR only when `(style, fg, bg)` differs from `emitted` (`None` =
/// default, true at row start after the leading reset), so a same-style run
/// costs one sequence.
fn emit_sgr_if_changed(
    out: &mut Vec<u8>,
    emitted: &mut Option<EmittedStyle>,
    style: Style,
    fg: Option<RgbColor>,
    bg: Option<RgbColor>,
) {
    if is_default_render(&style, fg, bg) {
        // Returning to default mid-row needs an explicit reset; at row start
        // (`emitted == None`) the default is already active, so skip it.
        if emitted.is_some() {
            out.extend_from_slice(b"\x1b[0m");
            *emitted = None;
        }
        return;
    }

    let key = (style, fg, bg);
    if *emitted == Some(key) {
        return;
    }
    emit_sgr_set(out, &style, fg, bg);
    *emitted = Some(key);
}

/// The copy-mode mark on a cell; a wide cell takes its spacer column's mark
/// when its own column carries none.
fn cell_mark(marks: &CopyMarks, row: u16, col: u16, wide: CellWide) -> CellMark {
    match marks.mark(row, col) {
        CellMark::None if matches!(wide, CellWide::Wide) => marks.mark(row, col.saturating_add(1)),
        mark => mark,
    }
}

/// Restyle a cell for its copy-mode mark: a selected cell flips reverse
/// video, a search hit is underlined.
const fn apply_mark(style: &mut Style, mark: CellMark) {
    match mark {
        CellMark::None => {}
        CellMark::Selected => style.inverse ^= true,
        CellMark::Matched => style.underline = Underline::Single,
    }
}

/// Write `\x1b[0m` followed by the SGR set for `(style, fg, bg)`.
fn emit_sgr_set(out: &mut Vec<u8>, style: &Style, fg: Option<RgbColor>, bg: Option<RgbColor>) {
    // Encode via the shared server/client SGR emitter (phux-protocol) so the
    // two ends cannot drift — they previously both dropped underline/overline.
    // It appends straight to the row buffer, so a style run costs no scratch
    // allocation of its own.
    write_reset_and_sgr(out, style, fg, bg);
}

fn emit_cursor_style(
    out: &mut impl Write,
    style: CursorVisualStyle,
    blinking: bool,
) -> io::Result<()> {
    let code: u8 = match (style, blinking) {
        (CursorVisualStyle::Block, true) => 1,
        (CursorVisualStyle::Underline, true) => 3,
        (CursorVisualStyle::Underline, false) => 4,
        (CursorVisualStyle::Bar, true) => 5,
        (CursorVisualStyle::Bar, false) => 6,
        _ => 2,
    };
    write!(out, "\x1b[{code} q")
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use libghostty_vt::{
        RenderState, Terminal as GhosttyTerminal,
        render::{CellIterator, RowIterator},
    };

    type Pane = GhosttyTerminal<'static, 'static>;

    fn fresh(cols: u16, rows: u16) -> Pane {
        let mut terminal = GhosttyTerminal::new(cols, rows).expect("Terminal::new");
        terminal
            .set_scrollback_max_lines(Some(100))
            .expect("Terminal::new");
        terminal
    }

    fn pane(cols: u16, rows: u16, bytes: &[u8]) -> Pane {
        let mut t = fresh(cols, rows);
        t.vt_write(bytes);
        t
    }

    fn renderer() -> TerminalRenderer<'static> {
        TerminalRenderer::new().expect("renderer")
    }

    /// One incremental `render` of `t` through `r`.
    fn paint(r: &mut TerminalRenderer<'static>, t: &Pane) -> (Dirty, Vec<u8>) {
        let mut buf = Vec::new();
        let dirty = r
            .render(ReplicaWalk::for_test(t), &mut buf)
            .expect("render");
        (dirty, buf)
    }

    /// A forced paint of `t` at `origin`, clipped to `clip`.
    fn full(
        r: &mut TerminalRenderer<'static>,
        t: &Pane,
        origin: (u16, u16),
        clip: (u16, u16),
    ) -> String {
        let mut buf = Vec::new();
        r.render_at_full(ReplicaWalk::for_test(t), &mut buf, origin, clip)
            .expect("render_at_full");
        String::from_utf8_lossy(&buf).into_owned()
    }

    fn letterboxed(
        r: &mut TerminalRenderer<'static>,
        t: &Pane,
        rect: (u16, u16),
        mirror: (u16, u16),
    ) -> Vec<u8> {
        let mut out = Vec::new();
        r.render_at_letterboxed(
            ReplicaWalk::for_test(t),
            &mut out,
            (0, 0),
            rect,
            mirror,
            true,
        )
        .expect("render_at_letterboxed");
        out
    }

    fn render_once(terminal: &Pane) -> Vec<u8> {
        paint(&mut renderer(), terminal).1
    }

    fn count(hay: &[u8], needle: &[u8]) -> usize {
        hay.windows(needle.len()).filter(|w| *w == needle).count()
    }

    fn sel(start: (u16, u16), end: (u16, u16), rectangle: bool) -> SelectionRect {
        SelectionRect {
            start_row: start.0,
            start_col: start.1,
            end_row: end.0,
            end_col: end.1,
            rectangle,
        }
    }

    // ---- selection ---------------------------------------------------------

    /// Copy mode reverse-videos (SGR 7) the selected cells over the real
    /// content, with no clear and no separate overlay surface.
    #[test]
    fn selection_emits_reverse_video_for_selected_cells() {
        let t = pane(10, 2, b"hello");
        let mut r = renderer();
        r.set_selection(Some(sel((0, 0), (0, 1), false)));
        let s = full(&mut r, &t, (0, 0), (10, 2));
        assert!(
            s.contains("\x1b[7") && s.contains("he") && s.contains("llo"),
            "{s:?}"
        );
        r.set_selection(None);
        assert!(!full(&mut r, &t, (0, 0), (10, 2)).contains("\x1b[7"));
    }

    /// Block and linear selections invert different cells: linear takes the
    /// whole interior row, block only the column band.
    #[test]
    fn block_and_linear_selection_invert_different_cells() {
        let t = pane(8, 3, b"ABCDEFGH\r\nabcdefgh\r\n01234567");
        let mut r = renderer();
        let mut render = |rectangle| {
            r.set_selection(Some(sel((0, 2), (2, 5), rectangle)));
            full(&mut r, &t, (0, 0), (8, 3))
        };
        let (linear, block) = (render(false), render(true));
        assert!(linear.contains("\x1b[7") && block.contains("\x1b[7"));
        assert_ne!(linear, block);
        // "ab" is unique to the interior row: plain after the row reset only
        // in block mode.
        assert!(block.contains("\x1b[0mab"), "{block:?}");
        assert!(!linear.contains("\x1b[0mab"), "{linear:?}");
        assert!(block.contains("cdef") && linear.contains("cdef"));
    }

    /// Copy-mode search underlines the other visible hits and leaves the
    /// current one to the reverse-video selection.
    #[test]
    fn search_hits_are_underlined_and_the_selection_still_inverts() {
        let t = pane(12, 1, b"foo bar foo");
        let mut r = renderer();
        r.set_copy_marks(CopyMarks {
            selection: Some(sel((0, 0), (0, 2), false)),
            matches: vec![sel((0, 8), (0, 10), false)],
        });
        let s = full(&mut r, &t, (0, 0), (12, 1));
        let inverse = s.find("\x1b[7").expect("the current hit inverts");
        let underline = s.find("\x1b[4").expect("the other hit underlines");
        assert!(
            inverse < s.find(" bar").expect("plain text") && underline > inverse,
            "{s:?}"
        );
        r.set_copy_marks(CopyMarks::default());
        let plain = full(&mut r, &t, (0, 0), (12, 1));
        assert!(
            !plain.contains("\x1b[4") && !plain.contains("\x1b[7"),
            "{plain:?}"
        );
    }

    #[test]
    fn selecting_only_a_wide_tail_highlights_its_base_glyph() {
        let t = pane(6, 1, "世X".as_bytes());
        let mut r = renderer();
        r.set_selection(Some(sel((0, 1), (0, 1), true)));
        let s = full(&mut r, &t, (0, 0), (6, 1));
        let inverse = s.find("\x1b[7").expect("tail selection is visible");
        assert!(inverse < s.find('世').expect("wide glyph"), "{s:?}");
    }

    // ---- pooled render state -----------------------------------------------

    /// ADR-0086: a geometry change rebuilds the pooled render state so a
    /// post-resize paint serves the live grid, not stale pooled rows.
    #[test]
    fn pooled_render_state_is_rebuilt_after_a_geometry_change() {
        let mut t = pane(10, 2, b"AA");
        let mut r = renderer();
        let _ = full(&mut r, &t, (0, 0), (10, 2));
        assert_eq!(r.pool.last_dims(), Some((10, 2)));
        // Grow, overwrite, and consume the row dirty bits through a separate
        // render state: the shape that leaves a pooled cache stale.
        t.resize(10, 4, 0, 0).expect("resize");
        t.vt_write(b"\x1b[1;1HZZ");
        let _ = read_seen(&t, (0, 0), (10, 4));
        let painted = full(&mut r, &t, (0, 0), (10, 4));
        assert_eq!(r.pool.last_dims(), Some((10, 4)));
        assert!(painted.contains("ZZ"), "{painted:?}");
    }

    /// A generation change rebuilds the pooled state even at identical
    /// geometry: a replaced `Terminal` must repaint every row.
    #[test]
    fn pooled_render_state_is_rebuilt_after_a_generation_change() {
        let t = pane(10, 2, b"AA");
        let mut r = renderer();
        let mut at = |generation| {
            let mut out = Vec::new();
            r.render_at(
                ReplicaWalk {
                    terminal: &t,
                    generation,
                },
                &mut out,
                (0, 0),
                (10, 2),
            )
            .expect("paint");
            String::from_utf8_lossy(&out).contains("AA")
        };
        assert!(at(1), "first paint serves the rows");
        assert!(
            !at(1),
            "same generation with no writes must not repaint rows"
        );
        assert!(at(2), "a generation change must force a repaint");
    }

    // ---- frame transaction and cursor ---------------------------------------

    /// A dirty frame is one DEC 2026 transaction opening with a cursor hide;
    /// an unchanged second render is `Clean` and emits zero bytes.
    #[test]
    fn dirty_frames_are_one_transaction_and_clean_frames_are_silent() {
        let t = pane(5, 2, b"ab");
        let mut r = renderer();
        let (_, buf) = paint(&mut r, &t);
        let mut prefix = SYNC_OUTPUT_BEGIN.to_vec();
        prefix.extend_from_slice(b"\x1b[?25l");
        assert!(
            buf.starts_with(&prefix) && buf.ends_with(SYNC_OUTPUT_END),
            "{buf:?}"
        );
        let s = String::from_utf8_lossy(&buf);
        assert!(s.contains('a') && s.contains('b'));
        let (dirty, again) = paint(&mut r, &t);
        assert!(matches!(dirty, Dirty::Clean));
        assert!(again.is_empty(), "clean frame emitted {again:?}");
    }

    /// Blocks nest (an inner one emits nothing, so a pane paint cannot end a
    /// frame-level transaction early), and a failed begin must not leak depth
    /// (every later frame would silently go unsynchronized).
    #[test]
    fn synchronized_output_blocks_nest_and_failures_release_depth() {
        struct FailingSink;
        impl Write for FailingSink {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("sink closed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let outermost = || {
            let mut out: Vec<u8> = Vec::new();
            let guard = SyncOutput::begin(&mut out).expect("begin");
            assert_eq!(out, SYNC_OUTPUT_BEGIN);
            (guard, out)
        };
        let (guard, mut outer) = outermost();
        let mut inner: Vec<u8> = Vec::new();
        SyncOutput::begin(&mut inner)
            .expect("inner")
            .end(&mut inner)
            .expect("inner end");
        assert!(inner.is_empty(), "a nested block must emit nothing");
        outer.clear();
        guard.end(&mut outer).expect("end");
        assert_eq!(outer, SYNC_OUTPUT_END);

        assert!(SyncOutput::begin(&mut FailingSink).is_err());
        let (guard, mut after) = outermost();
        after.clear();
        guard.end(&mut after).expect("end");
        assert_eq!(after, SYNC_OUTPUT_END);
    }

    /// ADR-0029: the composite end-of-frame owns the single flush; a pane
    /// paint (dirty or cursor-only) never flushes.
    #[test]
    fn pane_paint_does_not_flush() {
        #[derive(Default)]
        struct FlushCounter {
            bytes: usize,
            flushes: usize,
        }
        impl Write for FlushCounter {
            fn write(&mut self, data: &[u8]) -> io::Result<usize> {
                self.bytes += data.len();
                Ok(data.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                self.flushes += 1;
                Ok(())
            }
        }
        let mut t = pane(5, 2, b"ab");
        let mut r = renderer();
        let mut sink = FlushCounter::default();
        let _ = r
            .render(ReplicaWalk::for_test(&t), &mut sink)
            .expect("dirty");
        t.vt_write(b"\x1b[1;1H");
        let _ = r
            .render(ReplicaWalk::for_test(&t), &mut sink)
            .expect("clean");
        assert_eq!(sink.flushes, 0);
        assert!(sink.bytes > 0);
    }

    /// Regression: a pane painted at a non-zero origin caches its cursor both
    /// outer-absolute (host restore) and pane-local (predict anchor); feeding
    /// predict the outer cursor produced the ghost echo.
    #[test]
    fn render_at_offset_caches_pane_local_cursor_and_origin() {
        let t = pane(5, 2, b"ab");
        let mut r = renderer();
        r.render_at(ReplicaWalk::for_test(&t), &mut Vec::new(), (0, 13), (5, 2))
            .expect("render_at");
        assert_eq!(r.last_cursor_local(), Some((0, 2)));
        assert_eq!(r.last_cursor(), Some((13, 2)));
        assert_eq!(r.last_origin(), (0, 13));
    }

    /// A cursor-only move (rows Clean) still repositions the host cursor and
    /// the cache; hiding the cursor drops it from the cache and reaches the
    /// host.
    #[test]
    fn cursor_moves_and_visibility_reach_the_host_on_clean_renders() {
        let mut t = pane(10, 2, b"hello");
        let mut r = renderer();
        let _ = paint(&mut r, &t);
        assert_eq!(r.last_cursor(), Some((0, 5)));
        t.vt_write(b"\x1b[1;3H");
        let (dirty, buf) = paint(&mut r, &t);
        assert!(matches!(dirty, Dirty::Clean));
        assert!(String::from_utf8_lossy(&buf).contains("\x1b[1;3H"));
        assert_eq!(r.last_cursor(), Some((0, 2)));

        t.vt_write(b"\x1b[1;6H\x1b[?25l");
        let (_, hidden) = paint(&mut r, &t);
        assert!(count(&hidden, b"\x1b[?25l") > 0);
        assert_eq!((r.last_cursor(), r.last_cursor_local()), (None, None));
        t.vt_write(b"\x1b[?25h");
        let (_, shown) = paint(&mut r, &t);
        assert!(count(&shown, b"\x1b[?25h") > 0);
        assert_eq!(r.last_cursor(), Some((0, 5)));
    }

    /// Alt-screen exit repaints the restored primary rows plus the new prompt
    /// instead of classifying as Clean.
    #[test]
    fn alt_screen_exit_repaints_restored_primary_screen() {
        let mut t = pane(20, 5, b"$ old-prompt");
        let mut r = renderer();
        let _ = paint(&mut r, &t);
        t.vt_write(b"\x1b[?1049h\x1b[2J\x1b[HTUI-FRAME");
        assert!(String::from_utf8_lossy(&paint(&mut r, &t).1).contains("TUI-FRAME"));
        t.vt_write(b"\x1b[?1049l\r\n$ new-prompt");
        let (dirty, buf) = paint(&mut r, &t);
        let s = String::from_utf8_lossy(&buf);
        assert!(!matches!(dirty, Dirty::Clean));
        assert!(
            s.contains("old-prompt") && s.contains("new-prompt"),
            "{s:?}"
        );
    }

    /// One changed row repaints only that row.
    #[test]
    fn single_row_change_repaints_only_that_row() {
        let mut t = pane(10, 3, b"top\r\nmid");
        let mut r = renderer();
        let _ = paint(&mut r, &t);
        t.vt_write(b"\x1b[2;1HNEW");
        let s = String::from_utf8_lossy(&paint(&mut r, &t).1).into_owned();
        assert!(
            s.contains("\x1b[2;1H") && s.contains('N') && s.contains('W'),
            "{s:?}"
        );
        assert!(!s.contains("\x1b[1;1H") && !s.contains("top"), "{s:?}");
    }

    // ---- SGR coalescing and round trips ------------------------------------

    /// One SGR per style run, not per cell.
    #[test]
    fn sgr_is_emitted_once_per_style_run() {
        let row = |bytes: &[u8]| render_once(&pane(80, 1, bytes));
        let run = [b"\x1b[38;2;120;200;40m".as_slice(), &[b'#'; 80]].concat();
        let buf = row(&run);
        assert_eq!(count(&buf, b"38;2;120;200;40"), 1);
        assert_eq!(count(&buf, b"#"), 80);
        // Pre-coalescing each cell cost reset + SGR + glyph (~23 bytes).
        assert!(buf.len() * 3 < 80 * 23, "{} bytes", buf.len());

        let buf = row(b"\x1b[38;2;1;1;1mA\x1b[38;2;2;2;2mBBB\x1b[38;2;3;3;3mC");
        assert_eq!(count(&buf, b"38;2;2;2;2"), 1);
        assert_eq!(count(&buf, b"BBB"), 1);

        let alternating: Vec<u8> = (0..10)
            .flat_map(|i| {
                let pen: &[u8] = if i % 2 == 0 {
                    b"\x1b[38;2;255;0;0m"
                } else {
                    b"\x1b[38;2;0;255;0m"
                };
                [pen, b"z"].concat()
            })
            .collect();
        let buf = row(&alternating);
        assert_eq!(
            (count(&buf, b"38;2;255;0;0"), count(&buf, b"38;2;0;255;0")),
            (5, 5)
        );
    }

    /// The coalesced output, replayed into a fresh terminal, reconstructs the
    /// source grid exactly: runs, default gaps, bg colours, attributes, a style
    /// the row returns to, cell-tagged backgrounds, wide and combining text.
    #[test]
    fn coalesced_output_round_trips_to_identical_grid() {
        let cases: [(&[u8], (u16, u16)); 4] = [
            (
                b"\x1b[38;2;200;100;50mHELLO\x1b[0m   \x1b[1;48;2;0;0;255mWORLD\r\n\x1b[3;38;2;9;9;9mitalics same color run",
                (24, 3),
            ),
            (
                "\x1b[1;38;2;200;0;0mAAA\x1b[0mBBB\x1b[1;38;2;200;0;0mCCC\x1b[0m\r\n\x1b[48;2;0;0;90m  \x1b[0m\u{6771}e\u{301}x"
                    .as_bytes(),
                (24, 2),
            ),
            (b"\x1b[38;2;7;7;7mAAAAA\x1b[0mBBBBB", (10, 1)),
            ("\u{4e16}X".as_bytes(), (6, 1)),
        ];
        for (bytes, (cols, rows)) in cases {
            let t = pane(cols, rows, bytes);
            let buf = render_once(&t);
            assert_eq!(
                read_seen(&t, (0, 0), (cols, rows)),
                read_seen(&pane(cols, rows, &buf), (0, 0), (cols, rows)),
                "{:?}",
                String::from_utf8_lossy(bytes)
            );
        }
        // A repeated style is still one sequence per run, and a wide glyph's
        // spacer tail emits no intervening space.
        let buf = render_once(&pane(
            24,
            1,
            b"\x1b[1;38;2;200;0;0mAAA\x1b[0mBBB\x1b[1;38;2;200;0;0mCCC",
        ));
        assert_eq!(count(&buf, b"38;2;200;0;0"), 2);
        assert!(
            count(
                &render_once(&pane(6, 1, "世X".as_bytes())),
                "世X".as_bytes()
            ) > 0
        );
    }

    // ---- clipping, cell projection, letterbox -------------------------------

    /// The render clips to the pane rect, not the (possibly larger) mirror
    /// grid, on both axes; painting past it was the divider-overrun ghost.
    #[test]
    fn render_at_clips_to_the_rect_not_the_mirror() {
        let t = pane(20, 1, b"ABCDEFGHIJKLMNOPQRST");
        let mut r = renderer();
        let mut out = Vec::new();
        r.render_at(ReplicaWalk::for_test(&t), &mut out, (0, 0), (12, 1))
            .expect("render");
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains('A') && s.contains('L'), "{s:?}");
        assert!(!s.chars().any(|c| ('M'..='T').contains(&c)), "{s:?}");

        let t = pane(6, 4, b"row0\r\nrow1\r\nrow2\r\nrow3");
        let mut out = Vec::new();
        renderer()
            .render_at(ReplicaWalk::for_test(&t), &mut out, (0, 0), (6, 2))
            .expect("render");
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("\x1b[1;1H") && s.contains("\x1b[2;1H"), "{s:?}");
        assert!(
            !s.contains("\x1b[3;1H") && !s.contains("\x1b[4;1H"),
            "{s:?}"
        );
    }

    /// `render_at_cells` projects graphemes and style into a dense frame,
    /// shifted by the origin, with a wide glyph's tail as the empty grapheme
    /// and the cursor frame-absolute.
    #[test]
    fn render_at_cells_projects_graphemes_style_and_cursor() {
        let t = pane(10, 3, b"\x1b[1mHi\x1b[0m X\r\n\xe4\xb8\x96");
        let mut frame = RenderedFrame::blank(12, 4);
        let cursor = renderer()
            .render_at_cells(ReplicaWalk::for_test(&t), &mut frame, (1, 1), (10, 3))
            .expect("render_at_cells");
        let cell = |r, c| frame.cell(r, c).expect("in range");
        assert_eq!(
            (cell(1, 1).grapheme.as_str(), cell(1, 1).style.bold),
            ("H", true)
        );
        assert_eq!(
            (cell(1, 2).grapheme.as_str(), cell(1, 2).style.bold),
            ("i", true)
        );
        assert_eq!(
            (cell(1, 3).grapheme.as_str(), cell(1, 3).style.bold),
            (" ", false)
        );
        assert_eq!(cell(1, 4).grapheme, "X");
        assert_eq!(cell(0, 0).grapheme, " ", "outside the rect stays blank");
        assert_eq!(cell(2, 1).grapheme, "世");
        assert_eq!(cell(2, 2).grapheme, "", "a wide glyph's tail is empty");
        assert_eq!(cell(2, 3).grapheme, " ");
        let c = cursor.expect("cursor present");
        assert_eq!((c.x, c.y), (3, 2), "pane (row 1, col 2) + origin (1,1)");
    }

    /// Centring math: a floor split puts an odd gap's extra cell on the
    /// bottom/right, offset by the rect origin; a mirror that fills or exceeds
    /// the rect has no pad and clamps to the rect.
    #[test]
    fn letterbox_rect_centers_and_clamps() {
        let margins = |lb: &Letterbox| {
            (
                lb.margin_left,
                lb.margin_right,
                lb.margin_top,
                lb.margin_bottom,
            )
        };
        let lb = letterbox_rect((0, 0), (10, 6), (6, 4));
        assert_eq!(
            (lb.inner_origin, lb.inner_clip, margins(&lb)),
            ((2, 1), (6, 4), (2, 2, 1, 1))
        );
        let lb = letterbox_rect((0, 0), (9, 5), (6, 4));
        assert_eq!((lb.inner_origin, margins(&lb)), ((1, 0), (1, 2, 0, 1)));
        assert_eq!(letterbox_rect((4, 3), (10, 6), (6, 4)).inner_origin, (6, 4));
        for mirror in [(8, 4), (20, 10)] {
            let lb = letterbox_rect((0, 0), (8, 4), mirror);
            assert_eq!(
                (lb.inner_origin, lb.inner_clip, margins(&lb)),
                ((0, 0), (8, 4), (0, 0, 0, 0))
            );
        }
    }

    /// An undersized mirror paints centred with blank margins, and the cached
    /// cursor/origin include the pad.
    #[test]
    fn render_at_letterboxed_centers_undersized_mirror() {
        let t = pane(4, 2, b"WXYZ\r\nMN");
        let mut r = renderer();
        let out = letterboxed(&mut r, &t, (8, 4), (4, 2));
        let grid = read_seen(&pane(8, 4, &out), (0, 0), (8, 4));
        let at = |row: usize, col: usize| grid[row * 8 + col].text.as_str();
        for col in 0..8 {
            assert_eq!(
                (at(0, col), at(3, col)),
                ("", ""),
                "top/bottom margins, col {col}"
            );
        }
        for row in 1..3 {
            for col in [0, 1, 6, 7] {
                assert_eq!(at(row, col), "", "side margin ({row},{col})");
            }
        }
        assert_eq!((at(1, 2), at(1, 5), at(2, 2)), ("W", "Z", "M"));
        assert_eq!(r.last_cursor_local(), Some((1, 2)));
        assert_eq!(r.last_cursor(), Some((2, 4)), "cursor includes the pad");
        assert_eq!(r.last_origin(), (2, 1));
    }

    /// A mirror equal to or larger than the rect paints byte-identically to
    /// `render_at_full` (no margin bars; the larger one clamps).
    #[test]
    fn render_at_letterboxed_without_pad_matches_render_at_full() {
        for (bytes, dims, rect) in [
            (
                &b"\x1b[1mHELLO\x1b[0m world\r\nsecond row\r\nthird"[..],
                (10, 3),
                (10, 3),
            ),
            (
                b"ABCDEFGHIJKLMNOPQRST\r\nabcdefghijklmnopqrst",
                (20, 4),
                (12, 2),
            ),
        ] {
            let expected = full(&mut renderer(), &pane(dims.0, dims.1, bytes), (0, 0), rect);
            let got = letterboxed(&mut renderer(), &pane(dims.0, dims.1, bytes), rect, dims);
            assert_eq!(expected.as_bytes(), got.as_slice());
        }
    }

    // ---- the batched row read ----------------------------------------------

    /// The shared benchmark corpora (compiled in at the crate root).
    use crate::bench_support as support;

    /// A cell source reporting `len` columns but refusing `hole`, the shape
    /// `RowCells::get` takes on a non-UTF-8-boundary cluster.
    struct HolySource {
        len: usize,
        hole: usize,
    }

    impl<'buf> RowCellSource<'buf> for HolySource {
        fn cell_count(&self) -> usize {
            self.len
        }
        fn cell_at(&self, col: usize) -> Option<RowCell<'buf>> {
            (col != self.hole).then_some(RowCell {
                text: "x",
                style_index: 0,
                fg: None,
                bg: None,
                wide: CellWide::Narrow,
            })
        }
    }

    /// An unreadable column stops the walk with an error, never shifts the
    /// rest of the row left; a readable row visits min(row, clip) columns.
    #[test]
    fn row_walk_errors_on_an_unreadable_cell_and_respects_the_clip() {
        let walk = |len, hole, clip| {
            let mut visited: Vec<u16> = Vec::new();
            let result = walk_row_cells(&HolySource { len, hole }, clip, |col, _| {
                visited.push(col);
                Ok(())
            });
            (result, visited)
        };
        let (result, visited) = walk(8, 3, 8);
        assert!(
            matches!(result, Err(RenderError::UnreadableCell { col: 3 })),
            "{result:?}"
        );
        assert_eq!(visited, vec![0, 1, 2]);
        assert_eq!(walk(8, 99, 5).1, vec![0, 1, 2, 3, 4]);
        assert_eq!(walk(3, 99, 40).1, vec![0, 1, 2]);
    }

    /// The rows `paint_dirty_rows` emits for a full-dirty frame, alone.
    fn row_bytes(
        terminal: &Pane,
        extent: (u16, u16),
        selection: Option<SelectionRect>,
        record: bool,
    ) -> Vec<u8> {
        let marks = CopyMarks::selection(selection);
        let mut state = RenderState::new().expect("RenderState");
        let mut rows_it = RowIterator::new().expect("RowIterator");
        let mut cells_it = CellIterator::new().expect("CellIterator");
        let snap = state.update(terminal).expect("snapshot");
        let mut front = FrontBuffer::default();
        front.prepare(
            FrontKey {
                origin: (0, 0),
                extent,
                generation: 1,
                alt_screen: false,
            },
            true,
        );
        let mut out = Vec::new();
        let mut row_iter = rows_it.update(&snap).expect("rows");
        paint_dirty_rows(
            &mut out,
            &mut CellScratch::default(),
            &mut front,
            &mut row_iter,
            &mut cells_it,
            Dirty::Full,
            (0, 0),
            extent,
            &marks,
            record,
        )
        .expect("paint");
        out
    }

    /// Build one benchmark corpus, mirroring the bench's `build_terminal`.
    fn corpus_terminal(corpus: support::Corpus) -> Pane {
        let (cols, rows) = corpus.geometry();
        let mut terminal = GhosttyTerminal::new(cols, rows).expect("corpus terminal");
        terminal
            .set_scrollback_max_lines(Some(corpus.history_lines().max(1_000)))
            .expect("corpus terminal");
        match corpus {
            support::Corpus::Shell80x24 => {
                terminal.vt_write(b"$ printf 'ready\\n'\r\nready\r\n$ ");
                terminal.vt_write(b"\x1b[1;32mbranch\x1b[0m feat/negotiated-libghostty-codec\r\n");
                terminal.vt_write(
                    "wide: \u{6771}\u{4eac} \u{1f980} combining: e\u{301}\r\n".as_bytes(),
                );
            }
            support::Corpus::Tui200x60 | support::Corpus::Unicode50k => {
                terminal.vt_write(b"\x1b[?1049h\x1b[2J\x1b[H");
                for row in 0..rows {
                    let line = format!(
                        "\x1b[{};1H\x1b[38;5;{}m{:03} {:<170}\x1b[0m",
                        row + 1,
                        16 + (u32::from(row) * 37 % 216),
                        row,
                        support::deterministic_line(usize::from(row)).trim_end(),
                    );
                    terminal.vt_write(line.as_bytes());
                }
                terminal.vt_write(b"\x1b[30;70H\x1b[7m ACTIVE \x1b[0m");
            }
        }
        terminal
    }

    /// A row ending on a wide glyph's spacer tail, background-only cells, a
    /// returned-to style with a combining cluster, and a blank row.
    fn edge_case_terminal() -> Pane {
        pane(
            8,
            4,
            "abcdef\u{6771}\r\n\x1b[48;2;0;0;90m\x1b[K\x1b[0m\r\n\x1b[1;4;38;2;9;9;9mAA\x1b[0mB\x1b[1;4;38;2;9;9;9mCe\u{301}\r\n"
                .as_bytes(),
        )
    }

    /// Whole-row paints that record the front buffer and forced ones that do
    /// not emit the same bytes, and those bytes reconstruct the grid, over
    /// every bench corpus and the edge cases, with and without a selection.
    #[test]
    fn whole_row_paints_agree_and_reconstruct_the_grid() {
        let mut cases: Vec<(String, Pane, (u16, u16))> = support::Corpus::ALL
            .into_iter()
            .map(|c| (c.label().to_owned(), corpus_terminal(c), c.geometry()))
            .collect();
        cases.push(("edge cases".to_owned(), edge_case_terminal(), (8, 4)));
        for (label, terminal, extent) in &cases {
            let recorded = row_bytes(terminal, *extent, None, true);
            assert!(!recorded.is_empty(), "{label}");
            assert_eq!(
                recorded,
                row_bytes(terminal, *extent, None, false),
                "{label}"
            );
            if *extent == (8, 4) {
                assert_same_screen(
                    label,
                    &read_seen(terminal, (0, 0), *extent),
                    &read_seen(&pane(8, 4, &recorded), (0, 0), *extent),
                    8,
                );
            }
        }
        // Selection edges on and around the wide pair at cols 6-7.
        for end_col in [5u16, 6, 7] {
            for rectangle in [false, true] {
                let selection = Some(sel((0, 1), (2, end_col), rectangle));
                let t = edge_case_terminal();
                assert_eq!(
                    row_bytes(&t, (8, 4), selection, true),
                    row_bytes(&edge_case_terminal(), (8, 4), selection, false),
                    "end_col={end_col} rectangle={rectangle}"
                );
            }
        }
    }

    // ---- the cell-diff paint and its front buffer ---------------------------

    /// The text attributes a cell carries, all of which the pen emits.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[allow(
        clippy::struct_excessive_bools,
        reason = "mirrors libghostty's own independent Style flags"
    )]
    struct VisAttrs {
        bold: bool,
        faint: bool,
        italic: bool,
        underline: Underline,
        blink: bool,
        inverse: bool,
        invisible: bool,
        strikethrough: bool,
        overline: bool,
        underline_color: StyleColor,
    }

    impl VisAttrs {
        fn of(style: &Style) -> Self {
            Self {
                bold: style.bold,
                faint: style.faint,
                italic: style.italic,
                underline: style.underline,
                blink: style.blink,
                inverse: style.inverse,
                invisible: style.invisible,
                strikethrough: style.strikethrough,
                overline: style.overline,
                underline_color: style.underline_color,
            }
        }
    }

    /// One screen cell as a viewer sees it. A blank and a written space are
    /// the same verdict, and a spacer head reads as the blank the renderer
    /// paints for it.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Seen {
        text: String,
        wide: CellWide,
        fg: Option<RgbColor>,
        bg: Option<RgbColor>,
        attrs: VisAttrs,
    }

    /// Read the `extent` region of `terminal` whose top-left is `origin`.
    fn read_seen(
        terminal: &GhosttyTerminal<'_, '_>,
        origin: (u16, u16),
        extent: (u16, u16),
    ) -> Vec<Seen> {
        let ((ox, oy), (cols, rows)) = (origin, extent);
        let mut state = RenderState::new().expect("RenderState");
        let mut rows_it = RowIterator::new().expect("RowIterator");
        let mut cells_it = CellIterator::new().expect("CellIterator");
        let snap = state.update(terminal).expect("snapshot");
        let mut out = Vec::new();
        let mut row_iter = rows_it.update(&snap).expect("rows");
        let mut y: u16 = 0;
        while let Some(row) = row_iter.next() {
            if y >= oy.saturating_add(rows) {
                break;
            }
            if y >= oy {
                let mut cell_iter = cells_it.update(row).expect("cells");
                let mut x: u16 = 0;
                while let Some(cell) = cell_iter.next() {
                    if x >= ox.saturating_add(cols) {
                        break;
                    }
                    if x >= ox {
                        let mut text = String::new();
                        cell.graphemes_utf8(&mut text).expect("graphemes");
                        if text == " " {
                            text.clear();
                        }
                        let wide = match cell.raw_cell().expect("raw").wide().expect("wide") {
                            CellWide::SpacerHead => CellWide::Narrow,
                            other => other,
                        };
                        out.push(Seen {
                            text,
                            wide,
                            fg: cell.fg_color().expect("fg"),
                            bg: cell.bg_color().expect("bg"),
                            attrs: VisAttrs::of(&cell.style().expect("style")),
                        });
                    }
                    x += 1;
                }
            }
            y += 1;
        }
        out
    }

    /// The outer terminal: a libghostty grid the renderer's bytes are
    /// replayed into, frame after frame.
    struct Glass {
        screen: Pane,
    }

    impl Glass {
        fn new(cols: u16, rows: u16) -> Self {
            Self {
                screen: fresh(cols, rows),
            }
        }

        /// Paint `pane` at `origin`, replay the bytes onto the glass, and
        /// return them.
        fn paint(
            &mut self,
            renderer: &mut TerminalRenderer<'static>,
            pane: &Pane,
            origin: (u16, u16),
            force: bool,
        ) -> Vec<u8> {
            let (clip, walk) = (pane_extent(pane), ReplicaWalk::for_test(pane));
            let mut out = Vec::new();
            if force {
                renderer.render_at_full(walk, &mut out, origin, clip)
            } else {
                renderer.render_at(walk, &mut out, origin, clip)
            }
            .expect("render");
            self.screen.vt_write(&out);
            out
        }

        /// Write onto the glass behind the renderer's back.
        fn scribble(&mut self, bytes: &[u8]) {
            self.screen.vt_write(bytes);
        }

        fn seen(&self, origin: (u16, u16), extent: (u16, u16)) -> Vec<Seen> {
            read_seen(&self.screen, origin, extent)
        }
    }

    fn pane_extent(pane: &GhosttyTerminal<'_, '_>) -> (u16, u16) {
        (pane.cols().expect("cols"), pane.rows().expect("rows"))
    }

    fn assert_glass_shows(glass: &Glass, pane: &Pane, origin: (u16, u16), label: &str) {
        let extent = pane_extent(pane);
        assert_same_screen(
            label,
            &read_seen(pane, (0, 0), extent),
            &glass.seen(origin, extent),
            extent.0,
        );
    }

    fn assert_same_screen(label: &str, want: &[Seen], got: &[Seen], cols: u16) {
        if want == got {
            return;
        }
        let at = want
            .iter()
            .zip(got)
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| want.len().min(got.len()));
        let cols = usize::from(cols.max(1));
        panic!(
            "{label}: screens diverge at row {}, col {}\n  want: {:?}\n  got:  {:?}\nwant grid:\n{}got grid:\n{}",
            at / cols,
            at % cols,
            want.get(at),
            got.get(at),
            dump_screen(want, cols),
            dump_screen(got, cols),
        );
    }

    /// A readable grid for a failure: `.` blank, `~` spacer tail, styled
    /// cells bracketed.
    fn dump_screen(cells: &[Seen], cols: usize) -> String {
        let mut out = String::new();
        for row in cells.chunks(cols) {
            out.push_str("    |");
            for cell in row {
                let text = match (cell.wide, cell.text.as_str()) {
                    (CellWide::SpacerTail, _) => "~",
                    (_, "") => ".",
                    (_, text) => text,
                };
                let plain = cell.fg.is_none()
                    && cell.bg.is_none()
                    && cell.attrs == VisAttrs::of(&Style::default());
                if plain {
                    out.push_str(text);
                } else {
                    out.push('[');
                    out.push_str(text);
                    out.push(']');
                }
            }
            out.push_str("|\n");
        }
        out
    }

    /// The printable text in `bytes`, every escape sequence removed. Written
    /// over raw bytes: bracket literals throw `lizard`'s parse off.
    fn printed(bytes: &[u8]) -> String {
        const ESC: u8 = 0x1b;
        const CSI: u8 = 0x5b;
        const STRING_OPENERS: [u8; 3] = [0x5f, 0x5d, 0x50];
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let byte = bytes[i];
            i += 1;
            if byte != ESC {
                out.push(byte);
                continue;
            }
            let kind = bytes.get(i).copied().unwrap_or(0);
            i += 1;
            if kind == CSI {
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                i += 1;
            } else if STRING_OPENERS.contains(&kind) {
                i = string_end(bytes, i);
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// One past the BEL or ST that ends the control string starting at `i`.
    fn string_end(bytes: &[u8], mut i: usize) -> usize {
        while i < bytes.len() {
            if bytes[i] == 0x07 {
                return i + 1;
            }
            if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&0x5c) {
                return i + 2;
            }
            i += 1;
        }
        i
    }

    /// Paint `initial` onto a fresh glass, apply `edit`, paint incrementally,
    /// and return the second frame after checking the glass matches the pane.
    fn diff_frame(cols: u16, rows: u16, initial: &[u8], edit: &[u8], disturb: &[u8]) -> Vec<u8> {
        let mut pane = pane(cols, rows, initial);
        let mut renderer = renderer();
        let mut glass = Glass::new(cols, rows);
        let _ = glass.paint(&mut renderer, &pane, (0, 0), false);
        glass.scribble(disturb);
        pane.vt_write(edit);
        let frame = glass.paint(&mut renderer, &pane, (0, 0), false);
        assert_glass_shows(&glass, &pane, (0, 0), &String::from_utf8_lossy(edit));
        frame
    }

    /// The diff writes only what changed: one cell costs one positioned
    /// glyph; an identical rewrite costs nothing; a one-cell gap is bridged by
    /// rewriting it rather than jumping.
    #[test]
    fn the_diff_writes_only_changed_cells() {
        let frame = diff_frame(
            20,
            3,
            b"\x1b[1;32mhello world\x1b[0m\r\nsecond row here\r\nthird",
            b"\x1b[2;8HX",
            b"",
        );
        assert_eq!(printed(&frame), "X");
        assert!(String::from_utf8_lossy(&frame).contains("\x1b[2;8H"));

        assert_eq!(
            printed(&diff_frame(10, 2, b"abc", b"\x1b[1;1Habc", b"")),
            ""
        );

        let frame = diff_frame(40, 1, b"0123456789abcdefghij", b"\x1b[1;3HX\x1b[1;5HY", b"");
        assert!(printed(&frame).contains("X3Y"));
        assert!(!String::from_utf8_lossy(&frame).contains("\x1b[1;5H"));
    }

    /// Within one pane paint the pen carries across jumps (three same-pen
    /// spans cost one SGR), but the FIRST span sets its pen from scratch
    /// whatever another writer left on the glass.
    #[test]
    fn the_pen_carries_across_jumps_but_not_into_a_paint() {
        let frame = diff_frame(
            30,
            3,
            b"abcdefghijklmnopqrstuvwxyz\r\nabcdefghijklmnopqrstuvwxyz",
            b"\x1b[1;38;2;0;200;0m\x1b[1;3HX\x1b[1;20HY\x1b[2;7HZ\x1b[0m",
            b"",
        );
        let s = String::from_utf8_lossy(&frame);
        assert_eq!(count(&frame, b"38;2;0;200;0"), 1, "{s:?}");
        assert_eq!(count(&frame, b"\x1b[0m"), 2, "{s:?}");
        assert!(
            s.contains("\x1b[1;20HY") && s.contains("\x1b[2;7HZ"),
            "{s:?}"
        );

        let frame = diff_frame(
            30,
            2,
            b"\x1b[1;31mred bold text\x1b[0m and plain",
            b"\x1b[1;20Hq",
            b"\x1b[1;31m",
        );
        let s = String::from_utf8_lossy(&frame);
        assert!(
            s[..s.find('q').expect("change")].ends_with("\x1b[0m"),
            "{s:?}"
        );
    }

    /// The motivating shape (cmatrix): every row dirty, few cells changed,
    /// costs a small fraction of a repaint.
    #[test]
    fn a_full_dirty_frame_with_few_changes_costs_a_fraction_of_a_repaint() {
        let (cols, rows) = (80u16, 24u16);
        let mut pane = fresh(cols, rows);
        for r in 0..rows {
            let text: String = support::deterministic_line(usize::from(r))
                .chars()
                .take(usize::from(cols))
                .collect();
            let line = format!(
                "\x1b[{};1H\x1b[38;5;{}m{text:<width$}",
                r + 1,
                16 + u32::from(r) * 7,
                width = usize::from(cols) - 1
            );
            pane.vt_write(line.as_bytes());
        }
        let mut renderer = renderer();
        let mut glass = Glass::new(cols, rows);
        let repaint = glass.paint(&mut renderer, &pane, (0, 0), false);
        for r in 0..rows {
            pane.vt_write(
                format!("\x1b[{};{}H\x1b[1;38;5;46m#", r + 1, (r * 3) % cols + 1).as_bytes(),
            );
        }
        let frame = glass.paint(&mut renderer, &pane, (0, 0), false);
        assert!(
            frame.len() * 3 < repaint.len(),
            "{} vs {}",
            frame.len(),
            repaint.len()
        );
        assert_eq!(printed(&frame).matches('#').count(), usize::from(rows));
        assert_glass_shows(&glass, &pane, (0, 0), "few changes per dirty row");
    }

    /// A writer outside the renderer (a modal box, a predictive-echo guess)
    /// heals only once the front is invalidated for its rows; each control
    /// run proves the case has teeth.
    #[test]
    fn scribbles_heal_once_their_rows_are_forgotten() {
        type Forget = fn(&mut TerminalRenderer<'static>);
        type Case = (&'static [u8], &'static [u8], &'static [u8], Forget);
        let whole: Forget = TerminalRenderer::invalidate_front;
        let row0: Forget = |r| r.invalidate_front_rows(0..1);
        let cases: [Case; 2] = [
            (
                b"row zero\r\nrow one is here\r\nrow two\r\nrow three",
                b"\x1b[2;1H\x1b[7m### MODAL ###\x1b[0m",
                b"\x1b[2;18HZ",
                whole,
            ),
            (
                b"$ abc",
                b"\x1b[1;5H\x1b[0m\x1b[4m \x1b[0m",
                b"\x1b[1;15H!",
                row0,
            ),
        ];
        for (initial, scribble, edit, forget) in cases {
            for invalidate in [true, false] {
                let mut pane = pane(20, 4, initial);
                let mut renderer = renderer();
                let mut glass = Glass::new(20, 4);
                let _ = glass.paint(&mut renderer, &pane, (0, 0), false);
                glass.scribble(scribble);
                if invalidate {
                    forget(&mut renderer);
                }
                pane.vt_write(edit);
                let _ = glass.paint(&mut renderer, &pane, (0, 0), false);
                let matches = glass.seen((0, 0), (20, 4)) == read_seen(&pane, (0, 0), (20, 4));
                assert_eq!(
                    matches,
                    invalidate,
                    "{:?} invalidate={invalidate}",
                    String::from_utf8_lossy(scribble)
                );
            }
        }
    }

    /// Touch every row without changing a cell, so every row is dirty and the
    /// diff alone would write nothing. First glyphs are read before writing:
    /// a separate walk consumes the dirty bits the renderer needs.
    fn touch_every_row(pane: &mut Pane) {
        let (_, rows) = pane_extent(pane);
        let firsts: Vec<String> = (0..rows)
            .map(|r| {
                read_seen(pane, (0, r), (1, 1))
                    .pop()
                    .map(|cell| cell.text)
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| " ".to_owned())
            })
            .collect();
        for (r, glyph) in (1u16..).zip(firsts) {
            pane.vt_write(format!("\x1b[{r};1H{glyph}").as_bytes());
        }
    }

    /// The front buffer is void after a screen clear (forced paint), a moved
    /// origin, or a grid resize: every cell / dirty row is rewritten whole.
    #[test]
    fn a_cleared_moved_or_resized_pane_is_rewritten_whole() {
        let mut t = pane(
            16,
            3,
            b"\x1b[44mblue\x1b[0m line\r\nsecond\r\nthird \xe4\xb8\x96!",
        );
        let mut r = renderer();
        let mut glass = Glass::new(16, 6);
        let _ = glass.paint(&mut r, &t, (0, 0), false);
        glass.scribble(b"\x1b[2J");
        let _ = glass.paint(&mut r, &t, (0, 0), true);
        assert_glass_shows(&glass, &t, (0, 0), "forced paint after ED2");

        glass.scribble(b"\x1b[2J");
        touch_every_row(&mut t);
        let _ = glass.paint(&mut r, &t, (0, 3), false);
        assert_glass_shows(&glass, &t, (0, 3), "moved origin");

        let mut t = pane(10, 3, b"one\r\ntwo\r\nthree");
        let mut r = renderer();
        let mut glass = Glass::new(16, 3);
        let _ = glass.paint(&mut r, &t, (0, 0), false);
        glass.scribble(b"\x1b[2J");
        t.resize(16, 3, 0, 0).expect("resize");
        touch_every_row(&mut t);
        let _ = glass.paint(&mut r, &t, (0, 0), false);
        assert_glass_shows(&glass, &t, (0, 0), "resized grid");
    }

    /// Selection changes paint the right inversion, forced and incremental.
    #[test]
    fn selection_changes_repaint_the_inverted_cells_correctly() {
        let mut t = pane(12, 2, b"selectme now\r\nsecond");
        let extent = pane_extent(&t);
        let mut r = renderer();
        let mut glass = Glass::new(12, 2);
        let _ = glass.paint(&mut r, &t, (0, 0), false);
        for force in [false, true] {
            r.set_selection(Some(sel((0, 0), (0, 3), false)));
            t.vt_write(b"\x1b[1;12H!");
            let _ = glass.paint(&mut r, &t, (0, 0), force);
            let seen = glass.seen((0, 0), extent);
            assert!(
                seen[..4].iter().all(|c| c.attrs.inverse) && !seen[4].attrs.inverse,
                "force={force}"
            );
            r.set_selection(None);
            t.vt_write(b"\x1b[1;12H?");
            let _ = glass.paint(&mut r, &t, (0, 0), force);
            assert_glass_shows(&glass, &t, (0, 0), "selection cleared");
        }
    }

    /// Wide glyphs appearing, vanishing, and being overwritten through their
    /// tails never leave a half-written pair.
    #[test]
    fn wide_glyph_edits_keep_the_glass_consistent() {
        let mut t = fresh(12, 1);
        let mut r = renderer();
        let mut glass = Glass::new(12, 1);
        for (i, step) in [
            "ab\u{4e16}cd\u{754c}",
            "\x1b[1;2H\u{4e16}",
            "\x1b[1;5Hx",
            "\x1b[1;4Hy",
            "\x1b[1;1H\x1b[P",
            "\x1b[1;3H\x1b[2@",
            "\x1b[1;6H\x1b[31m\u{754c}\x1b[0m",
            "\x1b[1;7H\x1b[1mZ\x1b[0m",
            "\x1b[1;1H\u{1f980}\u{1f980}e\u{301}",
        ]
        .into_iter()
        .enumerate()
        {
            t.vt_write(step.as_bytes());
            let _ = glass.paint(&mut r, &t, (0, 0), false);
            assert_glass_shows(&glass, &t, (0, 0), &format!("wide step {i}"));
        }
    }

    /// ECH of a wrapped wide glyph's continuation rewrites the spacer head on
    /// the previous row; the pooled state must copy it (property seed 11).
    #[test]
    fn erasing_a_wrapped_wide_glyph_clears_the_pooled_spacer_head() {
        let mut t = pane(9, 3, "\x1b[1;6H\x1b[7;31m\u{754C}#\u{754C}".as_bytes());
        let mut r = renderer();
        let mut glass = Glass::new(9, 3);
        let _ = glass.paint(&mut r, &t, (0, 0), true);
        t.vt_write(b"\x1b[2;1H\x1b[48;5;39m\x1b[1X");
        let _ = glass.paint(&mut r, &t, (0, 0), true);
        assert_glass_shows(&glass, &t, (0, 0), "pooled spacer head after ECH");
    }

    /// A deterministic generator: every failure replays from its seed.
    struct XorShift(u64);

    impl XorShift {
        fn below(&mut self, n: u64) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x % n
        }

        fn pick<'a>(&mut self, from: &[&'a str]) -> &'a str {
            from[usize::try_from(self.below(from.len() as u64)).unwrap_or(0)]
        }
    }

    const GLYPHS: [&str; 10] = [
        "a",
        "Z",
        "#",
        " ",
        ".",
        "\u{4e16}",
        "\u{754c}",
        "e\u{301}",
        "\u{1f980}",
        "\u{e9}",
    ];
    const PENS: [&str; 10] = [
        "\x1b[0m",
        "\x1b[1m",
        "\x1b[31m",
        "\x1b[38;5;120m",
        "\x1b[48;2;10;20;30m",
        "\x1b[7m",
        "\x1b[4m",
        "\x1b[3;9m",
        "\x1b[0;2;38;2;200;100;0m",
        "\x1b[4;58;5;196m",
    ];

    /// One random edit of the kinds real programs make.
    fn random_edit(rng: &mut XorShift, cols: u16, rows: u16) -> String {
        let row = rng.below(u64::from(rows)) + 1;
        let col = rng.below(u64::from(cols)) + 1;
        let n = rng.below(4) + 1;
        match rng.below(12) {
            0..=3 => {
                let mut edit = format!("\x1b[{row};{col}H{}", rng.pick(&PENS));
                for _ in 0..=rng.below(8) {
                    edit.push_str(rng.pick(&GLYPHS));
                }
                edit
            }
            4 => format!("\x1b[{row};{col}H\x1b[0m\x1b[{}K", rng.below(3)),
            5 => format!("\x1b[{row};{col}H\x1b[{n}@"),
            6 => format!("\x1b[{row};{col}H\x1b[{n}P"),
            7 => format!("\x1b[{rows};1H\r\n{}scrolled", rng.pick(&PENS)),
            8 => format!("\x1b[{row};{col}H\x1b[{}J", rng.below(3)),
            9 => ["\x1b[?1049h", "\x1b[?1049l"][usize::from(rng.below(2) == 1)].to_owned(),
            10 => format!(
                "\x1b[{row};{col}H\x1b[48;5;{}m\x1b[{n}X\x1b[0m",
                rng.below(256)
            ),
            _ => format!("\x1b[{row};{col}H\x1b[{n}C"),
        }
    }

    /// One paint target: its own pane (dirty bits live on the terminal, so
    /// lanes cannot share one), a renderer, and its glass. The legacy lane
    /// forgets its front before every paint: the whole-row dirty painter.
    struct Lane {
        pane: Pane,
        renderer: TerminalRenderer<'static>,
        glass: Glass,
        legacy: bool,
    }

    impl Lane {
        fn new(pane: (u16, u16), glass: (u16, u16), legacy: bool) -> Self {
            Self {
                pane: fresh(pane.0, pane.1),
                renderer: renderer(),
                glass: Glass::new(glass.0, glass.1),
                legacy,
            }
        }

        fn paint(&mut self, origin: (u16, u16), force: bool) -> Vec<u8> {
            if self.legacy {
                self.renderer.invalidate_front();
            }
            self.glass
                .paint(&mut self.renderer, &self.pane, origin, force)
        }
    }

    /// The property: any edit sequence painted through the diff, with modals,
    /// forgotten predictions, stray pens, and forced repaints after clears,
    /// leaves the glass matching the whole-row painter and, whenever that
    /// painter agrees with one, a full repaint of the final grid. (If
    /// libghostty changes a cell without dirtying its row no dirty-bit painter
    /// can see it; those steps re-sync and are counted.)
    #[test]
    fn random_edits_through_the_diff_painter_match_a_full_repaint() {
        let mut tally = Tally::default();
        for seed in 1..=48u64 {
            let mut rng = XorShift(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let dims = [(16u16, 5u16), (23, 7), (9, 3)][usize::try_from(seed % 3).unwrap_or(0)];
            let origin = if seed % 2 == 0 { (0, 0) } else { (3, 2) };
            let mut trial = Trial::new(dims, origin);
            for step in 0..80 {
                let force = trial.disturb(&mut rng);
                let edit = random_edit(&mut rng, dims.0, dims.1);
                trial.step(
                    &edit,
                    force,
                    &format!("seed {seed} step {step} after {edit:?}"),
                    &mut tally,
                );
            }
        }
        assert!(tally.checked > tally.undetected * 20, "{tally:?}");
    }

    #[derive(Debug, Default)]
    struct Tally {
        checked: usize,
        undetected: usize,
    }

    /// One seed: the diff lane, the whole-row lane it is held to, and a
    /// reference repainted in full every step.
    struct Trial {
        diff: Lane,
        legacy: Lane,
        reference: Lane,
        origin: (u16, u16),
        dims: (u16, u16),
    }

    impl Trial {
        fn new(dims: (u16, u16), origin: (u16, u16)) -> Self {
            let glass = (dims.0 + 5, dims.1 + 3);
            let mut trial = Self {
                diff: Lane::new(dims, glass, false),
                legacy: Lane::new(dims, glass, true),
                reference: Lane::new(dims, glass, false),
                origin,
                dims,
            };
            let _ = trial.diff.paint(origin, false);
            let _ = trial.legacy.paint(origin, false);
            trial
        }

        /// Maybe disturb the glass the way the driver's other writers do;
        /// returns whether the next paint must be forced.
        fn disturb(&mut self, rng: &mut XorShift) -> bool {
            let (cols, rows) = self.dims;
            let (ox, oy) = self.origin;
            let row = u16::try_from(rng.below(u64::from(rows))).unwrap_or(0);
            match rng.below(16) {
                0 => {
                    self.scribble_both(b"\x1b[2J", None);
                    true
                }
                1 => {
                    let at = format!("\x1b[{};{}H\x1b[7m[modal]\x1b[0m", oy + row + 1, ox + 1);
                    self.scribble_both(at.as_bytes(), Some((0, rows)));
                    true
                }
                2 => {
                    // A guess over one row, that row forgotten, then touched.
                    let col = u16::try_from(rng.below(u64::from(cols))).unwrap_or(0);
                    let guess = format!(
                        "\x1b[{};{}H\x1b[0m\x1b[4m?\x1b[0m",
                        oy + row + 1,
                        ox + col + 1
                    );
                    self.scribble_both(guess.as_bytes(), Some((row, row + 1)));
                    let touch = format!("\x1b[{};{cols}H{}", row + 1, rng.pick(&GLYPHS[..5]));
                    self.write_all(&touch);
                    false
                }
                3 | 4 => {
                    // A stray pen left behind; the next span must not inherit it.
                    self.scribble_both(b"\x1b[1;4;41m", None);
                    false
                }
                _ => false,
            }
        }

        fn scribble_both(&mut self, bytes: &[u8], forget: Option<(u16, u16)>) {
            for lane in [&mut self.diff, &mut self.legacy] {
                lane.glass.scribble(bytes);
                if let Some((start, end)) = forget {
                    lane.renderer.invalidate_front_rows(start..end);
                }
            }
        }

        fn write_all(&mut self, bytes: &str) {
            for lane in [&mut self.diff, &mut self.legacy, &mut self.reference] {
                lane.pane.vt_write(bytes.as_bytes());
            }
        }

        fn step(&mut self, edit: &str, force: bool, label: &str, tally: &mut Tally) {
            let (origin, region) = (self.origin, self.dims);
            self.write_all(edit);
            let _ = self.diff.paint(origin, force);
            let _ = self.legacy.paint(origin, force);
            // A fresh renderer reads every row; a pooled one trusts dirty bits.
            self.reference.renderer = renderer();
            self.reference.glass.scribble(b"\x1b[2J");
            let _ = self.reference.paint(origin, true);

            let seen = self.diff.glass.seen(origin, region);
            let old = self.legacy.glass.seen(origin, region);
            assert_same_screen(
                &format!("{label} (against the dirty-row painter)"),
                &old,
                &seen,
                region.0,
            );
            let full = self.reference.glass.seen(origin, region);
            if old != full {
                tally.undetected += 1;
                for lane in [&mut self.diff, &mut self.legacy] {
                    lane.renderer = renderer();
                    lane.glass.scribble(b"\x1b[2J");
                    let _ = lane.paint(origin, true);
                }
                return;
            }
            tally.checked += 1;
            assert_same_screen(
                &format!("{label} (against a full repaint)"),
                &full,
                &seen,
                region.0,
            );
            assert_glass_shows(
                &self.diff.glass,
                &self.diff.pane,
                origin,
                &format!("{label} (against the grid)"),
            );
        }
    }
}
