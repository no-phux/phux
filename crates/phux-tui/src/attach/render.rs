//! Render the client's local `libghostty_vt::Terminal` to the outer
//! terminal as VT escape sequences.
//!
//! Under ADR-0013 the client owns one `Terminal` per attached pane;
//! `RESOURCE_OUTPUT` byte frames are fed into it via `vt_write`. This
//! module reads the resulting structured state back out via
//! `RenderState` (per-row dirty tracking) and emits VT to stdout.
//!
//! The paint is a **cell diff over dirty rows** (`phux-esge`). libghostty
//! tracks dirt per row, so the rows to visit are the ones `RenderState`
//! reports dirty. Each visited row is compared against the pane's
//! `FrontBuffer` — what this renderer last wrote to the outer terminal at
//! those cells — and only the changed spans are emitted, each positioned with
//! a `CUP` (or bridged by rewriting a short unchanged gap when that is
//! cheaper). A row the front buffer does not know (first paint, a forced
//! full-frame paint, anything that invalidated it) is emitted whole, exactly
//! as the pre-diff dirty-row painter did. Within a span an SGR sequence is
//! emitted only when a cell's style differs from the one currently active on
//! the outer terminal, so a run of same-style cells costs one SGR plus the
//! glyphs. Per-row dirty bits are reset after the row is drawn so subsequent
//! renders skip clean rows.
//!
//! The front buffer is only as good as the claim that nothing else wrote
//! over the pane's cells since; see `FrontBuffer` for every writer that
//! invalidates it.
//!
//! Two frame-level contracts hold across everything below (`phux-l96p.2`):
//!
//! * **A painted frame is a transaction.** The dirty-row paint opens a DEC
//!   2026 synchronized-output block (`SyncOutput`) and closes it after the
//!   cursor is placed, so a terminal that composites mid-sequence never shows
//!   a half-repainted pane. The guard nests, so the frame-level block
//!   `paint::paint_full_frame` opens around several panes plus the chrome is
//!   not truncated by the per-pane one. A CLEAN frame opens no block and
//!   emits nothing at all.
//! * **The renderer never flushes.** ADR-0029 already makes
//!   `paint::end_of_frame_cursor` the one cursor authority per frame; it is
//!   the one FLUSH authority too, so a composite frame reaches the outer
//!   terminal in a single write-out rather than one per component painter.
//!
//! The cell loop itself allocates nothing: the grapheme cluster and the row's
//! bytes are read into buffers the pane's renderer owns for its whole life
//! (see `CellScratch`).
//!
//! No raw-mode or alt-screen toggling happens here; the [`super::driver`]
//! owns those transitions via an RAII guard so they survive panics and
//! early returns.
//!
//! See `research/2026-05-25-libghostty-renderstate.md` for the
//! renderer-side contract this module implements.

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
    /// A batched row read produced a column the renderer could not decode.
    ///
    /// libghostty encodes each cluster into the row's text buffer itself, so
    /// this means the buffer disagreed with the offsets describing it. It is
    /// surfaced rather than skipped because a skipped column would slide
    /// every later cell in the row one place left (see `walk_row_cells`).
    #[error("row cell at column {col} could not be read from the batched row")]
    UnreadableCell {
        /// The pane-local column whose cell could not be decoded.
        col: u16,
    },
}

/// Per-cell snapshot of one row, filled from libghostty's cell iterator.
///
/// Upstream no longer ships a batched `read_row` C crossing. This local
/// buffer keeps the paint loop's [`RowCells`] shape while walking cells with
/// the official iterator.
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

/// The copy-mode selection rectangle the renderer reverse-videos while painting.
///
/// Relocated to the shared contract module by
/// [ADR-0045](../../../../ADR/0045-client-side-copy-mode.md): the renderer and
/// the selection UX (`render::overlay::copy_mode`) must agree byte-for-byte on
/// what a selection covers — including block vs linear geometry
/// ([`SelectionRect::contains`]) — so the type and its geometry live in one
/// leaf ([`crate::render::overlay::selection`]) both consumers import. Carrying
/// the highlight through this per-cell render — the same one that emits the
/// pane's real styles — is what lets copy-mode leave the screen untouched
/// except for inverting the selected cells, instead of clearing and repainting
/// a separate overlay surface.
pub use crate::render::overlay::selection::SelectionRect;

/// One pane's published replica `Terminal` paired with the walk-identity
/// token the renderer must walk it under.
///
/// The session kernel REPLACES a pane's published `Terminal` when a replica
/// generation is republished, and the pooled render state discards its cache
/// exactly when this token changes — even at unchanged geometry (`phux-994s`).
/// Carrying the two halves as ONE value is what makes a mismatch
/// unrepresentable: the only production constructor is
/// `attach::pane_state::published_replica`, which reads both halves off the
/// same replica, so no paint path can pick up a terminal and a token that
/// disagree, and no walk-starting method needs two positional parameters that
/// must be kept in order.
///
/// Paths that only INSPECT the terminal (alt-screen, mouse tracking, title)
/// start no pooled walk and keep using `attach::pane_state::published_terminal`.
///
/// The fields are readable throughout `attach` — paint sites that also need
/// the terminal itself (mirror dimensions, alt-screen mode) read `terminal`
/// directly — and sealed outside it, so the only pairing a foreign crate can
/// make is the test-only `ReplicaWalk::for_test` constructor's.
#[derive(Debug, Clone, Copy)]
pub struct ReplicaWalk<'a, 'alloc, 'cb> {
    pub(super) terminal: &'a GhosttyTerminal<'alloc, 'cb>,
    pub(super) generation: TerminalGeneration,
}

#[cfg(any(test, feature = "testkit"))]
impl<'a, 'alloc, 'cb> ReplicaWalk<'a, 'alloc, 'cb> {
    /// Pair a test-owned terminal with a fixed token.
    ///
    /// Fixtures build one terminal per case and never replace it, so the
    /// token is a constant — "a generation that never changes" is exactly
    /// what the pool's rebuild rule reads it as. Tests that specifically
    /// exercise a REPLACEMENT build the pair directly with distinct tokens.
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
/// Owns a [`RenderPool`] — the libghostty render trio plus the
/// geometry-change and generation-change rebuilds — so the iterators are
/// reused across frames instead of reallocated each tick.
///
/// Every walk-starting method takes a [`ReplicaWalk`] rather than a bare
/// terminal, so the walked terminal always arrives with the generation token
/// the pool needs to notice a replica swap; a paint path that fetches a
/// terminal any other way does not type-check.
///
/// The pool owns allocation and geometry only. This renderer's dirty policy —
/// clear each row it drew, then clear the snapshot-level bit — stays here,
/// because the other walkers of a libghostty grid in this workspace
/// legitimately use different ones (ADR-0086).
#[derive(Debug)]
pub struct TerminalRenderer<'alloc> {
    /// Pooled render state + row/cell iterators. Rebuilt when the pane's
    /// grid changes dimensions (`phux-5pyx`) — the server's snapshot
    /// synthesizer has done this since that bead; this renderer inherited it
    /// by adopting the shared pool.
    pool: RenderPool<'alloc>,
    kitty_placements: libghostty_vt::kitty::graphics::PlacementIterator<'alloc>,
    /// Last-seen authoritative cursor position (outer-viewport coords:
    /// pane-local cursor plus [`Self::last_origin`]). Updated at the end of
    /// [`Self::render`]. The host-cursor restore paths read this. `None`
    /// while the cursor is hidden.
    last_cursor: Option<(u16, u16)>,
    /// Pane-local cursor `(row, col)` as of the most recent render — the
    /// libghostty viewport cursor BEFORE [`Self::last_origin`] is added.
    /// This is the authoritative anchor the predictive-echo layer
    /// (`phux-9gw.1`) re-syncs from: predictions are pane-local, so feeding
    /// the layer the outer-absolute [`Self::last_cursor`] instead would
    /// clamp a lower pane's cursor up into the wrong region (the mid-screen
    /// ghost echo after a split, phux-7ry0). `None` while the cursor is hidden.
    last_cursor_local: Option<(u16, u16)>,
    /// Outer-viewport origin `(x, y)` of the most recent `render_at` paint.
    /// The predictive-echo overlay adds this to each pane-local prediction
    /// so a pane offset from the viewport origin (any split that isn't the
    /// top-left leaf) paints its echo over the pane's real cells rather than
    /// at the viewport-absolute coordinate. Defaults to `(0, 0)`.
    last_origin: (u16, u16),
    /// Copy-mode selection to reverse-video on the next render, if any.
    ///
    /// Transient: the driver sets it on the focused pane's renderer just
    /// before a copy-mode repaint and clears it immediately after, so
    /// ordinary renders are unaffected and no other paint path needs to know
    /// copy-mode exists.
    selection: Option<SelectionRect>,
    /// Per-frame emission buffers, reused for the life of the pane
    /// (`phux-l96p.2`). See [`CellScratch`].
    scratch: CellScratch,
    /// What this pane last emitted to the outer terminal, cell by cell
    /// (`phux-esge`). A visited row whose front row is still known emits
    /// only the cells that differ from it. See [`FrontBuffer`].
    front: FrontBuffer,
}

/// The outer terminal's contents at this pane's cells, as this renderer last
/// wrote them (`phux-esge`).
///
/// libghostty tracks dirt per ROW, so a full-screen animation (cmatrix, a
/// progress spinner that redraws its line, a TUI that clears and repaints)
/// dirties every row every frame, and repainting each dirty row whole made
/// every such frame a full-screen rewrite: ~25x the bytes tmux sends for the
/// same program, all of it crossing the link when the client runs on the far
/// side of ssh. The front buffer turns the row dirt into cell dirt: a dirty
/// row is diffed against what the outer terminal already shows, and only the
/// changed spans are written.
///
/// # The invariant, and who can break it
///
/// A KNOWN front row is a claim that the outer terminal's cells at that row
/// hold exactly the recorded clusters in exactly the recorded pens. Anything
/// that writes those cells behind the renderer's back falsifies the claim, and
/// a diff against a false claim leaves stale cells on screen. So every such
/// writer invalidates, and an invalidated row falls back to the pre-front
/// behaviour exactly: when libghostty next reports it dirty it is repainted
/// whole. That is the safety argument for the whole design — wherever the
/// front buffer is unknown the renderer is byte-for-byte the old dirty-row
/// painter, so an over-eager invalidation costs bandwidth, never correctness.
///
/// The renderer invalidates on its own when:
///
/// * the paint is forced ([`TerminalRenderer::render_at_full`], the
///   full-frame path's repaint after its `ED2`);
/// * the paint's origin or clipped extent moves (a split, a zoom, a resize, a
///   relayout, a letterbox pad appearing or vanishing, a sidebar toggle);
/// * the replica generation changes (bootstrap, republish, reattach);
/// * the pane switches between the primary and alternate screen;
/// * the copy-mode selection changes;
/// * kitty graphics were replayed over the pane.
///
/// Writers outside the renderer invalidate explicitly through
/// [`TerminalRenderer::invalidate_front`] and
/// [`TerminalRenderer::invalidate_front_rows`]: the predictive-echo overlay
/// (the rows it painted); and, for the whole pane, the full-frame clear (at
/// the clear itself, not trusting each pane's forced paint to get that far),
/// the SIGWINCH clear, a stdout-writer resync, an incremental frame that
/// failed to ship, modal overlays and the copy-mode status strip.
///
/// A forced paint records nothing: it emits straight from the batched read
/// and leaves its rows unknown, so a failed forced frame cannot leave a false
/// claim behind. And a row becomes known only after its bytes reach the sink.
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
    /// Replica walk identity (`phux-994s`).
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

/// One row as the renderer last emitted it — or, while [`CellScratch::next`],
/// as it is about to be emitted.
///
/// Clusters are stored back to back in `text` and pens are deduplicated into
/// `pens` per style run, so recording a row costs two `Vec` appends per cell
/// and no allocation once the buffers have grown to the row's width.
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

/// The renderer's reusable per-frame emission buffers.
///
/// These live on the pane's [`TerminalRenderer`] rather than on the stack of
/// a paint call because the cell loop is the tightest loop in the product: a
/// full-dirty 200x60 frame walks 12 000 cells, and the pre-`phux-l96p.2` loop
/// bought a fresh `Vec<char>` from the allocator for every non-empty one —
/// ~10 000 malloc/free pairs before a single byte reached the terminal, over
/// half the frame's wall time. Hoisting the two buffers here makes the steady
/// state allocation-free: the first frame grows them to a row's width and
/// every later frame reuses that capacity.
#[derive(Debug, Default)]
struct CellScratch {
    /// One painted row's VT bytes, handed to the sink in a single
    /// `write_all` instead of one call per cell.
    row: Vec<u8>,
    /// The current cell's grapheme cluster, UTF-8 encoded in place by
    /// [`libghostty_vt::render::CellIteration::graphemes_utf8`] — the
    /// allocation-free counterpart to `CellIteration::graphemes`, which
    /// allocates a `Vec<char>` per call.
    ///
    /// Only the point reads (the predictive-echo reconcile) still go through
    /// this; the paint and projection loops read whole rows at once through
    /// [`Self::rowbuf`].
    cluster: String,
    /// One row's cells, read from libghostty in a SINGLE crossing
    /// (`phux-l96p.9`). See [`libghostty_vt::render::CellIteration::read_row`].
    ///
    /// The per-cell accessors are one C call each, so a cell that needs a
    /// cluster, a style and both resolved colours cost four or five
    /// crossings — over 60 000 on a full-dirty 200x60 frame, which is what
    /// bound the loop once it stopped allocating. This buffer receives the
    /// whole row instead: a compact record per cell, the row's styles
    /// deduplicated into runs, and every cluster's UTF-8 bytes back to back.
    /// It grows to the widest row the pane has seen and then never allocates
    /// again.
    rowbuf: RowBuf,
    /// The row being painted, recorded from [`Self::rowbuf`] before any byte
    /// is emitted (`phux-esge`). Emission reads from here — the diff against
    /// the pane's [`FrontBuffer`] row and the full-row paint alike — and the
    /// two are then swapped, so the old front row's buffers become the next
    /// row's scratch and the steady state stays allocation-free.
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
            selection: None,
            scratch: CellScratch::default(),
            front: FrontBuffer::default(),
        })
    }

    /// Set (or clear) the copy-mode selection to reverse-video on the next
    /// render. Transient — see [`SelectionRect`]; callers set it before a
    /// copy-mode repaint and clear it (`None`) immediately after.
    ///
    /// A change forgets the front buffer (`phux-esge`). The inversion is part
    /// of every recorded pen, so a diff would already see it; forgetting is
    /// the cheap insurance for a selection repaint that is not also forced.
    pub fn set_selection(&mut self, selection: Option<SelectionRect>) {
        if self.selection != selection {
            self.front.invalidate_all();
        }
        self.selection = selection;
    }

    /// Forget what this pane last emitted, so the next paint of each row
    /// rewrites it whole (`phux-esge`).
    ///
    /// Call this after writing anything over the pane's cells outside the
    /// renderer — a modal, a status strip, a cleared screen — that is not
    /// followed by a forced repaint. See the private `FrontBuffer` for the invariant
    /// and why forgetting is always safe.
    pub fn invalidate_front(&mut self) {
        self.front.invalidate_all();
    }

    /// Forget what this pane last emitted on the pane-local rows `rows`.
    ///
    /// The narrow form of [`Self::invalidate_front`], for a writer that knows
    /// which rows it touched: the predictive-echo overlay paints its guesses
    /// straight over the cursor row, and only that row needs rewriting when
    /// the authoritative echo lands.
    pub fn invalidate_front_rows(&mut self, rows: std::ops::Range<u16>) {
        self.front.invalidate_rows(rows);
    }

    /// Cursor (row, col) as of the most recent [`Self::render`] call.
    /// Returns `None` if the cursor was hidden or no render has yet
    /// occurred. The predictive-echo layer reads this to re-anchor its
    /// cursor estimate after a server frame.
    #[must_use]
    pub const fn last_cursor(&self) -> Option<(u16, u16)> {
        self.last_cursor
    }

    /// Pane-local cursor `(row, col)` as of the most recent render — the
    /// cursor BEFORE the pane's outer-viewport origin is added. The
    /// predictive-echo layer re-anchors from this (predictions are
    /// pane-local); see [`Self::last_cursor_local`]'s field docs for why
    /// feeding it [`Self::last_cursor`] strands the echo mid-screen
    /// (phux-7ry0). `None` if the cursor was hidden or no render has occurred.
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

    /// Read the base grapheme of the cell at `(row, col)` in `terminal`.
    ///
    /// Returns `Some(ch)` if the cell has a base grapheme, `None` if it
    /// is blank (no grapheme, wide-tail placeholder, or out of range).
    /// A `' '` (space) cell yields `Some(' ')` so callers can distinguish
    /// "explicitly blanked" from "out of range" — the predict-layer
    /// reconcile treats `' '` and `None` as the same "blank" verdict.
    ///
    /// This takes a fresh snapshot of `terminal` — it must not be called
    /// concurrently with [`Self::render`] (the `&mut self` receiver
    /// guarantees that statically). Used by the per-cell reconcile in
    /// the predict layer (phux-9gw.1.1) to confirm or contradict
    /// predictions against the authoritative cell grid.
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

    /// Read the full grapheme cluster of the cell at `(row, col)` as a
    /// `String`, joining every scalar in the cell.
    ///
    /// Returns `Some(s)` if the cell has any grapheme (`s` may be a
    /// multi-codepoint cluster — a flag emoji, a ZWJ family sequence, or
    /// a base plus combining marks), `None` if the cell is blank
    /// (no grapheme, wide-tail placeholder, or out of range). Unlike
    /// [`Self::read_grapheme_at`], which truncates to the base scalar,
    /// this preserves the whole cluster so the predict-layer reconcile
    /// (phux-9gw.1.6) can compare it against a predicted multi-codepoint
    /// cluster.
    ///
    /// Same snapshot semantics as [`Self::read_grapheme_at`]: takes a
    /// fresh snapshot of `terminal`; the `&mut self` receiver guarantees
    /// it is not called concurrently with [`Self::render`].
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

    /// Shared cell-grapheme lookup backing [`Self::read_grapheme_at`] and
    /// [`Self::read_grapheme_string_at`].
    ///
    /// Loads the cell's grapheme cluster into [`CellScratch::cluster`] and
    /// returns whether `(row, col)` was in range; an in-range blank cell
    /// leaves the buffer empty. Reading through the shared scratch is what
    /// makes the predictive-echo reconcile allocation-free — it runs once per
    /// pending prediction on every server frame, and used to buy (and throw
    /// away) a `Vec<char>` on each call.
    ///
    /// Row seeking is still linear: libghostty's `RowIterator` exposes only
    /// `next()`, with no counterpart to `CellIterator::select`, so reaching
    /// row *n* costs *n* iterator steps. `select` on the *cell* iterator does
    /// make the column O(1). A `row_iterator_select(y)` in libghostty-vt
    /// would close the remaining gap.
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

    /// Render dirty rows of `terminal` to `out`. Returns the dirty
    /// classification observed; the caller can use it to decide whether
    /// to flush.
    ///
    /// After this returns, every dirty bit (global + per-row) is reset,
    /// per the libghostty contract documented in
    /// `research/2026-05-25-libghostty-renderstate.md` §3.
    ///
    /// This is the single-pane entry point — equivalent to
    /// [`Self::render_at`] with origin `(0, 0)`. Multi-pane callers
    /// (see `attach::multi_pane`, phux-4li.4) use [`Self::render_at`] to
    /// position the terminal's content inside a sub-rectangle of the
    /// outer viewport.
    pub fn render(
        &mut self,
        walk: ReplicaWalk<'_, 'alloc, '_>,
        out: &mut impl Write,
    ) -> Result<Dirty, RenderError> {
        // No pane rect to clip against; the terminal's own grid defines the
        // extent (`u16::MAX` clamps to the grid size on both axes).
        self.render_at(walk, out, (0, 0), (u16::MAX, u16::MAX))
    }

    /// Render `terminal` into the outer viewport with its top-left at
    /// `origin = (x, y)` in outer-viewport cell coordinates, clipped to
    /// `clip = (cols, rows)` of the pane's render rect.
    ///
    /// The painted extent is `min(terminal grid, clip)` on each axis. The
    /// mirror's libghostty grid size is server-authoritative and may
    /// transiently exceed the client's layout rect during a resize
    /// handshake; `clip` confines the paint to the rect so a wider mirror
    /// never spills past the rect (into a divider or a neighbour pane) and
    /// a narrower mirror never paints beyond its own grid. Every row CUP is
    /// shifted by `origin.1` and every column by `origin.0`; the final
    /// cursor placement (cached in [`Self::last_cursor`]) is reported in
    /// **outer-viewport** coordinates, not pane-local — that's what the
    /// predictive-echo overlay needs for direct stdout writes.
    ///
    /// Multi-pane drivers call this once per visible pane; dividers are
    /// painted separately via
    /// [`crate::render::chrome::dividers::render_dividers`].
    pub fn render_at(
        &mut self,
        walk: ReplicaWalk<'_, 'alloc, '_>,
        out: &mut impl Write,
        origin: (u16, u16),
        clip: (u16, u16),
    ) -> Result<Dirty, RenderError> {
        self.render_at_inner(walk, out, origin, clip, false)
    }

    /// Like [`Self::render_at`] but unconditionally repaints every row,
    /// ignoring the incremental dirty tracking.
    ///
    /// Required by the full-frame paint path: that path emits `ED2`
    /// (clear screen) before re-rendering each pane, which wipes the
    /// terminal but leaves libghostty's per-row dirty bits clean for a
    /// pane whose *content* didn't change (e.g. the surviving pane after
    /// a split or resize). A plain `render_at` would see `Dirty::Clean`,
    /// early-return, and leave that pane blank on the freshly-cleared
    /// screen. Forcing a full redraw repaints it from the grid. See the
    /// split-leaves-original-pane-blank bug.
    pub fn render_at_full(
        &mut self,
        walk: ReplicaWalk<'_, 'alloc, '_>,
        out: &mut impl Write,
        origin: (u16, u16),
        clip: (u16, u16),
    ) -> Result<Dirty, RenderError> {
        self.render_at_inner(walk, out, origin, clip, true)
    }

    /// Project this pane's grid into a region of a dense [`RenderedFrame`]
    /// instead of emitting VT (`phux-l5xa`).
    ///
    /// Walks the **same** `RenderState` snapshot + `RowIterator` /
    /// `CellIterator` as [`Self::render_at`], but writes each cell's
    /// grapheme + resolved style into `frame` at `(row + origin.1, col +
    /// origin.0)`, clipped to `clip = (cols, rows)` of the pane's render
    /// rect exactly as the VT path clips. This is the structured-cells
    /// counterpart to the byte renderer: no VT, no re-parse, so the
    /// composited view can be introspected with no external emulator.
    ///
    /// Wide glyphs are mirrored faithfully: the base cell carries the
    /// cluster, and its `SpacerTail` column is left as the empty grapheme
    /// (`""`) so a consumer reconstructs exact widths (see [`RenderedCell`]).
    /// Copy-mode selection inversion is intentionally *not* applied — this
    /// is a side-effect-free introspection path, not the live overlay.
    ///
    /// Returns the pane's cursor in **frame-absolute** coordinates (pane
    /// viewport cursor shifted by `origin`), or `None` when the cursor is
    /// off-viewport or clipped away. The compositor elects which pane's
    /// cursor becomes the frame cursor.
    ///
    /// [`RenderedCell`]: phux_core::screen::RenderedCell
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

    /// Render `terminal` into the outer-viewport rect at `rect_origin =
    /// (x, y)` spanning `rect_clip = (cols, rows)`, **letterboxed**: when the
    /// server-authoritative mirror grid (`mirror = (cols, rows)`) is smaller
    /// than the rect on an axis, centre the content within the rect and blank
    /// the surrounding margin bars rather than painting at the rect origin
    /// (which would pin an undersized mirror to the top-left and leave stale
    /// cells along the bottom/right of the rect).
    ///
    /// When the mirror is >= the rect on an axis, this degrades to the
    /// existing [`Self::render_at`] clamp on that axis (no pad, clip to the
    /// rect) — a wider/taller mirror is confined to the rect exactly as
    /// before (phux-wurs). The mirror-equals-rect case is byte-identical to
    /// [`Self::render_at_full`]: zero pad ⇒ no margin bars ⇒ the same core
    /// paint at the same origin.
    ///
    /// `force_full` forwards to the core paint (the full-frame path forces a
    /// redraw after its `ED2`). The centring math (floor split, the extra pad
    /// cell on the bottom/right of an odd gap) lives in the private
    /// `letterbox_rect` helper.
    ///
    /// This is the single-view letterbox of ADR-0027 decision points 1-2:
    /// one Terminal rendered into one slot under the nk07/xjgs geometry
    /// policy. True multi-leaf mirroring (the same Terminal in N slots) is a
    /// layout-model change and is out of scope here.
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
        // Bars and content are ONE transaction when there are bars: otherwise
        // an undersized mirror shows its blanked margins a beat before the
        // content lands inside them. Opened only in the pad case so the
        // clamp path (and every clean frame through it) stays byte-identical;
        // the guard nests with the one `render_at_inner` opens.
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
        // Every incremental paint is a transaction, not just the full-frame
        // one: a dirty-row repaint moves the cursor, rewrites rows, and
        // re-places the cursor, and a terminal that composites mid-sequence
        // shows the intermediate states as tearing. The guard nests, so this
        // costs nothing extra when the frame-level paint already opened a
        // block around several panes plus the chrome.
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
            self.selection,
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

/// Replay the pane's kitty graphics over its cells at `at = (origin, clip)`,
/// returning whether any placement was emitted.
///
/// A replay places images over the pane's cells. Text under a placement
/// survives it in every terminal we know of, but nothing promises that, so a
/// replay that emitted anything forgets the front buffer (`phux-esge`). A
/// free function rather than a method because the paint that calls it still
/// holds the pooled render state's borrow of the renderer.
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

/// The painted extent as `(cols, rows)`.
///
/// Clip to the render rect: a server-authoritative mirror may be larger than
/// the client's layout rect during a resize handshake; painting past the rect
/// would spill into a divider or neighbour pane. `min` also keeps a smaller
/// mirror within its own grid.
fn clipped_extent(
    snapshot: &Snapshot<'_, '_>,
    clip: (u16, u16),
) -> Result<(u16, u16), RenderError> {
    let (clip_cols, clip_rows) = clip;
    let rows_total = snapshot.rows()?.min(clip_rows);
    let cols_total = snapshot.cols()?.min(clip_cols);
    Ok((cols_total, rows_total))
}

/// Walk rows, painting each one that needs redrawing.
///
/// Under `Dirty::Full` paint every row; under `Dirty::Partial` skip rows whose
/// per-row dirty bit is clear. Which rows are VISITED is unchanged by the
/// front buffer (`phux-esge`); what a visited row EMITS is decided against it
/// in [`paint_row`].
///
/// `record` is `false` for a forced paint: every row is then emitted straight
/// from its batched read and left unknown, and the next incremental paint of
/// the row records it (see [`emit_and_record`]).
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
    selection: Option<SelectionRect>,
    record: bool,
) -> Result<(), RenderError> {
    let (cols_total, rows_total) = extent;
    // The outer pen is unknown at the start of every pane paint: chrome,
    // another pane, or an overlay may have written anything since this pane
    // last emitted. From here on nothing else writes until the paint ends, so
    // the pen each span leaves carries to the next, across jumps and rows.
    let mut pass = PaintPass {
        selection,
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

/// What one pane paint threads across its rows: the copy-mode selection,
/// whether rows are recorded into the front buffer, and the outer terminal's
/// pen as the paint has left it so far.
#[derive(Debug)]
struct PaintPass {
    selection: Option<SelectionRect>,
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

/// Paint one row, then clear its dirty bit.
///
/// The emission is composed into [`CellScratch::row`] and handed to `out` in
/// a single `write_all`, and a known row that did not change at all emits
/// nothing. See [`emit_and_record`] for what is emitted.
fn paint_row<'alloc>(
    out: &mut impl Write,
    scratch: &mut CellScratch,
    front_row: &mut FrontRow,
    row: &RowIteration<'alloc, '_>,
    cells: &mut CellIterator<'alloc>,
    at: RowAt,
    pass: &mut PaintPass,
) -> Result<(), RenderError> {
    let CellScratch {
        row: buf,
        rowbuf,
        next,
        ..
    } = scratch;
    buf.clear();

    // One crossing into libghostty for the whole row. Every cell consumes
    // one column, including a wide glyph's spacer tail (which emits nothing),
    // so walking the columns in step with the cells clips at `cols_total`
    // exactly as the old per-cell walk did.
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

/// Emit one row into `buf` and make its recording the front row.
///
/// If the pane's front row is KNOWN, the row is recorded into `next` and only
/// the cells that differ are emitted ([`emit_row_diff`]). Otherwise the whole
/// row is emitted WHILE it is recorded, in the one walk ([`begin_full_row`]
/// then [`record_row`]), byte-identical to the pre-front-buffer paint. The
/// old front row's buffers are then swapped into `next`, to be reused as
/// scratch by the next row painted.
///
/// Returns whether `front_row` now holds a recording of the row. It is left
/// UNKNOWN either way: [`paint_row`] marks it known only once the emitted
/// bytes have reached the sink.
fn emit_and_record(
    buf: &mut Vec<u8>,
    front_row: &mut FrontRow,
    next: &mut FrontRow,
    batch: &RowCells<'_>,
    at: RowAt,
    pass: &mut PaintPass,
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
        record_row(next, batch, at, pass.selection, None)?;
        emit_row_diff(buf, front_row, next, at, &mut pass.pen)?;
    } else {
        begin_full_row(buf, at, &mut pass.pen)?;
        record_row(next, batch, at, pass.selection, Some((buf, &mut pass.pen)))?;
    }
    std::mem::swap(front_row, next);
    front_row.known = false;
    Ok(true)
}

/// Emit a whole row straight from its batched read, recording nothing — the
/// forced-paint path, and the pre-`phux-esge` cell loop exactly: a cell whose
/// pen identity ([`PenKey`]) matches its predecessor's skips straight to its
/// glyphs, and a spacer tail writes nothing.
///
/// This loop and [`record_row`]'s emitting mode must stay byte-identical: a
/// row painted forced and the same row painted as an unknown incremental row
/// must reach the terminal as the same bytes. The byte-identity gate
/// (`batched_row_read_emits_the_same_bytes_as_the_per_cell_walk` and its two
/// siblings) runs both paths against the per-cell walk and against each
/// other, so a change to one that is not made to the other fails there.
fn emit_unrecorded_row(
    buf: &mut Vec<u8>,
    batch: &RowCells<'_>,
    at: RowAt,
    pass: &mut PaintPass,
) -> Result<(), RenderError> {
    let mut prev: Option<PenKey> = None;
    walk_row_cells(batch, at.cols_total, |col, cell| {
        if matches!(cell.wide, CellWide::SpacerTail) {
            return Ok(());
        }
        let inverted = selection_covers_cell(pass.selection, at.row_index, col, cell.wide);
        let key = PenKey {
            style_index: cell.style_index,
            fg: cell.fg,
            bg: cell.bg,
            inverted,
        };
        if prev != Some(key) {
            let mut style = batch.style(cell.style_index)?;
            style.inverse ^= inverted;
            emit_sgr_if_changed(buf, &mut pass.pen.emitted, style, cell.fg, cell.bg);
            prev = Some(key);
        }
        emit_cell_glyphs(buf, cell.text.as_bytes());
        Ok(())
    })
}

/// Record one row's cells into `next`, clipped to `at.cols_total`.
///
/// Each cell's pen is resolved exactly as the emitter will send it — the
/// copy-mode inversion applied — so a recorded row is a faithful statement of
/// what emitting it puts on screen. A cell whose pen IDENTITY ([`PenKey`]:
/// this row's style-run index plus the resolved colours and the selection
/// flip) matches its predecessor's is by construction another member of the
/// same run, so it shares the run's pen entry and the 72-byte [`Style`] is
/// materialised once per run rather than once per cell.
///
/// The per-cell walk this replaced also consulted the ROW's `styled` flag to
/// skip the style and foreground reads wholesale on an unstyled row. It is
/// dead weight now: the batched read resolves an unstyled row to a single
/// default style-table entry anyway. The byte-identity gate
/// (`batched_row_read_emits_the_same_bytes_as_the_per_cell_walk`) holds this
/// path to the old one, flag and all, and to [`emit_unrecorded_row`], its
/// forced-paint twin.
///
/// With `emit`, the row is also written to the sink as it is recorded — the
/// whole-row paint, fused into the same walk so an unknown row costs one pass
/// rather than a recording pass plus an emitting one. The caller has already
/// written the row prologue ([`begin_full_row`]), so the pen is known.
fn record_row(
    next: &mut FrontRow,
    batch: &RowCells<'_>,
    at: RowAt,
    selection: Option<SelectionRect>,
    mut emit: Option<(&mut Vec<u8>, &mut SpanPen)>,
) -> Result<(), RenderError> {
    next.clear();
    let mut prev_pen: Option<PenKey> = None;
    walk_row_cells(batch, at.cols_total, |col, cell| {
        let run = record_pen(
            next,
            batch,
            cell,
            (at.row_index, col),
            selection,
            &mut prev_pen,
        )?;
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
/// a new style run (the only point at which the emitted SGR can change).
///
/// A spacer tail emits nothing, so its style never reaches the terminal: it
/// borrows the base's pen (a row that somehow opens on a tail records its
/// own) and leaves the run identity untouched.
fn record_pen(
    next: &mut FrontRow,
    batch: &RowCells<'_>,
    cell: &RowCell<'_>,
    at: (u16, u16),
    selection: Option<SelectionRect>,
    prev: &mut Option<PenKey>,
) -> Result<Option<EmittedStyle>, RenderError> {
    let tail = matches!(cell.wide, CellWide::SpacerTail);
    if tail && !next.pens.is_empty() {
        return Ok(None);
    }
    let inverted = !tail && selection_covers_cell(selection, at.0, at.1, cell.wide);
    let key = PenKey {
        style_index: cell.style_index,
        fg: cell.fg,
        bg: cell.bg,
        inverted,
    };
    let opens_run = *prev != Some(key) || next.pens.is_empty();
    if !tail {
        *prev = Some(key);
    }
    if !opens_run {
        return Ok(None);
    }
    let mut style = batch.style(cell.style_index)?;
    style.inverse ^= inverted;
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

/// Open a whole-row paint: position the cursor at the row's start and reset
/// the pen. [`record_row`] then writes every cell.
///
/// Together they are the pre-`phux-esge` row paint, byte for byte — the path
/// an unknown front row (first paint, forced paint, any invalidation) takes,
/// and the one `batched_row_read_emits_the_same_bytes_as_the_per_cell_walk`
/// holds to the per-cell walk it descends from.
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
/// spans.
///
/// A span is a maximal run of changed columns, widened so no wide glyph is
/// half-written: it starts on the base of any wide glyph (old or new) whose
/// spacer tail it would otherwise start on, and it runs on through any tail
/// that follows its last cell. Between two spans the cursor either JUMPS
/// (`CUP`) or the unchanged cells between them are simply rewritten, whichever
/// is fewer bytes ([`bridge_gap`]).
///
/// A jump does not touch the outer pen — a `CUP` changes no SGR state, and
/// nothing else writes between the spans of one pane paint — so `pen`, the
/// pen the paint has left so far, carries across it and the next span emits
/// only the SGR it actually needs. The pen is unknown only at the start of a
/// pane paint, where the first cell emits a complete SGR (a reset included).
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

/// The first column of the span whose first changed column is `changed`.
///
/// Backs up onto the base of a wide glyph whose spacer tail `changed` is — in
/// either row. Writing into a tail on the outer terminal erases the glyph it
/// belongs to, and a new tail can only be drawn by writing its base.
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

/// Move the outer cursor from `from` to `to` (pane-local columns, same row)
/// by rewriting the unchanged cells between them, if that costs no more
/// bytes than the `CUP` a jump would. Returns whether it did; on `false`
/// nothing was written and `pen` is untouched.
///
/// The rewrite is tried and rolled back rather than estimated, because what
/// it costs depends on the pens in the gap and on the pen already active.
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

/// Pen comparisons between two recorded rows, memoised on the last pair.
///
/// Pens are compared by value — a run index means nothing across rows — and a
/// row's cells come in runs, so consecutive cells almost always ask about the
/// same `(front pen, next pen)` pair. Remembering the last answer makes the
/// ~100-byte comparison a per-run cost instead of a per-cell one.
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

/// The per-column cell source a row walk reads.
///
/// [`RowCells`] — one row, read from libghostty in a single crossing — is the
/// only production implementation. The trait exists so that
/// [`walk_row_cells`]'s refusal to SKIP a column can be tested: a `None` from
/// `RowCells::get` needs a text slice that does not land on a UTF-8 boundary,
/// which no live terminal produces and no test can stage against the real
/// type.
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
/// The obvious spelling — `(0..cols_total).zip(source.iter())` — is a trap.
/// [`RowCells::iter`] is a `filter_map` over `get`, so ONE unreadable cell
/// mid-row would silently vanish from the iterator and pair every later cell
/// with the column to its left: the row paints a column short with its whole
/// tail shifted, and nothing reports it. The per-cell walk this batched read
/// replaced could not do that, because it advanced the column and the cell
/// together.
///
/// So the columns are indexed explicitly, and a column the source cannot
/// produce is a [`RenderError::UnreadableCell`] rather than a hole. The walk
/// still stops at the shorter of the row and the clip, which is the ordinary
/// end-of-row case and not an error.
///
/// The `let ... else` rather than `cell_at(..).ok_or(..)?` is load-bearing at
/// this size. `ok_or` builds a `Result<RowCell, RenderError>` for EVERY cell,
/// and `RenderError` carries an `io::Error`, so the happy path pays a wide
/// move plus a discriminant test 12 000 times a frame; measured on
/// `render_frame`, spelling it that way cost ~20% of the full-dirty frame.
/// The `else` arm keeps the error construction on the cold path where it
/// belongs.
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
/// cursor style, and clear the global dirty bit.
///
/// It does NOT flush. A composite frame is one pane paint (or several) plus
/// dividers, the sidebar strip and the status bar, and every one of those
/// used to flush on its way out — several syscalls per frame, each one a
/// chance for the outer terminal to composite a half-built screen. ADR-0029
/// already names `paint::end_of_frame_cursor` the single cursor authority per
/// frame; it is now the single FLUSH authority too, and the renderer leaves
/// its bytes in the sink for it.
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

/// The grapheme a cell contributes to a [`RenderedFrame`].
///
/// A blank cell becomes a single space; a wide glyph's spacer tail becomes
/// the empty string (the base cell already carries the cluster, and leaving
/// the tail empty is what lets a consumer reconstruct exact widths);
/// everything else is the cell's own cluster, borrowed straight out of the
/// row buffer.
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

/// Project a libghostty cell's `(Style, resolved fg, resolved bg)` into a
/// plain-data [`CellStyle`] for the rendered-frame introspection path
/// (`phux-l5xa`).
///
/// This mirrors the server synthesizer's `collect_cell` (`phux-8yl`) — the
/// two can't share code because that projection lives in `phux-server` and
/// this walk runs client-side, but they must agree cell-for-cell so a
/// `--rendered` frame and a `--cells` snapshot describe the same glyph
/// identically. A third copy walks the same projection in `phux-record`'s
/// `replay::project_cell`, for the same reason.
///
/// `crates/phux/tests/conformance/cell_projection_conformance.rs` holds all three to one
/// corpus of VT sequences and fails on any divergence (`phux-h5hj.2`). Change
/// this function and expect that test to name the other two.
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
    /// How many [`SyncOutput`] guards are open on this thread.
    ///
    /// DEC 2026 is a MODE, not a counter: a nested `?2026l` ends the outer
    /// terminal's transaction early, so a naive guard inside `render_at`
    /// would break the frame-level block `paint_full_frame` opens around
    /// several panes plus the chrome — the atomicity it exists for. Counting
    /// the depth here makes the guard nestable: only the outermost one emits
    /// the mode bytes.
    ///
    /// Thread-local rather than a field because the two nesting levels are
    /// opened by different layers (the composite paint and the per-pane
    /// renderer) that never see each other's state, and the client's paint
    /// path is a single tokio current-thread runtime — every emit reaching
    /// stdout comes from one thread. A guard on another thread simply nests
    /// against its own counter, which is the correct answer for a separate
    /// sink.
    static SYNC_OUTPUT_DEPTH: core::cell::Cell<u32> = const { core::cell::Cell::new(0) };
}

/// An open DEC 2026 synchronized-output block.
///
/// Nestable: [`SyncOutput::begin`] emits `CSI ? 2026 h` only when no block is
/// already open on this thread, and [`SyncOutput::end`] emits `CSI ? 2026 l`
/// only when it closes the outermost one. That is what lets the frame-level
/// block and the per-pane block land as independent changes without one
/// truncating the other.
///
/// The counter is released in `Drop`, so an early return or a panic between
/// `begin` and `end` cannot strand the depth (it can still leave the outer
/// terminal inside a transaction — the driver's `SYNC_OUTPUT_WATCHDOG` is the
/// backstop for that, exactly as it is for an application that omits its own
/// `?2026l`). Closing is explicit rather than `Drop`-driven because the sink
/// to write to is not something a guard can hold across the borrow of `out`
/// the paint needs.
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

/// Convenience for callers that just want the cursor reset.
///
/// Used by [`super::driver::RawModeGuard`]'s `Drop` to ensure the outer
/// terminal isn't left with our hidden cursor or random SGR state. Kept
/// fallible because the underlying `Write` might be a closed stdout.
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

/// The centred placement of a mirror within a render rect, plus the margin
/// bars to blank around it (ADR-0027 single-view letterbox, phux-7ubw).
///
/// All coordinates are outer-viewport cells. `inner_origin`/`inner_clip` are
/// what the core paint ([`TerminalRenderer::render_at_inner`]) consumes:
/// the content's centred top-left and its clamped extent. The four `margin_*`
/// fields are the surrounding gap the mirror does not cover and that
/// [`emit_letterbox_margins`] blanks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Letterbox {
    /// Centred top-left of the mirror content (rect origin + pad).
    inner_origin: (u16, u16),
    /// Painted extent `min(mirror, rect)` on each axis — the clamp the
    /// existing `render_at` already applies, so a mirror >= the rect is
    /// confined to the rect (phux-wurs) with no pad.
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
    /// Whether any margin bar exists at all.
    ///
    /// `false` is the mirror-fills-or-exceeds-the-rect clamp case, where
    /// [`emit_letterbox_margins`] emits nothing and the paint stays
    /// byte-identical to [`TerminalRenderer::render_at`].
    const fn has_pad(self) -> bool {
        self.margin_left > 0
            || self.margin_right > 0
            || self.margin_top > 0
            || self.margin_bottom > 0
    }
}

/// Centre a mirror of `mirror = (cols, rows)` within the render rect at
/// `rect_origin = (x, y)` spanning `rect_clip = (cols, rows)`, returning the
/// centred [`Letterbox`].
///
/// Per axis: when the mirror is smaller than the rect, the gap
/// `rect - mirror` is split with `pad = gap / 2` on the leading edge
/// (left/top) and the remainder `gap - pad` on the trailing edge
/// (right/bottom) — a floor split that puts the extra cell of an odd gap on
/// the bottom/right. When the mirror is `>=` the rect, the pad is `0` and the
/// clip clamps to the rect (the existing `render_at` behaviour, phux-wurs).
///
/// Pure — no I/O, no `terminal` access — so it is unit-testable in isolation.
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

/// Blank the four margin bars of a [`Letterbox`] so an undersized mirror's
/// surrounding rect cells are cleared before the centred content paints.
///
/// Each bar is a sequence of `CUP` + an SGR-reset blank run: top and bottom
/// bars span the full rect width; left and right bars span only the interior
/// rows (between the top and bottom bars) so the corners are written once, by
/// the top/bottom bars. A `Letterbox` with no pad (the mirror fills or
/// exceeds the rect) emits nothing, keeping the clamp path byte-identical to
/// [`TerminalRenderer::render_at`].
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

/// The cell style currently active on the outer terminal, as a comparable
/// key for run coalescing. `fg`/`bg` are tracked alongside `Style` because
/// the renderer sources the resolved RGB foreground/background from the
/// per-cell [`libghostty_vt::render::CellIterator`] (`cell.fg_color()`/`cell.bg_color()`)
/// rather than from `Style`'s palette-indexed color fields.
type EmittedStyle = (Style, Option<RgbColor>, Option<RgbColor>);

/// A cell's pen identity within ONE row read — everything that decides the
/// SGR it would emit, in 20 bytes rather than [`EmittedStyle`]'s ~100.
///
/// `style_index` is a style-RUN index, not a style identity: the batched read
/// appends a style-table entry only where a cell's style differs from the
/// preceding cell's, so the index rises monotonically across the row and a
/// style the row RETURNS to is given a fresh index. That makes equal indices
/// a strictly stronger statement than "equal style" — two cells sharing one
/// are adjacent members of a single run — so the fast path is sound: it can
/// only skip work [`emit_sgr_if_changed`] would have found redundant, never
/// change what reaches the terminal. A DIFFERENT index proves nothing, which
/// is why the miss path still does the real `(Style, fg, bg)` comparison.
///
/// The indices belong to one `read_row` and are never reused across rows, so
/// [`record_row`] starts every row with no previous key and no key from one
/// row is ever compared against a key from another. Across rows — and across
/// frames, in the front buffer — pens are compared by VALUE ([`PenMemo`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PenKey {
    /// This cell's run within the row's style table — valid only within the
    /// `read_row` that produced it.
    style_index: u32,
    /// The cell's resolved foreground.
    fg: Option<RgbColor>,
    /// The cell's resolved background.
    bg: Option<RgbColor>,
    /// Whether a copy-mode selection flips this cell's inverse attribute.
    inverted: bool,
}

/// Whether a `(style, fg, bg)` triple renders as the terminal default — no
/// attributes and no explicit colors. Such a run needs only a plain `\x1b[0m`
/// reset (which `emit_sgr_set` already produces), and at row start the active
/// state is already default, so it emits nothing at all.
fn is_default_render(style: &Style, fg: Option<RgbColor>, bg: Option<RgbColor>) -> bool {
    fg.is_none() && bg.is_none() && *style == Style::default()
}

/// Emit an SGR sequence only when `(style, fg, bg)` differs from the style
/// currently active on the outer terminal (`emitted`).
///
/// `emitted` is the per-row coalescing state: `None` means the default style
/// is active (true at row start, just after the row-leading `\x1b[0m`), and
/// `Some(key)` means `key` was the last sequence written on this row. A run
/// of cells sharing a style therefore emits a single SGR sequence; only a
/// real style change writes another. The bytes for an isolated style change
/// are identical to the pre-coalescing per-cell emission (a `\x1b[0m` reset
/// followed by the attribute/color set), so the rendered screen is unchanged.
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

fn selection_covers_cell(
    selection: Option<SelectionRect>,
    row: u16,
    col: u16,
    wide: CellWide,
) -> bool {
    selection.is_some_and(|selection| {
        selection.contains(row, col)
            || matches!(wide, CellWide::Wide) && selection.contains(row, col.saturating_add(1))
    })
}

/// Write a full `\x1b[0m` reset followed by the SGR set for `(style, fg, bg)`.
///
/// The leading reset clears any prior attributes so the resulting outer-
/// terminal state is exactly `(style, fg, bg)` regardless of what preceded
/// it; coalescing in [`emit_sgr_if_changed`] decides *when* this runs.
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

    /// The core copy-mode fix: with a selection set, the renderer emits the
    /// real pane content and reverse-videos (SGR 7) the selected cells — no
    /// screen clear, no separate overlay surface.
    #[test]
    fn selection_emits_reverse_video_for_selected_cells() {
        let mut t = fresh(10, 2);
        t.vt_write(b"hello");
        let mut r = TerminalRenderer::new().expect("renderer");
        r.set_selection(Some(SelectionRect {
            start_row: 0,
            start_col: 0,
            end_row: 0,
            end_col: 1,
            rectangle: false,
        }));
        let mut out: Vec<u8> = Vec::new();
        let _ = r.render_at_full(ReplicaWalk::for_test(&t), &mut out, (0, 0), (10, 2));
        let s = String::from_utf8_lossy(&out);
        // Inverse is emitted first in emit_sgr_set, so the param leads the CSI.
        assert!(
            s.contains("\x1b[7"),
            "expected reverse-video (SGR 7) for the selection, got {s:?}"
        );
        // The real content is still there (no blank/clear). The glyphs are
        // split by the selection's SGR runs (`\x1b[7mhe\x1b[0mllo`), so check
        // the selected and unselected halves separately.
        assert!(s.contains("he"), "selected glyphs must render, got {s:?}");
        assert!(
            s.contains("llo"),
            "unselected glyphs must render, got {s:?}"
        );
        // And without a selection the same render has no reverse-video.
        r.set_selection(None);
        let mut plain: Vec<u8> = Vec::new();
        let _ = r.render_at_full(ReplicaWalk::for_test(&t), &mut plain, (0, 0), (10, 2));
        assert!(!String::from_utf8_lossy(&plain).contains("\x1b[7"));
    }

    // `SelectionRect::contains` (linear + block geometry) is owned and tested
    // by the shared contract module, `render::overlay::selection`, after
    // ADR-0045 relocated the type there. The render-layer test below
    // (`block_and_linear_selection_invert_different_cells`) instead exercises
    // the *paint* path — that the two geometries emit different VT.

    /// Block and linear inversion visibly differ in the emitted VT. Three rows
    /// so row 1 is a *true interior* row; select corners (0,2)..(2,5). Linear
    /// reverse-videos the full interior row (including its leading `ab`); block
    /// reverse-videos only the [2,5] band on every row, so the interior row's
    /// leading `ab` stays plain. The renders MUST differ on exactly that.
    #[test]
    fn block_and_linear_selection_invert_different_cells() {
        let mut t = fresh(8, 3);
        // Distinct glyphs per row; the interior row's leading pair "ab" is
        // unique to row 1, so a substring match pins the interior row.
        t.vt_write(b"ABCDEFGH\r\nabcdefgh\r\n01234567");
        let sel_corners = |rectangle| SelectionRect {
            start_row: 0,
            start_col: 2,
            end_row: 2,
            end_col: 5,
            rectangle,
        };

        let mut r = TerminalRenderer::new().expect("renderer");

        r.set_selection(Some(sel_corners(false)));
        let mut linear_out: Vec<u8> = Vec::new();
        let _ = r.render_at_full(ReplicaWalk::for_test(&t), &mut linear_out, (0, 0), (8, 3));
        let linear = String::from_utf8_lossy(&linear_out);

        r.set_selection(Some(sel_corners(true)));
        let mut block_out: Vec<u8> = Vec::new();
        let _ = r.render_at_full(ReplicaWalk::for_test(&t), &mut block_out, (0, 0), (8, 3));
        let block = String::from_utf8_lossy(&block_out);

        // Both invert *something*, and the two geometries produce different VT.
        assert!(linear.contains("\x1b[7"), "linear must invert something");
        assert!(block.contains("\x1b[7"), "block must invert something");
        assert_ne!(
            linear, block,
            "block and linear geometries must paint differently"
        );

        // The load-bearing contrast, robust to SGR coalescing: each row starts
        // with a `\x1b[0m` reset (emitted = default). In BLOCK, the interior
        // row's leading `ab` (cols 0,1 — outside the [2,5] band) is plain, so
        // the glyphs follow the row-start reset directly: `\x1b[0mab`. In
        // LINEAR the whole interior row is selected, so an inverse SGR sits
        // between the reset and `ab`, and `\x1b[0mab` never appears. "ab" is
        // unique to the interior row, so this isolates that row.
        assert!(
            block.contains("\x1b[0mab"),
            "block: interior row's leading `ab` (outside the band) stays plain \
             right after the row reset, got {block:?}"
        );
        assert!(
            !linear.contains("\x1b[0mab"),
            "linear: interior row is fully selected, so `ab` is inverted (an \
             SGR intervenes after the reset), got {linear:?}"
        );
        // And the shared band glyphs c,d,e,f (cols 2..=5) render in both.
        assert!(block.contains("cdef"), "block band glyphs, got {block:?}");
        assert!(
            linear.contains("cdef"),
            "linear band glyphs, got {linear:?}"
        );
    }

    fn fresh(cols: u16, rows: u16) -> GhosttyTerminal<'static, 'static> {
        {
            let mut terminal = GhosttyTerminal::new(cols, rows).expect("Terminal::new");
            terminal
                .set_scrollback_max_lines(Some(100))
                .expect("Terminal::new");
            terminal
        }
    }

    /// ADR-0086: the renderer's pooled render state is rebuilt when the pane's
    /// grid changes dimensions, so a post-resize paint serves the live grid
    /// rather than the pooled cache's pre-resize row bodies (`phux-5pyx`).
    ///
    /// The server's snapshot synthesizer has had this since `phux-5pyx`; the
    /// client renderer inherited it by adopting the shared `RenderPool`, and
    /// before that adoption it had no equivalent. Like the server's
    /// counterpart this is a forward-looking contract lock, not a
    /// fails-without-the-fix guard: the original staleness is a
    /// timing-dependent shared-dirty-bit race with no deterministic
    /// single-threaded repro.
    #[test]
    fn pooled_render_state_is_rebuilt_after_a_geometry_change() {
        let mut t = fresh(10, 2);
        t.vt_write(b"AA");
        let mut renderer = TerminalRenderer::new().expect("renderer");

        let mut before: Vec<u8> = Vec::new();
        let _ = renderer
            .render_at_full(ReplicaWalk::for_test(&t), &mut before, (0, 0), (10, 2))
            .expect("first paint");
        assert_eq!(
            renderer.pool.last_dims(),
            Some((10, 2)),
            "the first walk records the live dims"
        );

        // Grow the grid and overwrite row 0, then race a walk through a
        // SEPARATE render state that consumes the terminal's per-row dirty
        // bits — the shape that leaves a pooled cache serving stale rows.
        t.resize(10, 4, 0, 0).expect("resize");
        t.vt_write(b"\x1b[1;1HZZ");
        let _ = read_grid(&t, 10, 4);

        let mut after: Vec<u8> = Vec::new();
        let _ = renderer
            .render_at_full(ReplicaWalk::for_test(&t), &mut after, (0, 0), (10, 4))
            .expect("post-resize paint");
        assert_eq!(
            renderer.pool.last_dims(),
            Some((10, 4)),
            "the pool tracks the new dims"
        );
        let painted = String::from_utf8_lossy(&after);
        assert!(
            painted.contains("ZZ"),
            "post-resize paint must serve the fresh 'ZZ', not the stale cache, got {painted:?}"
        );
    }

    /// phux-994s: a walk-identity (generation) change rebuilds the pooled
    /// render state even at identical geometry, so the first incremental
    /// paint of the new generation repaints every row instead of trusting
    /// the previous generation's already-painted cache.
    ///
    /// Unlike the resize test above, this one IS a deterministic
    /// fails-without-the-fix guard: driving the same terminal under a new
    /// token is exactly what a replaced `Terminal` whose pages recycled the
    /// old allocation looks like from the pool's seat — the case
    /// libghostty's viewport-pin comparison cannot catch.
    #[test]
    fn pooled_render_state_is_rebuilt_after_a_generation_change() {
        let mut t = fresh(10, 2);
        t.vt_write(b"AA");
        let mut renderer = TerminalRenderer::new().expect("renderer");

        let mut first = Vec::new();
        let _ = renderer
            .render_at(
                ReplicaWalk {
                    terminal: &t,
                    generation: 1,
                },
                &mut first,
                (0, 0),
                (10, 2),
            )
            .expect("first paint");
        assert!(
            String::from_utf8_lossy(&first).contains("AA"),
            "first paint serves the rows"
        );

        // Steady state under the same token: nothing changed, nothing paints.
        let mut steady = Vec::new();
        let _ = renderer
            .render_at(
                ReplicaWalk {
                    terminal: &t,
                    generation: 1,
                },
                &mut steady,
                (0, 0),
                (10, 2),
            )
            .expect("steady paint");
        assert!(
            !String::from_utf8_lossy(&steady).contains("AA"),
            "same generation with no writes must not repaint rows"
        );

        // New token, same terminal, same geometry: must repaint everything.
        let mut swapped = Vec::new();
        let _ = renderer
            .render_at(
                ReplicaWalk {
                    terminal: &t,
                    generation: 2,
                },
                &mut swapped,
                (0, 0),
                (10, 2),
            )
            .expect("post-swap paint");
        assert!(
            String::from_utf8_lossy(&swapped).contains("AA"),
            "a generation change at unchanged geometry must force a repaint \
             from the fresh pooled state, not serve the old Clean cache"
        );
    }

    /// phux-l96p.2 updated the pinned prefix: a painted frame now OPENS with
    /// the DEC 2026 begin (the paint is a transaction — see [`SyncOutput`])
    /// and hides the cursor immediately inside it, and closes with the
    /// matching end. The hide/show pair itself is unchanged.
    #[test]
    fn renderer_writes_cursor_hide_then_show_for_dirty_full() {
        let mut terminal = fresh(5, 2);
        terminal.vt_write(b"ab");
        let mut renderer = TerminalRenderer::new().expect("TerminalRenderer::new");
        let mut buf = Vec::new();
        let _ = renderer
            .render(ReplicaWalk::for_test(&terminal), &mut buf)
            .expect("render");
        // Opens the synchronized-output transaction, then hides the cursor.
        let mut prefix = SYNC_OUTPUT_BEGIN.to_vec();
        prefix.extend_from_slice(b"\x1b[?25l");
        assert!(
            buf.starts_with(&prefix),
            "frame must open with sync-begin + cursor hide; got {:?}",
            String::from_utf8_lossy(&buf)
        );
        assert!(
            buf.ends_with(SYNC_OUTPUT_END),
            "frame must close the transaction; got {:?}",
            String::from_utf8_lossy(&buf)
        );
        // Should contain the literal characters "a" and "b" somewhere.
        let s = String::from_utf8_lossy(&buf);
        assert!(s.contains('a') && s.contains('b'));
    }

    /// The guard NESTS: an inner block emits nothing, so a `render_at`
    /// inside a frame-level transaction (what `paint_full_frame` opens
    /// around several panes plus the chrome) cannot end it early. This is
    /// the invariant that lets the frame-level and per-pane blocks land as
    /// independent changes.
    #[test]
    fn synchronized_output_blocks_nest() {
        let mut outer: Vec<u8> = Vec::new();
        let guard = SyncOutput::begin(&mut outer).expect("outer begin");
        assert_eq!(outer, SYNC_OUTPUT_BEGIN);

        let mut inner: Vec<u8> = Vec::new();
        let nested = SyncOutput::begin(&mut inner).expect("inner begin");
        nested.end(&mut inner).expect("inner end");
        assert!(
            inner.is_empty(),
            "a nested block must emit nothing; got {inner:?}"
        );

        outer.clear();
        guard.end(&mut outer).expect("outer end");
        assert_eq!(outer, SYNC_OUTPUT_END);

        // Depth is back to zero, so the next block is outermost again.
        let mut again: Vec<u8> = Vec::new();
        let guard = SyncOutput::begin(&mut again).expect("begin");
        assert_eq!(again, SYNC_OUTPUT_BEGIN);
        again.clear();
        guard.end(&mut again).expect("end");
        assert_eq!(again, SYNC_OUTPUT_END);
    }

    /// A sink that fails the begin write must not strand the nesting depth.
    /// A leaked level would make every later block on this thread believe it
    /// was nested and emit no mode bytes at all — every frame silently
    /// un-synchronized, with nothing to notice it.
    #[test]
    fn failed_begin_releases_the_nesting_depth() {
        struct FailingSink;
        impl Write for FailingSink {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("sink closed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        assert!(SyncOutput::begin(&mut FailingSink).is_err());

        let mut after: Vec<u8> = Vec::new();
        let guard = SyncOutput::begin(&mut after).expect("begin");
        assert_eq!(
            after, SYNC_OUTPUT_BEGIN,
            "the next block must still be outermost"
        );
        after.clear();
        guard.end(&mut after).expect("end");
        assert_eq!(after, SYNC_OUTPUT_END);
    }

    /// A frame that painted nothing (no dirty rows, cursor unmoved) must
    /// still emit ZERO bytes — the sync-output transaction opens only on the
    /// dirty path, so an idle pane costs nothing per server frame.
    #[test]
    fn clean_frame_emits_no_transaction_bytes() {
        let mut terminal = fresh(5, 2);
        terminal.vt_write(b"ab");
        let mut renderer = TerminalRenderer::new().expect("TerminalRenderer::new");
        let mut buf = Vec::new();
        let _ = renderer
            .render(ReplicaWalk::for_test(&terminal), &mut buf)
            .expect("first render");
        buf.clear();
        let _ = renderer
            .render(ReplicaWalk::for_test(&terminal), &mut buf)
            .expect("clean render");
        assert!(buf.is_empty(), "clean frame emitted {buf:?}");
    }

    /// The renderer never flushes: ADR-0029's composite end-of-frame owns the
    /// frame's single flush, so a pane paint leaves its bytes in the sink.
    #[test]
    fn pane_paint_does_not_flush() {
        #[derive(Default)]
        struct FlushCounter {
            bytes: Vec<u8>,
            flushes: usize,
        }
        impl Write for FlushCounter {
            fn write(&mut self, data: &[u8]) -> io::Result<usize> {
                self.bytes.extend_from_slice(data);
                Ok(data.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                self.flushes += 1;
                Ok(())
            }
        }

        let mut terminal = fresh(5, 2);
        terminal.vt_write(b"ab");
        let mut renderer = TerminalRenderer::new().expect("TerminalRenderer::new");
        let mut sink = FlushCounter::default();
        let _ = renderer
            .render(ReplicaWalk::for_test(&terminal), &mut sink)
            .expect("dirty render");
        // A pure cursor move on an otherwise clean frame must not flush either.
        terminal.vt_write(b"\x1b[1;1H");
        let _ = renderer
            .render(ReplicaWalk::for_test(&terminal), &mut sink)
            .expect("clean render");
        assert_eq!(sink.flushes, 0, "renderer flushed {} times", sink.flushes);
        assert!(!sink.bytes.is_empty(), "renderer emitted nothing at all");
    }

    /// phux-7ry0 regression: rendering a pane at a non-zero outer origin
    /// (a lower split leaf) must cache the cursor BOTH ways — outer-absolute
    /// in `last_cursor` (for the host-cursor restore) and pane-local in
    /// `last_cursor_local` (for the predictive-echo anchor) — and record the
    /// paint origin. Feeding the predict layer the outer-absolute cursor was
    /// the bug: its pane-grid clamp dragged a lower pane's cursor up into the
    /// middle of the screen (the ghost echo).
    #[test]
    fn render_at_offset_caches_pane_local_cursor_and_origin() {
        let mut terminal = fresh(5, 2);
        terminal.vt_write(b"ab"); // cursor lands pane-local at (row 0, col 2)
        let mut renderer = TerminalRenderer::new().expect("TerminalRenderer::new");
        let mut buf = Vec::new();
        // Paint as the bottom leaf of a 24-row split: origin (x=0, y=13).
        let _ = renderer
            .render_at(ReplicaWalk::for_test(&terminal), &mut buf, (0, 13), (5, 2))
            .expect("render_at");
        assert_eq!(
            renderer.last_cursor_local(),
            Some((0, 2)),
            "pane-local cursor must be origin-free (the predict anchor)"
        );
        assert_eq!(
            renderer.last_cursor(),
            Some((13, 2)),
            "outer cursor must include the pane origin offset"
        );
        assert_eq!(
            renderer.last_origin(),
            (0, 13),
            "last_origin must record where the pane was painted"
        );
    }

    /// Alt-screen exit must repaint the restored primary screen. A TUI app
    /// (claude, vim, htop) enters 1049h, paints, then exits with 1049l; the
    /// restored primary rows + the shell's fresh prompt must be emitted, not
    /// skipped as Clean with only a cursor reposition.
    #[test]
    fn alt_screen_exit_repaints_restored_primary_screen() {
        let mut terminal = fresh(20, 5);
        terminal.vt_write(b"$ old-prompt");
        let mut renderer = TerminalRenderer::new().expect("TerminalRenderer::new");
        let mut buf = Vec::new();
        let _ = renderer
            .render(ReplicaWalk::for_test(&terminal), &mut buf)
            .expect("render 1");

        // Enter alt screen, paint a TUI frame, render it.
        terminal.vt_write(b"\x1b[?1049h\x1b[2J\x1b[HTUI-FRAME");
        buf.clear();
        let _ = renderer
            .render(ReplicaWalk::for_test(&terminal), &mut buf)
            .expect("render 2");
        assert!(
            String::from_utf8_lossy(&buf).contains("TUI-FRAME"),
            "alt-screen content must paint"
        );

        // Exit alt screen; the shell prints a fresh prompt.
        terminal.vt_write(b"\x1b[?1049l\r\n$ new-prompt");
        buf.clear();
        let dirty = renderer
            .render(ReplicaWalk::for_test(&terminal), &mut buf)
            .expect("render 3");
        let s = String::from_utf8_lossy(&buf);
        assert!(
            !matches!(dirty, Dirty::Clean),
            "alt-screen exit must not classify as Clean"
        );
        assert!(
            s.contains("old-prompt") && s.contains("new-prompt"),
            "restored primary rows + new prompt must repaint, got {s:?}"
        );
    }

    /// Incremental-paint baseline: a second `render` of a terminal with no
    /// new input is `Dirty::Clean` and emits ZERO bytes. This is what the
    /// status-bar cache change leans on — the focused pane render is
    /// already a no-op on a steady screen, so the per-frame paint cost
    /// collapses toward zero when nothing changed.
    #[test]
    fn second_render_of_unchanged_terminal_emits_nothing() {
        let mut terminal = fresh(10, 3);
        terminal.vt_write(b"hello");
        let mut renderer = TerminalRenderer::new().expect("TerminalRenderer::new");
        let mut first = Vec::new();
        let _ = renderer
            .render(ReplicaWalk::for_test(&terminal), &mut first)
            .expect("first render");
        assert!(!first.is_empty(), "first render must emit content");

        // No new vt_write — the grid is unchanged, so render is Clean.
        let mut second = Vec::new();
        let dirty = renderer
            .render(ReplicaWalk::for_test(&terminal), &mut second)
            .expect("second render");
        assert!(
            matches!(dirty, Dirty::Clean),
            "unchanged terminal must report Clean, got {dirty:?}"
        );
        assert!(
            second.is_empty(),
            "unchanged repaint must emit zero bytes; got {:?}",
            String::from_utf8_lossy(&second)
        );
    }

    /// A pure cursor move — no cell changed, so libghostty reports
    /// `Dirty::Clean` — must still reposition the on-screen cursor (and
    /// refresh `last_cursor`), or the cursor lags a frame behind arrow-key
    /// navigation / autosuggestion-accept until the next dirtying keystroke.
    #[test]
    fn cursor_only_move_repositions_on_a_clean_render() {
        let mut terminal = fresh(10, 2);
        terminal.vt_write(b"hello"); // cursor lands at (row 0, col 5)
        let mut renderer = TerminalRenderer::new().expect("TerminalRenderer::new");
        let mut first = Vec::new();
        let _ = renderer
            .render(ReplicaWalk::for_test(&terminal), &mut first)
            .expect("first render");
        assert_eq!(renderer.last_cursor(), Some((0, 5)));

        // Move the cursor only — `\x1b[1;3H` ⇒ row 0, col 2 — no cell changes.
        terminal.vt_write(b"\x1b[1;3H");
        let mut second = Vec::new();
        let dirty = renderer
            .render(ReplicaWalk::for_test(&terminal), &mut second)
            .expect("second render");
        assert!(
            matches!(dirty, Dirty::Clean),
            "a cursor-only move leaves rows Clean, got {dirty:?}"
        );
        let s = String::from_utf8_lossy(&second);
        assert!(
            s.contains("\x1b[1;3H"),
            "Clean render must reposition the cursor to (0,2) ⇒ CUP 1;3; got {s:?}"
        );
        assert_eq!(
            renderer.last_cursor(),
            Some((0, 2)),
            "cached cursor must follow the move so the bar-restore agrees"
        );
    }

    #[test]
    fn hidden_cursor_is_not_cached_as_visible() {
        let mut terminal = fresh(10, 2);
        terminal.vt_write(b"hello");
        let mut renderer = TerminalRenderer::new().expect("TerminalRenderer::new");
        let mut first = Vec::new();
        let _ = renderer
            .render(ReplicaWalk::for_test(&terminal), &mut first)
            .expect("first render");
        assert_eq!(renderer.last_cursor(), Some((0, 5)));

        terminal.vt_write(b"\x1b[?25l");
        let mut hidden = Vec::new();
        let _ = renderer
            .render(ReplicaWalk::for_test(&terminal), &mut hidden)
            .expect("hidden render");
        assert!(
            hidden.windows(6).any(|bytes| bytes == b"\x1b[?25l"),
            "cursor visibility change must reach the host terminal"
        );
        assert_eq!(renderer.last_cursor(), None);
        assert_eq!(renderer.last_cursor_local(), None);

        terminal.vt_write(b"\x1b[?25h");
        let mut shown = Vec::new();
        let _ = renderer
            .render(ReplicaWalk::for_test(&terminal), &mut shown)
            .expect("shown render");
        assert!(shown.windows(6).any(|bytes| bytes == b"\x1b[?25h"));
        assert_eq!(renderer.last_cursor(), Some((0, 5)));
    }

    /// A single changed row repaints only that row: the emitted CUP
    /// targets the touched row and the untouched row's content is absent.
    #[test]
    fn single_row_change_repaints_only_that_row() {
        let mut terminal = fresh(10, 3);
        // Row 0 = "top", row 1 = "mid" (CRLF between).
        terminal.vt_write(b"top\r\nmid");
        let mut renderer = TerminalRenderer::new().expect("TerminalRenderer::new");
        let mut first = Vec::new();
        let _ = renderer
            .render(ReplicaWalk::for_test(&terminal), &mut first)
            .expect("first render");

        // Park the cursor on row 1 (CUP row 2, col 1) and overwrite it.
        terminal.vt_write(b"\x1b[2;1HNEW");
        let mut second = Vec::new();
        let _ = renderer
            .render(ReplicaWalk::for_test(&terminal), &mut second)
            .expect("second render");
        let s = String::from_utf8_lossy(&second);
        // The changed row (row index 1 ⇒ CUP row 2) must be re-emitted with
        // its new content. The renderer interleaves an SGR reset between
        // cells, so "NEW" is not contiguous — assert on each glyph.
        assert!(
            s.contains("\x1b[2;1H"),
            "changed row CUP missing; out = {s:?}"
        );
        assert!(
            s.contains('N') && s.contains('E') && s.contains('W'),
            "changed row content missing; out = {s:?}"
        );
        // The unchanged row 0 ("top") must NOT be re-emitted: no CUP to
        // row 1 (1-based) and no "top" text on the wire.
        assert!(
            !s.contains("\x1b[1;1H"),
            "unchanged row 0 should not be repainted (CUP leaked); out = {s:?}"
        );
        assert!(
            !s.contains("top"),
            "unchanged row 0 content should not be repainted; out = {s:?}"
        );
    }

    /// Count occurrences of `needle` in `hay`.
    fn count(hay: &[u8], needle: &[u8]) -> usize {
        if needle.is_empty() {
            return 0;
        }
        hay.windows(needle.len()).filter(|w| *w == needle).count()
    }

    /// Render a single terminal once and return the emitted bytes.
    fn render_once(terminal: &GhosttyTerminal<'_, '_>) -> Vec<u8> {
        let mut renderer = TerminalRenderer::new().expect("TerminalRenderer::new");
        let mut buf = Vec::new();
        let _ = renderer
            .render(ReplicaWalk::for_test(terminal), &mut buf)
            .expect("render");
        buf
    }

    /// One visible cell, normalized for grid comparison: a blank cell
    /// (no grapheme) and a single space are the same visible verdict, so
    /// both collapse to `None`. Colors are kept even on blanks because a
    /// colored gap (e.g. a bg run) is visually distinct.
    ///
    /// The ATTRIBUTES are kept for the same reason the colors are, and
    /// because dropping them made the round-trip assertions much weaker than
    /// they looked: a renderer that emitted the right glyph in the right
    /// colour but lost every bold/italic/underline/inverse would have
    /// satisfied a colour-and-char comparison exactly.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct VisCell {
        ch: Option<char>,
        fg: Option<RgbColor>,
        bg: Option<RgbColor>,
        attrs: VisAttrs,
    }

    /// The text attributes a cell carries, all of which
    /// [`write_reset_and_sgr`] emits and a terminal re-parses.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[allow(
        clippy::struct_excessive_bools,
        reason = "a faithful projection of libghostty's own eight independent Style flags; folding them into enums would stop this mirroring the thing it compares against"
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
        /// Emitted from `style.underline_color` directly (it has no resolved
        /// accessor), so a pen that drops or mangles it shows here.
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

    fn vis_cell(
        graphemes: &[char],
        fg: Option<RgbColor>,
        bg: Option<RgbColor>,
        style: &Style,
    ) -> VisCell {
        let ch = match graphemes {
            [] | [' '] => None,
            [c, ..] => Some(*c),
        };
        VisCell {
            ch,
            fg,
            bg,
            attrs: VisAttrs::of(style),
        }
    }

    /// Read the visible grid of a live terminal, normalized via [`vis_cell`].
    fn read_grid(terminal: &GhosttyTerminal<'_, '_>, cols: u16, rows: u16) -> Vec<VisCell> {
        let mut state = RenderState::new().expect("RenderState");
        let mut rows_it = RowIterator::new().expect("RowIterator");
        let mut cells_it = CellIterator::new().expect("CellIterator");
        let snap = state.update(terminal).expect("snapshot");
        let mut out = Vec::new();
        let mut row_iter = rows_it.update(&snap).expect("rows");
        let mut ri: u16 = 0;
        while let Some(row) = row_iter.next() {
            if ri >= rows {
                break;
            }
            let mut cell_iter = cells_it.update(row).expect("cells");
            let mut ci: u16 = 0;
            while let Some(cell) = cell_iter.next() {
                if ci >= cols {
                    break;
                }
                out.push(vis_cell(
                    &cell.graphemes().expect("graphemes"),
                    cell.fg_color().expect("fg"),
                    cell.bg_color().expect("bg"),
                    &cell.style().expect("style"),
                ));
                ci += 1;
            }
            ri += 1;
        }
        out
    }

    /// Decode `bytes` into a fresh `cols`x`rows` terminal and return its
    /// normalized visible grid. This is the in-crate stand-in for the Screen
    /// oracle: it proves the coalesced byte stream reconstructs the same
    /// visible grid, not just the same byte count.
    fn decode_grid(bytes: &[u8], cols: u16, rows: u16) -> Vec<VisCell> {
        let mut term = fresh(cols, rows);
        term.vt_write(bytes);
        read_grid(&term, cols, rows)
    }

    /// (a) A row of N identical-style colored cells emits exactly ONE SGR
    /// set for the run, not one per cell.
    #[test]
    fn identical_colored_run_emits_single_sgr() {
        let cols = 20u16;
        let mut terminal = fresh(cols, 1);
        // Set a truecolor fg, then write a full row of the same color.
        terminal.vt_write(b"\x1b[38;2;10;20;30m");
        terminal.vt_write(&vec![b'x'; cols as usize]);
        let buf = render_once(&terminal);

        // The truecolor fg set appears exactly once for the whole run.
        assert_eq!(
            count(&buf, b"38;2;10;20;30"),
            1,
            "expected a single fg SGR for the identical-style run; out = {:?}",
            String::from_utf8_lossy(&buf)
        );
        // All N glyphs are present.
        assert_eq!(
            count(&buf, b"x"),
            cols as usize,
            "all glyphs must be emitted"
        );
    }

    /// (b) A row alternating two styles emits an SGR per style change, not
    /// per cell.
    #[test]
    fn alternating_styles_emit_one_sgr_per_change() {
        let cols = 10u16;
        let mut terminal = fresh(cols, 1);
        // Alternate red / green truecolor fg per cell.
        for i in 0..cols {
            if i % 2 == 0 {
                terminal.vt_write(b"\x1b[38;2;255;0;0m");
            } else {
                terminal.vt_write(b"\x1b[38;2;0;255;0m");
            }
            terminal.vt_write(b"z");
        }
        let buf = render_once(&terminal);

        let reds = count(&buf, b"38;2;255;0;0");
        let greens = count(&buf, b"38;2;0;255;0");
        // 10 cells alternating ⇒ 5 reds, 5 greens — one SGR per change, i.e.
        // one per cell here because every adjacent pair differs. The point is
        // we emit no MORE than the number of style changes.
        assert_eq!(reds, 5, "one red SGR per red cell; out = {reds}");
        assert_eq!(greens, 5, "one green SGR per green cell; out = {greens}");
    }

    /// A run of three same-color cells between two differently-colored
    /// neighbors collapses to one SGR for the middle run.
    #[test]
    fn middle_run_collapses_to_single_sgr() {
        let cols = 5u16;
        let mut terminal = fresh(cols, 1);
        terminal.vt_write(b"\x1b[38;2;1;1;1mA"); // cell 0: color A
        terminal.vt_write(b"\x1b[38;2;2;2;2mBBB"); // cells 1-3: color B (run)
        terminal.vt_write(b"\x1b[38;2;3;3;3mC"); // cell 4: color C
        let buf = render_once(&terminal);
        assert_eq!(count(&buf, b"38;2;2;2;2"), 1, "middle run is one SGR");
        assert_eq!(count(&buf, b"BBB"), 1, "run glyphs are contiguous");
    }

    /// (c) Round-trip: feed the coalesced output back through a fresh
    /// libghostty Terminal and assert the reconstructed grid equals the
    /// source grid — coalesced output renders identically.
    #[test]
    fn coalesced_output_round_trips_to_identical_grid() {
        let cols = 24u16;
        let rows = 3u16;
        let mut terminal = fresh(cols, rows);
        // A mix that exercises runs, a return-to-default gap, a bg color, and
        // attributes, across rows.
        terminal.vt_write(b"\x1b[38;2;200;100;50mHELLO");
        terminal.vt_write(b"\x1b[0m   "); // default-style gap
        terminal.vt_write(b"\x1b[1;48;2;0;0;255mWORLD"); // bold + bg
        terminal.vt_write(b"\r\n");
        terminal.vt_write(b"\x1b[3;38;2;9;9;9mitalics same color run");
        let buf = render_once(&terminal);

        let src = read_grid(&terminal, cols, rows);
        let reconstructed = decode_grid(&buf, cols, rows);
        assert_eq!(
            src, reconstructed,
            "coalesced output must reconstruct the source grid exactly"
        );
    }

    /// The batched row read (`phux-l96p.9`) hands the emitter a per-cell
    /// STYLE-RUN INDEX and coalesces on that instead of on a materialized
    /// `Style`. Two places that could drift from the per-cell reads it
    /// replaced:
    ///
    /// * a style the row returns to later gets a NEW run index, so the run
    ///   key alone would call it a change — the real `(Style, fg, bg)`
    ///   comparison behind it has to notice it is not;
    /// * a background that comes from the CELL's content tag rather than
    ///   from a style entry shares its neighbours' run index while differing
    ///   from them, so the run key has to carry the resolved colours too.
    ///
    /// Both are covered here by reconstructing the grid from the emitted
    /// bytes, which fails on any pen that reaches the terminal wrong.
    #[test]
    fn style_runs_that_repeat_or_carry_a_cell_background_round_trip() {
        let cols = 24u16;
        let rows = 2u16;
        let mut terminal = fresh(cols, rows);
        // Bold red, back to default, then bold red AGAIN: a repeated style
        // that the run dedup sees as two separate runs.
        terminal.vt_write(b"\x1b[1;38;2;200;0;0mAAA\x1b[0mBBB\x1b[1;38;2;200;0;0mCCC\x1b[0m");
        terminal.vt_write(b"\r\n");
        // Cell-tagged backgrounds (erase with a bg set) next to a wide glyph
        // and a combining cluster.
        terminal.vt_write("\x1b[48;2;0;0;90m  \x1b[0m\u{6771}e\u{301}x".as_bytes());
        let buf = render_once(&terminal);

        let src = read_grid(&terminal, cols, rows);
        let reconstructed = decode_grid(&buf, cols, rows);
        assert_eq!(
            src, reconstructed,
            "batched row reads must reconstruct the source grid exactly"
        );
        // The repeated style is still coalesced per run, not per cell: three
        // A cells and three C cells cost two sequences between them, not six.
        assert_eq!(count(&buf, b"38;2;200;0;0"), 2);
    }

    /// (verify the win) A heavy-colored full-width dirty row emits
    /// substantially fewer bytes coalesced than the per-cell baseline would.
    #[test]
    fn colored_full_row_emits_far_fewer_bytes_than_per_cell() {
        let cols = 80u16;
        let mut terminal = fresh(cols, 1);
        terminal.vt_write(b"\x1b[38;2;120;200;40m");
        terminal.vt_write(&vec![b'#'; cols as usize]);
        let buf = render_once(&terminal);

        // Pre-coalescing, each of the 80 cells emitted `\x1b[0m` (4 bytes)
        // plus `\x1b[38;2;120;200;40m` (18 bytes) plus the glyph (1) ≈ 23
        // bytes/cell ⇒ ~1840 bytes for the run alone. Coalesced, the run is
        // one such sequence (~22 bytes) plus 80 glyphs.
        let per_cell_baseline = cols as usize * (4 + 18 + 1);
        assert!(
            buf.len() * 3 < per_cell_baseline,
            "coalesced row ({} bytes) should be far smaller than the \
             per-cell baseline (~{} bytes)",
            buf.len(),
            per_cell_baseline
        );
        // Exactly one fg SGR for the whole run is the source of the win.
        assert_eq!(count(&buf, b"38;2;120;200;40"), 1);
    }

    /// Returning to the default style mid-row emits a single reset, and a
    /// default run at row start emits no SGR at all.
    #[test]
    fn default_run_emits_at_most_one_reset() {
        let cols = 10u16;
        let mut terminal = fresh(cols, 1);
        // First half colored, second half default.
        terminal.vt_write(b"\x1b[38;2;7;7;7mAAAAA");
        terminal.vt_write(b"\x1b[0mBBBBB");
        let buf = render_once(&terminal);
        // Row leads with one reset (row start) + the colored SGR + a reset
        // when returning to default. Count total `\x1b[0m`: row-start reset
        // (1) + return-to-default (1) + the colored set's leading reset (1)
        // + final cursor reset (1) = 4. The key invariant: the default run
        // (BBBBB) added exactly one reset, not one per cell.
        assert_eq!(count(&buf, b"BBBBB"), 1, "default run glyphs contiguous");
        // The colored fg appears once.
        assert_eq!(count(&buf, b"38;2;7;7;7"), 1);
        // Round-trip equality as the real correctness guard.
        let reconstructed = decode_grid(&buf, cols, 1);
        let src = read_grid(&terminal, cols, 1);
        assert_eq!(
            src, reconstructed,
            "default-gap row round-trips identically"
        );
    }

    /// phux-wurs: the render must clip to the pane's rect, not to the
    /// (server-authoritative) mirror grid. When the mirror is WIDER than the
    /// rect — the resize-handshake window where the server's grid is still
    /// width N while the client layout reports width M < N — `render_at` must
    /// emit at most M columns per row. Painting the mirror's full width would
    /// spill prior content past the rect (the ghost cells / divider overrun).
    #[test]
    fn render_at_clips_columns_to_rect_not_mirror_width() {
        // Mirror is 20 cols wide, full of distinct content across the row.
        let mirror_cols = 20u16;
        let mut terminal = fresh(mirror_cols, 1);
        terminal.vt_write(b"ABCDEFGHIJKLMNOPQRST"); // 20 glyphs, cols 0..20
        let mut renderer = TerminalRenderer::new().expect("renderer");
        // Rect is only 12 cols wide.
        let rect_cols = 12u16;
        let mut out: Vec<u8> = Vec::new();
        let _ = renderer
            .render_at(
                ReplicaWalk::for_test(&terminal),
                &mut out,
                (0, 0),
                (rect_cols, 1),
            )
            .expect("render");
        let s = String::from_utf8_lossy(&out);
        // Columns inside the rect (0..12 ⇒ 'A'..'L') are painted.
        assert!(
            s.contains('A') && s.contains('L'),
            "in-rect glyphs must paint; out = {s:?}"
        );
        // Columns past the rect (12..20 ⇒ 'M'..'T') must NOT be emitted — they
        // would land beyond the pane's rect (divider / neighbour pane), which
        // is exactly the right-side ghost.
        for ch in ['M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T'] {
            assert!(
                !s.contains(ch),
                "column {ch} past the rect must not be painted; out = {s:?}"
            );
        }
    }

    /// phux-wurs: the row walk clips to the rect height too — a mirror taller
    /// than the rect must not paint rows below the rect.
    #[test]
    fn render_at_clips_rows_to_rect_not_mirror_height() {
        let cols = 6u16;
        let mut terminal = fresh(cols, 4);
        terminal.vt_write(b"row0\r\nrow1\r\nrow2\r\nrow3");
        let mut renderer = TerminalRenderer::new().expect("renderer");
        // Rect is only 2 rows tall.
        let mut out: Vec<u8> = Vec::new();
        let _ = renderer
            .render_at(
                ReplicaWalk::for_test(&terminal),
                &mut out,
                (0, 0),
                (cols, 2),
            )
            .expect("render");
        let s = String::from_utf8_lossy(&out);
        // Rows 0..2 emit a CUP (1-based rows 1 and 2); row 2/3 (1-based 3/4)
        // must not.
        assert!(s.contains("\x1b[1;1H"), "row 0 CUP missing; out = {s:?}");
        assert!(s.contains("\x1b[2;1H"), "row 1 CUP missing; out = {s:?}");
        assert!(
            !s.contains("\x1b[3;1H"),
            "row 2 past the rect must not paint; out = {s:?}"
        );
        assert!(
            !s.contains("\x1b[4;1H"),
            "row 3 past the rect must not paint; out = {s:?}"
        );
    }

    /// phux-l5xa: `render_at_cells` projects graphemes + resolved style into
    /// a dense frame, shifted by the origin, and returns the cursor in
    /// frame-absolute coordinates.
    #[test]
    fn render_at_cells_projects_graphemes_style_and_cursor() {
        let mut terminal = fresh(10, 3);
        // Bold "Hi", reset, then " X": cols 0..1 bold, col 2 a default space,
        // col 3 a default 'X'. Cursor parks pane-local at col 4.
        terminal.vt_write(b"\x1b[1mHi\x1b[0m X");
        let mut renderer = TerminalRenderer::new().expect("renderer");
        let mut frame = RenderedFrame::blank(12, 4);
        let cursor = renderer
            .render_at_cells(
                ReplicaWalk::for_test(&terminal),
                &mut frame,
                (1, 1),
                (10, 3),
            )
            .expect("render_at_cells");

        assert_eq!(frame.cell(1, 1).expect("in range").grapheme, "H");
        assert!(frame.cell(1, 1).expect("in range").style.bold, "H is bold");
        assert_eq!(frame.cell(1, 2).expect("in range").grapheme, "i");
        assert!(frame.cell(1, 2).expect("in range").style.bold);
        assert_eq!(frame.cell(1, 3).expect("in range").grapheme, " ");
        assert!(
            !frame.cell(1, 3).expect("in range").style.bold,
            "the space after the reset is default style"
        );
        assert_eq!(frame.cell(1, 4).expect("in range").grapheme, "X");
        // Cells outside the painted rect stay blank.
        assert_eq!(frame.cell(0, 0).expect("in range").grapheme, " ");

        let c = cursor.expect("cursor present");
        assert_eq!((c.x, c.y), (5, 1), "pane col 4 + origin (1,1)");
    }

    // ── phux-7ubw: single-view letterbox ─────────────────────────────────

    /// The centring math: a mirror smaller than the rect on both axes is
    /// centred with a floor split, the extra cell of an odd gap landing on
    /// the bottom/right margin.
    #[test]
    fn letterbox_rect_centers_with_floor_split() {
        // Rect 10x6 at origin (0,0), mirror 6x4: even gaps (4 cols, 2 rows).
        let lb = letterbox_rect((0, 0), (10, 6), (6, 4));
        assert_eq!(lb.inner_origin, (2, 1), "even gap centres symmetrically");
        assert_eq!(lb.inner_clip, (6, 4), "clip is the mirror size");
        assert_eq!((lb.margin_left, lb.margin_right), (2, 2));
        assert_eq!((lb.margin_top, lb.margin_bottom), (1, 1));

        // Odd gaps: rect 9x5, mirror 6x4 ⇒ gap 3 cols / 1 row. Floor split
        // puts the extra pad on the right/bottom.
        let lb = letterbox_rect((0, 0), (9, 5), (6, 4));
        assert_eq!(
            (lb.margin_left, lb.margin_right),
            (1, 2),
            "extra col on right"
        );
        assert_eq!(
            (lb.margin_top, lb.margin_bottom),
            (0, 1),
            "extra row on bottom"
        );
        assert_eq!(lb.inner_origin, (1, 0));
    }

    /// The centred origin is offset by the rect origin too, so a pane that is
    /// not the top-left leaf letterboxes within its own rect.
    #[test]
    fn letterbox_rect_offsets_by_rect_origin() {
        let lb = letterbox_rect((4, 3), (10, 6), (6, 4));
        // rect origin (4,3) + pad (2,1).
        assert_eq!(lb.inner_origin, (6, 4));
    }

    /// A mirror that fills or exceeds the rect produces no pad and clamps the
    /// clip to the rect — the existing `render_at` behaviour (phux-wurs).
    #[test]
    fn letterbox_rect_clamps_when_mirror_ge_rect() {
        // Equal: no pad, clip == rect.
        let lb = letterbox_rect((0, 0), (8, 4), (8, 4));
        assert_eq!(lb.inner_origin, (0, 0));
        assert_eq!(lb.inner_clip, (8, 4));
        assert_eq!(
            (
                lb.margin_left,
                lb.margin_right,
                lb.margin_top,
                lb.margin_bottom
            ),
            (0, 0, 0, 0)
        );

        // Larger: still no pad, clip clamps DOWN to the rect.
        let lb = letterbox_rect((0, 0), (8, 4), (20, 10));
        assert_eq!(lb.inner_origin, (0, 0));
        assert_eq!(
            lb.inner_clip,
            (8, 4),
            "clip clamps to the rect, not the mirror"
        );
        assert_eq!(
            (
                lb.margin_left,
                lb.margin_right,
                lb.margin_top,
                lb.margin_bottom
            ),
            (0, 0, 0, 0)
        );
    }

    /// An undersized mirror renders centred: its content's CUP is shifted by
    /// the pad, and the margin rows/cols are blanked.
    #[test]
    fn render_at_letterboxed_centers_undersized_mirror() {
        // Mirror is 4x2 of "ab"/"cd"; rect is 8x4 ⇒ pad (2 cols, 1 row) each.
        let mut terminal = fresh(4, 2);
        terminal.vt_write(b"ab\r\ncd");
        let mut renderer = TerminalRenderer::new().expect("renderer");
        let mut out: Vec<u8> = Vec::new();
        let _ = renderer
            .render_at_letterboxed(
                ReplicaWalk::for_test(&terminal),
                &mut out,
                (0, 0),
                (8, 4),
                (4, 2),
                true,
            )
            .expect("render");
        let s = String::from_utf8_lossy(&out);

        // Content is centred: row 0 of the mirror lands at outer row 1
        // (0-based) ⇒ 1-based CUP row 2, col = pad_left 2 ⇒ 1-based col 3.
        assert!(
            s.contains("\x1b[2;3H"),
            "centred content CUP (row 2, col 3) missing; out = {s:?}"
        );
        // The top margin row (outer row 0 ⇒ CUP 1;1) is blanked full-width.
        assert!(
            s.contains("\x1b[1;1H"),
            "top margin bar CUP missing; out = {s:?}"
        );
        // The bottom margin row: content occupies outer rows 1..3, so the
        // bottom bar is outer row 3 ⇒ CUP 4;1.
        assert!(
            s.contains("\x1b[4;1H"),
            "bottom margin bar CUP missing; out = {s:?}"
        );
        // The content glyphs are present.
        assert!(s.contains('a') && s.contains('d'), "content missing; {s:?}");
    }

    /// An undersized mirror blanks exactly N margin rows + the left/right
    /// margin columns: decode the emitted bytes into an 8x4 grid and confirm
    /// the centred 4x2 content sits in the middle with blank borders.
    #[test]
    fn render_at_letterboxed_blanks_margins_around_content() {
        let mut terminal = fresh(4, 2);
        terminal.vt_write(b"WXYZ\r\nMNOP"); // 4 cols x 2 rows of content
        let mut renderer = TerminalRenderer::new().expect("renderer");
        let mut out: Vec<u8> = Vec::new();
        let _ = renderer
            .render_at_letterboxed(
                ReplicaWalk::for_test(&terminal),
                &mut out,
                (0, 0),
                (8, 4),
                (4, 2),
                true,
            )
            .expect("render");

        // Decode into an 8x4 grid: content centred at cols 2..6, rows 1..3.
        let grid = decode_grid(&out, 8, 4);
        let at = |r: usize, c: usize| grid[r * 8 + c].ch;
        // Top + bottom margin rows are entirely blank.
        for c in 0..8 {
            assert_eq!(at(0, c), None, "top margin row must be blank at col {c}");
            assert_eq!(at(3, c), None, "bottom margin row must be blank at col {c}");
        }
        // Interior rows: left (cols 0,1) and right (cols 6,7) margins blank,
        // content in cols 2..6.
        for r in 1..3 {
            assert_eq!(at(r, 0), None, "left margin blank, row {r}");
            assert_eq!(at(r, 1), None, "left margin blank, row {r}");
            assert_eq!(at(r, 6), None, "right margin blank, row {r}");
            assert_eq!(at(r, 7), None, "right margin blank, row {r}");
        }
        assert_eq!(at(1, 2), Some('W'), "content top-left");
        assert_eq!(at(1, 5), Some('Z'), "content top-right");
        assert_eq!(at(2, 2), Some('M'), "content bottom-left");
        assert_eq!(at(2, 5), Some('P'), "content bottom-right");
    }

    /// A mirror equal to the rect is byte-identical to today's
    /// `render_at_full`: no pad ⇒ no margin bars ⇒ the same core paint.
    #[test]
    fn render_at_letterboxed_equal_size_is_byte_identical() {
        let make = || {
            let mut t = fresh(10, 3);
            t.vt_write(b"\x1b[1mHELLO\x1b[0m world\r\nsecond row\r\nthird");
            t
        };

        let t_a = make();
        let mut r_a = TerminalRenderer::new().expect("renderer");
        let mut today: Vec<u8> = Vec::new();
        let _ = r_a
            .render_at_full(ReplicaWalk::for_test(&t_a), &mut today, (0, 0), (10, 3))
            .expect("render_at_full");

        let t_b = make();
        let mut r_b = TerminalRenderer::new().expect("renderer");
        let mut letterboxed: Vec<u8> = Vec::new();
        let _ = r_b
            .render_at_letterboxed(
                ReplicaWalk::for_test(&t_b),
                &mut letterboxed,
                (0, 0),
                (10, 3),
                (10, 3),
                true,
            )
            .expect("render_at_letterboxed");

        assert_eq!(
            today, letterboxed,
            "mirror==rect letterbox must be byte-identical to render_at_full"
        );
    }

    /// A mirror larger than the rect clamps exactly as `render_at` does (the
    /// phux-wurs clip): no margin bars, content confined to the rect.
    #[test]
    fn render_at_letterboxed_larger_mirror_clamps_like_render_at() {
        let make = || {
            let mut t = fresh(20, 4);
            t.vt_write(b"ABCDEFGHIJKLMNOPQRST\r\nabcdefghijklmnopqrst");
            t
        };

        // Today's clamp path.
        let t_a = make();
        let mut r_a = TerminalRenderer::new().expect("renderer");
        let mut clamp: Vec<u8> = Vec::new();
        let _ = r_a
            .render_at_full(ReplicaWalk::for_test(&t_a), &mut clamp, (0, 0), (12, 2))
            .expect("render_at_full");

        // Letterboxed path with mirror 20x4 > rect 12x2: must match.
        let t_b = make();
        let mut r_b = TerminalRenderer::new().expect("renderer");
        let mut letterboxed: Vec<u8> = Vec::new();
        let _ = r_b
            .render_at_letterboxed(
                ReplicaWalk::for_test(&t_b),
                &mut letterboxed,
                (0, 0),
                (12, 2),
                (20, 4),
                true,
            )
            .expect("render_at_letterboxed");

        assert_eq!(
            clamp, letterboxed,
            "mirror>rect letterbox must clamp byte-identically to render_at_full"
        );
    }

    /// The cursor cached in `last_cursor` (and the recorded `last_origin`)
    /// include the letterbox pad offset, so the composite bar-restore agrees
    /// with where the content was actually painted.
    #[test]
    fn render_at_letterboxed_cursor_includes_pad_offset() {
        let mut terminal = fresh(4, 2);
        terminal.vt_write(b"ab"); // cursor parks pane-local at (row 0, col 2)
        let mut renderer = TerminalRenderer::new().expect("renderer");
        let mut out: Vec<u8> = Vec::new();
        // Rect 8x4, mirror 4x2 ⇒ pad (2 cols, 1 row).
        let _ = renderer
            .render_at_letterboxed(
                ReplicaWalk::for_test(&terminal),
                &mut out,
                (0, 0),
                (8, 4),
                (4, 2),
                true,
            )
            .expect("render");
        // Pane-local cursor is origin-free (the predict anchor).
        assert_eq!(renderer.last_cursor_local(), Some((0, 2)));
        // Outer cursor includes the pad: (row 0 + pad_top 1, col 2 + pad_left 2).
        assert_eq!(
            renderer.last_cursor(),
            Some((1, 4)),
            "last_cursor must include the letterbox pad offset"
        );
        // The recorded paint origin is the centred (padded) origin.
        assert_eq!(renderer.last_origin(), (2, 1));
    }

    /// phux-l5xa: a double-width glyph occupies its base cell; the
    /// `SpacerTail` column is the empty grapheme so widths stay exact.
    #[test]
    fn render_at_cells_marks_wide_glyph_tail_empty() {
        let mut terminal = fresh(6, 2);
        terminal.vt_write("世".as_bytes());
        let mut renderer = TerminalRenderer::new().expect("renderer");
        let mut frame = RenderedFrame::blank(6, 2);
        let _ = renderer
            .render_at_cells(ReplicaWalk::for_test(&terminal), &mut frame, (0, 0), (6, 2))
            .expect("render_at_cells");
        assert_eq!(frame.cell(0, 0).expect("in range").grapheme, "世");
        assert_eq!(
            frame.cell(0, 1).expect("in range").grapheme,
            "",
            "the wide glyph's tail column is the empty grapheme"
        );
        assert_eq!(
            frame.cell(0, 2).expect("in range").grapheme,
            " ",
            "the cell after the wide glyph is a normal blank"
        );
    }

    #[test]
    fn live_vt_render_does_not_overwrite_wide_glyph_tail() {
        let mut terminal = fresh(6, 1);
        terminal.vt_write("世X".as_bytes());

        let emitted = render_once(&terminal);
        assert!(
            emitted
                .windows("世X".len())
                .any(|window| window == "世X".as_bytes()),
            "the SpacerTail must emit no intervening space: {:?}",
            String::from_utf8_lossy(&emitted)
        );
        assert_eq!(
            decode_grid(&emitted, 6, 1),
            read_grid(&terminal, 6, 1),
            "the emitted VT must reconstruct the wide glyph and following cell at their logical columns"
        );
    }

    #[test]
    fn selecting_only_a_wide_tail_highlights_its_base_glyph() {
        let mut terminal = fresh(6, 1);
        terminal.vt_write("世X".as_bytes());
        let mut renderer = TerminalRenderer::new().expect("renderer");
        renderer.set_selection(Some(SelectionRect {
            start_row: 0,
            start_col: 1,
            end_row: 0,
            end_col: 1,
            rectangle: true,
        }));
        let mut emitted = Vec::new();
        renderer
            .render_at_full(
                ReplicaWalk::for_test(&terminal),
                &mut emitted,
                (0, 0),
                (6, 1),
            )
            .expect("render");
        let output = String::from_utf8_lossy(&emitted);
        let inverse = output.find("\x1b[7").expect("tail selection is visible");
        let glyph = output.find('世').expect("wide glyph is rendered");
        assert!(inverse < glyph, "inverse style must precede the wide glyph");
    }

    // ---------------------------------------------------------------
    // The batched row read, held to the per-cell walk it replaced
    // ---------------------------------------------------------------

    /// The benchmark corpora, so the byte-identity gate below runs over the
    /// same grids the render bench measures rather than a hand-picked few.
    /// Compiled in at the crate root — see [`crate::bench_support`].
    use crate::bench_support as support;

    /// A cell source that reports `len` columns but refuses one of them —
    /// the shape `RowCells::get` takes when a cluster's byte range does not
    /// land on a UTF-8 boundary.
    struct HolySource {
        len: usize,
        hole: usize,
    }

    impl<'buf> RowCellSource<'buf> for HolySource {
        fn cell_count(&self) -> usize {
            self.len
        }
        fn cell_at(&self, col: usize) -> Option<RowCell<'buf>> {
            if col == self.hole {
                return None;
            }
            Some(RowCell {
                text: "x",
                style_index: 0,
                fg: None,
                bg: None,
                wide: CellWide::Narrow,
            })
        }
    }

    /// A column the row read cannot produce must STOP the walk with an error,
    /// never be skipped.
    ///
    /// `RowCells::iter` is a `filter_map`, so the natural
    /// `(0..cols).zip(batch.iter())` would drop the bad column and hand every
    /// later cell the column to its left: the row paints one short with its
    /// whole tail shifted, silently. The per-cell walk this replaced advanced
    /// the column and the cell together and could not do that, so neither may
    /// the batched one.
    #[test]
    fn an_unreadable_cell_errors_instead_of_shifting_the_row() {
        let mut visited: Vec<u16> = Vec::new();
        let err = walk_row_cells(&HolySource { len: 8, hole: 3 }, 8, |col, _| {
            visited.push(col);
            Ok(())
        })
        .expect_err("an unreadable column must be reported");

        assert!(
            matches!(err, RenderError::UnreadableCell { col: 3 }),
            "the failing column must be named, got {err:?}"
        );
        assert_eq!(
            visited,
            vec![0, 1, 2],
            "the walk stops at the hole; it must not carry on with shifted columns"
        );
    }

    /// A readable row visits every column exactly once, in order, and stops
    /// at the shorter of the row and the clip.
    #[test]
    fn a_readable_row_visits_every_column_in_order() {
        let mut visited: Vec<u16> = Vec::new();
        walk_row_cells(&HolySource { len: 8, hole: 99 }, 5, |col, _| {
            visited.push(col);
            Ok(())
        })
        .expect("no hole");
        assert_eq!(visited, vec![0, 1, 2, 3, 4], "clip wins when it is shorter");

        visited.clear();
        walk_row_cells(&HolySource { len: 3, hole: 99 }, 40, |col, _| {
            visited.push(col);
            Ok(())
        })
        .expect("no hole");
        assert_eq!(visited, vec![0, 1, 2], "the row wins when it is shorter");
    }

    /// The pre-`8a49759d` cell-emission path, preserved so the batched read
    /// can be held to BYTE identity against it rather than to a
    /// visible-grid round trip.
    ///
    /// This is the old code, not a paraphrase: one crossing into libghostty
    /// per value per cell, the `prev_had_text` shortcut that let `wide` go
    /// unread across a run of blanks, and the unstyled-row shortcut that
    /// skipped the style and foreground reads. It shares
    /// [`emit_sgr_if_changed`] and [`write_cup`] with the production path on
    /// purpose: the only thing that may differ between the two is HOW a
    /// cell's values are read, so any byte difference is attributable to the
    /// batched read.
    ///
    /// [`RefRowStyling`] is a copy of the row-level `styled` shortcut the
    /// production path no longer needs, kept here so the reference stays the
    /// code that actually shipped rather than a simplification of it.
    struct RefRowState {
        emitted: Option<EmittedStyle>,
        prev_had_text: bool,
        styling: RefRowStyling,
    }

    /// Whether a row holds any styled cell, decided once per row.
    ///
    /// libghostty keeps a deliberately conservative `styled` flag per row: it
    /// is set the first time a styled cell is written to the row and never
    /// cleared again. So it can be a false POSITIVE, never a false negative,
    /// which is what made it safe for the per-cell walk to lead with: on an
    /// unstyled row every cell's `style()` resolved to the default and every
    /// `fg_color()` to `None`, so both reads could be skipped for the whole
    /// row at the cost of one flag.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum RefRowStyling {
        Unstyled,
        Styled,
    }

    impl RefRowStyling {
        fn of(row: &RowIteration<'_, '_>) -> Result<Self, RenderError> {
            Ok(if row.raw_row()?.is_styled()? {
                Self::Styled
            } else {
                Self::Unstyled
            })
        }
    }

    fn reference_paint_rows<'alloc>(
        out: &mut Vec<u8>,
        cluster: &mut String,
        row_iter: &mut RowIteration<'alloc, '_>,
        cells: &mut CellIterator<'alloc>,
        origin: (u16, u16),
        extent: (u16, u16),
        selection: Option<SelectionRect>,
    ) -> Result<(), RenderError> {
        let (cols_total, rows_total) = extent;
        let (ox, oy) = origin;
        let mut row_index: u16 = 0;
        while let Some(row) = row_iter.next() {
            if row_index >= rows_total {
                break;
            }
            write_cup(out, row_index.saturating_add(oy), ox)?;
            out.extend_from_slice(b"\x1b[0m");
            let mut state = RefRowState {
                emitted: None,
                prev_had_text: false,
                styling: RefRowStyling::of(row)?,
            };
            let mut col: u16 = 0;
            let mut cell_iter = cells.update(row)?;
            while let Some(cell) = cell_iter.next() {
                if col >= cols_total {
                    break;
                }
                reference_emit_cell(out, cluster, cell, &mut state, selection, (row_index, col))?;
                col = col.saturating_add(1);
            }
            row.set_dirty(false)?;
            row_index = row_index.saturating_add(1);
        }
        Ok(())
    }

    fn reference_emit_cell(
        out: &mut Vec<u8>,
        cluster: &mut String,
        cell: &libghostty_vt::render::CellIteration<'_, '_>,
        state: &mut RefRowState,
        selection: Option<SelectionRect>,
        at: (u16, u16),
    ) -> Result<(), RenderError> {
        let (row, col) = at;
        cluster.clear();
        cell.graphemes_utf8(cluster)?;
        let has_text = !cluster.is_empty();
        let wide = reference_read_wide(cell, state, has_text, selection)?;
        state.prev_had_text = has_text;
        if matches!(wide, CellWide::SpacerTail) {
            return Ok(());
        }
        let (mut style, fg, bg) = reference_read_cell_pen(cell, state.styling, has_text)?;
        if selection_covers_cell(selection, row, col, wide) {
            style.inverse = !style.inverse;
        }
        emit_sgr_if_changed(out, &mut state.emitted, style, fg, bg);
        if cluster.is_empty() {
            out.push(b' ');
        } else {
            out.extend_from_slice(cluster.as_bytes());
        }
        Ok(())
    }

    fn reference_read_wide(
        cell: &libghostty_vt::render::CellIteration<'_, '_>,
        state: &RefRowState,
        has_text: bool,
        selection: Option<SelectionRect>,
    ) -> Result<CellWide, RenderError> {
        let could_be_tail = !has_text && state.prev_had_text;
        if could_be_tail || selection.is_some() {
            return Ok(cell.raw_cell()?.wide()?);
        }
        Ok(CellWide::Narrow)
    }

    fn reference_read_cell_pen(
        cell: &libghostty_vt::render::CellIteration<'_, '_>,
        styling: RefRowStyling,
        has_text: bool,
    ) -> Result<(Style, Option<RgbColor>, Option<RgbColor>), RenderError> {
        if matches!(styling, RefRowStyling::Styled) {
            return Ok((cell.style()?, cell.fg_color()?, cell.bg_color()?));
        }
        let bg = if has_text { None } else { cell.bg_color()? };
        Ok((Style::default(), None, bg))
    }

    /// The row payload the PRODUCTION path emits for a full-dirty frame —
    /// `paint_dirty_rows` alone, with no prologue, cursor or epilogue, so the
    /// comparison isolates the cell loop.
    ///
    /// Both whole-row paths are held to it: the one that records the row as
    /// it emits (an incremental paint of an unknown row) and the one that
    /// records nothing (a forced paint). They must agree byte for byte.
    fn production_row_bytes(
        terminal: &GhosttyTerminal<'_, '_>,
        extent: (u16, u16),
        selection: Option<SelectionRect>,
    ) -> Vec<u8> {
        let recorded = production_row_bytes_with(terminal, extent, selection, true);
        let unrecorded = production_row_bytes_with(terminal, extent, selection, false);
        assert_same_bytes(
            "recording vs forced whole-row paint",
            &recorded,
            &unrecorded,
        );
        recorded
    }

    fn production_row_bytes_with(
        terminal: &GhosttyTerminal<'_, '_>,
        extent: (u16, u16),
        selection: Option<SelectionRect>,
        record: bool,
    ) -> Vec<u8> {
        let mut state = RenderState::new().expect("RenderState");
        let mut rows_it = RowIterator::new().expect("RowIterator");
        let mut cells_it = CellIterator::new().expect("CellIterator");
        let snap = state.update(terminal).expect("snapshot");
        let mut scratch = CellScratch::default();
        // A fresh front buffer knows nothing, so every row takes the
        // full-row path — the one this gate holds to the per-cell walk.
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
            &mut scratch,
            &mut front,
            &mut row_iter,
            &mut cells_it,
            Dirty::Full,
            (0, 0),
            extent,
            selection,
            record,
        )
        .expect("paint");
        out
    }

    /// The same payload as the pre-batch per-cell walk would have emitted.
    fn reference_row_bytes(
        terminal: &GhosttyTerminal<'_, '_>,
        extent: (u16, u16),
        selection: Option<SelectionRect>,
    ) -> Vec<u8> {
        let mut state = RenderState::new().expect("RenderState");
        let mut rows_it = RowIterator::new().expect("RowIterator");
        let mut cells_it = CellIterator::new().expect("CellIterator");
        let snap = state.update(terminal).expect("snapshot");
        let mut out = Vec::new();
        let mut cluster = String::new();
        let mut row_iter = rows_it.update(&snap).expect("rows");
        reference_paint_rows(
            &mut out,
            &mut cluster,
            &mut row_iter,
            &mut cells_it,
            (0, 0),
            extent,
            selection,
        )
        .expect("reference paint");
        out
    }

    /// Report the first differing byte with a readable window rather than
    /// dumping two 15 KB buffers.
    fn assert_same_bytes(label: &str, actual: &[u8], expected: &[u8]) {
        if actual == expected {
            return;
        }
        let at = actual
            .iter()
            .zip(expected)
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| actual.len().min(expected.len()));
        let from = at.saturating_sub(40);
        panic!(
            "{label}: batched and per-cell emission diverge at byte {at} \
             (lengths {} vs {})\n  batched:  {:?}\n  per-cell: {:?}",
            actual.len(),
            expected.len(),
            String::from_utf8_lossy(&actual[from..(at + 40).min(actual.len())]),
            String::from_utf8_lossy(&expected[from..(at + 40).min(expected.len())]),
        );
    }

    /// Build one of the shared benchmark corpora, mirroring
    /// `phux-tui/benches/render_frame.rs::build_terminal` so the gate and
    /// the bench walk the same cells.
    fn corpus_terminal(corpus: support::Corpus) -> GhosttyTerminal<'static, 'static> {
        let (cols, rows) = corpus.geometry();
        let mut terminal = {
            let mut terminal = GhosttyTerminal::new(cols, rows).expect("corpus terminal");
            terminal
                .set_scrollback_max_lines(Some(corpus.history_lines().max(1_000)))
                .expect("corpus terminal");
            terminal
        };

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
                    let color = 16 + (u32::from(row) * 37 % 216);
                    let line = format!(
                        "\x1b[{};1H\x1b[38;5;{}m{:03} {:<170}\x1b[0m",
                        row + 1,
                        color,
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

    /// A grid built for the cases the corpora do not reliably contain: a row
    /// that ENDS on a wide glyph's spacer tail, a background-only cell (a
    /// cell whose colour comes from its own content tag rather than a style
    /// entry), and a style the row returns to after leaving it.
    fn edge_case_terminal() -> GhosttyTerminal<'static, 'static> {
        // 8 columns: "abcdef" then a wide glyph occupying cols 6-7, so the
        // row's LAST column is the glyph's spacer tail.
        let mut terminal = {
            let mut terminal = GhosttyTerminal::new(8, 4).expect("edge terminal");
            terminal
                .set_scrollback_max_lines(Some(100))
                .expect("edge terminal");
            terminal
        };
        terminal.vt_write("abcdef\u{6771}".as_bytes());
        terminal.vt_write(b"\r\n");
        // Background-only cells: erase a run with a bg set, so the cells carry
        // a content-tag background and no style entry.
        terminal.vt_write(b"\x1b[48;2;0;0;90m\x1b[K\x1b[0m");
        terminal.vt_write(b"\r\n");
        // A style the row leaves and returns to, plus a combining cluster.
        terminal.vt_write("\x1b[1;4;38;2;9;9;9mAA\x1b[0mB\x1b[1;4;38;2;9;9;9mCe\u{301}".as_bytes());
        terminal.vt_write(b"\r\n");
        // A fully blank row, so the all-default fast path is covered too.
        terminal
    }

    /// The batched row read must emit the SAME BYTES as the per-cell walk it
    /// replaced, over every benchmark corpus and over the edge cases the
    /// corpora do not cover, with and without a copy-mode selection.
    ///
    /// The visible-grid round trips elsewhere in this module prove the output
    /// RECONSTRUCTS correctly; this proves the change was a pure speedup,
    /// which is the claim `8a49759d` actually made.
    #[test]
    fn batched_row_read_emits_the_same_bytes_as_the_per_cell_walk() {
        for corpus in support::Corpus::ALL {
            let extent = corpus.geometry();
            let actual = production_row_bytes(&corpus_terminal(corpus), extent, None);
            let expected = reference_row_bytes(&corpus_terminal(corpus), extent, None);
            assert!(!actual.is_empty(), "{} emitted nothing", corpus.label());
            assert_same_bytes(corpus.label(), &actual, &expected);
        }
    }

    /// The same identity over a row ending in a wide glyph's spacer tail, a
    /// background-only cell, and a repeated style — the three shapes whose
    /// per-cell shortcuts (`prev_had_text`, the unstyled-row pen, the
    /// style-run index) the batched read had to reproduce rather than
    /// approximate.
    #[test]
    fn batched_row_read_matches_the_per_cell_walk_on_the_edge_cases() {
        let extent = (8u16, 4u16);
        assert_same_bytes(
            "edge cases",
            &production_row_bytes(&edge_case_terminal(), extent, None),
            &reference_row_bytes(&edge_case_terminal(), extent, None),
        );
    }

    /// And with a live copy-mode selection, whose edges are where the two
    /// paths' `wide` handling could most easily disagree: the per-cell walk
    /// read `wide` for real only under a selection, and a selection that
    /// covers a wide glyph's BASE must also invert its tail column.
    #[test]
    fn batched_row_read_matches_the_per_cell_walk_under_a_selection() {
        let extent = (8u16, 4u16);
        // Row 0 ends `...\u{6771}` across cols 6-7. Select up to col 6 (the
        // base) so the selection edge lands exactly on the wide pair, and
        // again up to col 5 so it stops one short of it.
        for end_col in [5u16, 6, 7] {
            for rectangle in [false, true] {
                let selection = Some(SelectionRect {
                    start_row: 0,
                    start_col: 1,
                    end_row: 2,
                    end_col,
                    rectangle,
                });
                assert_same_bytes(
                    &format!("selection end_col={end_col} rectangle={rectangle}"),
                    &production_row_bytes(&edge_case_terminal(), extent, selection),
                    &reference_row_bytes(&edge_case_terminal(), extent, selection),
                );
            }
        }
    }

    // ---------------------------------------------------------------
    // phux-esge: the cell-diff paint and its front buffer
    // ---------------------------------------------------------------

    /// One screen cell as a viewer sees it: the whole cluster, the wide-glyph
    /// role, the resolved colours, and every attribute. A blank and a written
    /// space are the same verdict, and a spacer head (the blank a wide glyph
    /// leaves when it wraps) is read as the ordinary blank the renderer paints
    /// for it — a pre-existing projection the diff neither causes nor fixes.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Seen {
        text: String,
        wide: CellWide,
        fg: Option<RgbColor>,
        bg: Option<RgbColor>,
        attrs: VisAttrs,
    }

    /// Read the `extent = (cols, rows)` region of `terminal` whose top-left is
    /// `origin = (x, y)`.
    fn read_seen(
        terminal: &GhosttyTerminal<'_, '_>,
        origin: (u16, u16),
        extent: (u16, u16),
    ) -> Vec<Seen> {
        let (ox, oy) = origin;
        let (cols, rows) = extent;
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

    /// The outer terminal in these tests: a fresh libghostty grid the
    /// renderer's bytes are replayed into, frame after frame, so what it
    /// shows is what a real terminal would show after the same stream.
    struct Glass {
        screen: GhosttyTerminal<'static, 'static>,
    }

    impl Glass {
        fn new(cols: u16, rows: u16) -> Self {
            Self {
                screen: fresh(cols, rows),
            }
        }

        /// Paint `pane` through `renderer` with its top-left at `origin`,
        /// replay the bytes onto the glass, and return them.
        fn paint(
            &mut self,
            renderer: &mut TerminalRenderer<'static>,
            pane: &GhosttyTerminal<'static, 'static>,
            origin: (u16, u16),
            force: bool,
        ) -> Vec<u8> {
            let clip = pane_extent(pane);
            let walk = ReplicaWalk::for_test(pane);
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

        /// Write bytes onto the glass behind the renderer's back — a modal,
        /// a prediction, a clear.
        fn scribble(&mut self, bytes: &[u8]) {
            self.screen.vt_write(bytes);
        }

        /// The pane region of the glass.
        fn seen(&self, origin: (u16, u16), extent: (u16, u16)) -> Vec<Seen> {
            read_seen(&self.screen, origin, extent)
        }
    }

    fn pane_extent(pane: &GhosttyTerminal<'_, '_>) -> (u16, u16) {
        (pane.cols().expect("cols"), pane.rows().expect("rows"))
    }

    /// Fail with the first diverging cell when the glass does not show the
    /// pane at `origin`.
    fn assert_glass_shows(
        glass: &Glass,
        pane: &GhosttyTerminal<'_, '_>,
        origin: (u16, u16),
        label: &str,
    ) {
        let extent = pane_extent(pane);
        let want = read_seen(pane, (0, 0), extent);
        let got = glass.seen(origin, extent);
        assert_same_screen(label, &want, &got, extent.0);
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
            "{label}: screens diverge at row {}, col {}\n  want: {:?}\n  got:  {:?}\n\
             want grid:\n{}got grid:\n{}",
            at / cols,
            at % cols,
            want.get(at),
            got.get(at),
            dump_screen(want, cols),
            dump_screen(got, cols),
        );
    }

    /// A readable grid for a failure message: each cell's text (`.` for a
    /// blank, `~` for a spacer tail), with a styled cell bracketed.
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

    /// The printable text in `bytes`, every escape sequence removed.
    ///
    /// Written over raw bytes with numeric constants: bracket character
    /// literals throw the project's `lizard` complexity report off its parse.
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
                i = csi_end(bytes, i);
            } else if STRING_OPENERS.contains(&kind) {
                i = string_end(bytes, i);
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// One past the final byte of the CSI whose parameters start at `i`.
    fn csi_end(bytes: &[u8], mut i: usize) -> usize {
        while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
            i += 1;
        }
        i + 1
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

    /// The acceptance case: one changed cell on an otherwise steady screen
    /// costs one positioned glyph, not a row.
    #[test]
    fn an_incremental_frame_after_a_single_cell_change_emits_only_that_cell() {
        let mut pane = fresh(20, 3);
        pane.vt_write(b"\x1b[1;32mhello world\x1b[0m\r\nsecond row here\r\nthird");
        let mut renderer = TerminalRenderer::new().expect("renderer");
        let mut glass = Glass::new(20, 3);
        let _ = glass.paint(&mut renderer, &pane, (0, 0), false);

        pane.vt_write(b"\x1b[2;8HX");
        let frame = glass.paint(&mut renderer, &pane, (0, 0), false);
        let s = String::from_utf8_lossy(&frame);
        assert_eq!(
            printed(&frame),
            "X",
            "only the changed cell may be written; {s:?}"
        );
        assert!(
            s.contains("\x1b[2;8H"),
            "the span lands on the changed cell; {s:?}"
        );
        assert_glass_shows(&glass, &pane, (0, 0), "single-cell change");
    }

    /// Within one pane paint the pen a span leaves is still active after the
    /// next jump, on the same row and on the next one: three same-pen changes
    /// cost one SGR and no reset between them, and replay to the same grid.
    #[test]
    fn the_pen_carries_across_jumps_within_a_paint() {
        let mut pane = fresh(30, 3);
        pane.vt_write(b"abcdefghijklmnopqrstuvwxyz\r\nabcdefghijklmnopqrstuvwxyz");
        let mut renderer = TerminalRenderer::new().expect("renderer");
        let mut glass = Glass::new(30, 3);
        let _ = glass.paint(&mut renderer, &pane, (0, 0), false);
        pane.vt_write(b"\x1b[1;38;2;0;200;0m\x1b[1;3HX\x1b[1;20HY\x1b[2;7HZ\x1b[0m");
        let frame = glass.paint(&mut renderer, &pane, (0, 0), false);
        let s = String::from_utf8_lossy(&frame);
        assert_eq!(
            count(&frame, b"38;2;0;200;0"),
            1,
            "one SGR for three spans; {s:?}"
        );
        assert_eq!(
            count(&frame, b"\x1b[0m"),
            2,
            "only the first span's reset-and-set and the epilogue reset; {s:?}"
        );
        assert!(
            s.contains("\x1b[1;20HY") && s.contains("\x1b[2;7HZ"),
            "later spans jump straight to their glyph; {s:?}"
        );
        assert_glass_shows(&glass, &pane, (0, 0), "pen carried across jumps");
    }

    /// The FIRST span of a pane paint carries its own complete SGR: it may not
    /// inherit the pen of whatever another writer left before it.
    #[test]
    fn the_first_span_of_a_paint_sets_its_pen_from_scratch() {
        let mut pane = fresh(30, 2);
        pane.vt_write(b"\x1b[1;31mred bold text\x1b[0m and plain");
        let mut renderer = TerminalRenderer::new().expect("renderer");
        let mut glass = Glass::new(30, 2);
        let _ = glass.paint(&mut renderer, &pane, (0, 0), false);
        // Leave the glass's pen bold red, as some other writer might.
        glass.scribble(b"\x1b[1;31m");
        pane.vt_write(b"\x1b[1;20Hq");
        let frame = glass.paint(&mut renderer, &pane, (0, 0), false);
        let s = String::from_utf8_lossy(&frame);
        let glyph = s.find('q').expect("the change is written");
        assert!(
            s[..glyph].ends_with("\x1b[0m"),
            "a default-pen span after a jump resets explicitly; {s:?}"
        );
        assert_glass_shows(&glass, &pane, (0, 0), "pen after a jump");
    }

    /// A dirty row whose cells did not actually change writes no cells.
    #[test]
    fn a_dirty_row_whose_cells_did_not_change_emits_no_cells() {
        let mut pane = fresh(10, 2);
        pane.vt_write(b"abc");
        let mut renderer = TerminalRenderer::new().expect("renderer");
        let mut glass = Glass::new(10, 2);
        let _ = glass.paint(&mut renderer, &pane, (0, 0), false);
        pane.vt_write(b"\x1b[1;1Habc");
        let frame = glass.paint(&mut renderer, &pane, (0, 0), false);
        assert_eq!(printed(&frame), "", "{:?}", String::from_utf8_lossy(&frame));
        assert_glass_shows(&glass, &pane, (0, 0), "rewrite of identical cells");
    }

    /// The motivating shape (cmatrix): every row dirty, few cells changed. The
    /// frame must cost a small fraction of a whole-screen repaint.
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
                width = usize::from(cols) - 1,
            );
            pane.vt_write(line.as_bytes());
        }
        let mut renderer = TerminalRenderer::new().expect("renderer");
        let mut glass = Glass::new(cols, rows);
        let repaint = glass.paint(&mut renderer, &pane, (0, 0), false);

        for r in 0..rows {
            let at = format!("\x1b[{};{}H\x1b[1;38;5;46m#", r + 1, (r * 3) % cols + 1);
            pane.vt_write(at.as_bytes());
        }
        let frame = glass.paint(&mut renderer, &pane, (0, 0), false);
        assert!(
            frame.len() * 3 < repaint.len(),
            "{} bytes for {rows} changed cells against a {}-byte repaint",
            frame.len(),
            repaint.len()
        );
        assert_eq!(
            printed(&frame).chars().filter(|&c| c == '#').count(),
            usize::from(rows)
        );
        assert_glass_shows(&glass, &pane, (0, 0), "few changes per dirty row");
    }

    /// A short gap between two changes is bridged by rewriting it when that is
    /// cheaper than a `CUP`, and still lands the right screen.
    #[test]
    fn a_short_gap_between_changes_is_rewritten_instead_of_jumped() {
        let mut pane = fresh(40, 1);
        pane.vt_write(b"0123456789abcdefghij");
        let mut renderer = TerminalRenderer::new().expect("renderer");
        let mut glass = Glass::new(40, 1);
        let _ = glass.paint(&mut renderer, &pane, (0, 0), false);
        pane.vt_write(b"\x1b[1;3HX\x1b[1;5HY");
        let frame = glass.paint(&mut renderer, &pane, (0, 0), false);
        let s = String::from_utf8_lossy(&frame);
        assert!(
            printed(&frame).contains("X3Y"),
            "a one-cell gap is rewritten; {s:?}"
        );
        assert!(
            !s.contains("\x1b[1;5H"),
            "no jump for a one-cell gap; {s:?}"
        );
        assert_glass_shows(&glass, &pane, (0, 0), "bridged gap");
    }

    fn overlay_case(invalidate: bool) -> (Glass, GhosttyTerminal<'static, 'static>) {
        let mut pane = fresh(20, 4);
        pane.vt_write(b"row zero\r\nrow one is here\r\nrow two\r\nrow three");
        let mut renderer = TerminalRenderer::new().expect("renderer");
        let mut glass = Glass::new(20, 4);
        let _ = glass.paint(&mut renderer, &pane, (0, 0), false);
        // A modal's box lands over row 1, outside the renderer.
        glass.scribble(b"\x1b[2;1H\x1b[7m### MODAL ###\x1b[0m");
        if invalidate {
            renderer.invalidate_front();
        }
        pane.vt_write(b"\x1b[2;18HZ");
        let _ = glass.paint(&mut renderer, &pane, (0, 0), false);
        (glass, pane)
    }

    /// Invalidation after an overlay: the next paint of the dirty row rewrites
    /// it whole and the box is gone. The control proves the case has teeth —
    /// without the invalidation the diff trusts its stale claim and leaves the
    /// box on screen.
    #[test]
    fn an_overlay_over_the_pane_is_healed_after_invalidation() {
        let (glass, pane) = overlay_case(true);
        assert_glass_shows(&glass, &pane, (0, 0), "overlay then invalidate");

        let (glass, pane) = overlay_case(false);
        assert_ne!(
            glass.seen((0, 0), pane_extent(&pane)),
            read_seen(&pane, (0, 0), pane_extent(&pane)),
            "control: an un-invalidated front buffer must leave the box (or the test proves nothing)"
        );
    }

    fn prediction_case(invalidate: bool) -> (Glass, GhosttyTerminal<'static, 'static>) {
        let mut pane = fresh(20, 2);
        pane.vt_write(b"$ abc");
        let mut renderer = TerminalRenderer::new().expect("renderer");
        let mut glass = Glass::new(20, 2);
        let _ = glass.paint(&mut renderer, &pane, (0, 0), false);
        // A backspace guess: an underlined blank over the `c` — the shape the
        // predictive-echo overlay writes. The shell ignores the key, so the
        // `c` stays, and something else on the row changes.
        glass.scribble(b"\x1b[1;5H\x1b[0m\x1b[4m \x1b[0m");
        if invalidate {
            renderer.invalidate_front_rows(0..1);
        }
        pane.vt_write(b"\x1b[1;15H!");
        let _ = glass.paint(&mut renderer, &pane, (0, 0), false);
        (glass, pane)
    }

    /// The predictive-echo overlay never erases its guesses. Forgetting the
    /// rows it painted puts them back on the whole-row path, so a wrong guess
    /// over an unchanged cell heals when its row is next painted.
    #[test]
    fn predicted_cells_are_rewritten_when_their_rows_are_forgotten() {
        let (glass, pane) = prediction_case(true);
        assert_glass_shows(&glass, &pane, (0, 0), "prediction then invalidate rows");

        let (glass, pane) = prediction_case(false);
        assert_ne!(
            glass.seen((0, 0), pane_extent(&pane)),
            read_seen(&pane, (0, 0), pane_extent(&pane)),
            "control: without forgetting the row, the stale guess must survive"
        );
    }

    /// The full-frame path: a cleared screen and a forced paint redraw every
    /// cell, whatever the front buffer recorded.
    #[test]
    fn a_forced_paint_after_a_screen_clear_repaints_every_cell() {
        let mut pane = fresh(16, 3);
        pane.vt_write(b"\x1b[44mblue\x1b[0m line\r\nsecond\r\nthird \xe4\xb8\x96!");
        let mut renderer = TerminalRenderer::new().expect("renderer");
        let mut glass = Glass::new(16, 3);
        let _ = glass.paint(&mut renderer, &pane, (0, 0), false);
        glass.scribble(b"\x1b[2J");
        let _ = glass.paint(&mut renderer, &pane, (0, 0), true);
        assert_glass_shows(&glass, &pane, (0, 0), "forced paint after ED2");
    }

    /// Touch every row without changing a cell, so every row is dirty and the
    /// diff alone would write nothing.
    ///
    /// The first glyphs are read BEFORE anything is written: reading walks a
    /// separate render state, and a walk between a write and the renderer's
    /// own consumes the dirty bits the renderer needs to see.
    fn touch_every_row(pane: &mut GhosttyTerminal<'static, 'static>) {
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

    /// A relayout that moves the pane (split, zoom, sidebar toggle) voids the
    /// front buffer: the dirty rows are rewritten whole at the new origin.
    #[test]
    fn a_moved_pane_rewrites_its_dirty_rows_whole() {
        let mut pane = fresh(12, 3);
        pane.vt_write(b"alpha\r\nbravo\r\ncharlie");
        let mut renderer = TerminalRenderer::new().expect("renderer");
        let mut glass = Glass::new(12, 6);
        let _ = glass.paint(&mut renderer, &pane, (0, 0), false);
        glass.scribble(b"\x1b[2J");
        touch_every_row(&mut pane);
        let _ = glass.paint(&mut renderer, &pane, (0, 3), false);
        assert_glass_shows(&glass, &pane, (0, 3), "moved origin");
    }

    /// A grid resize voids the front buffer too.
    #[test]
    fn a_resized_grid_rewrites_its_dirty_rows_whole() {
        let mut pane = fresh(10, 3);
        pane.vt_write(b"one\r\ntwo\r\nthree");
        let mut renderer = TerminalRenderer::new().expect("renderer");
        let mut glass = Glass::new(16, 3);
        let _ = glass.paint(&mut renderer, &pane, (0, 0), false);
        glass.scribble(b"\x1b[2J");
        pane.resize(16, 3, 0, 0).expect("resize");
        touch_every_row(&mut pane);
        let _ = glass.paint(&mut renderer, &pane, (0, 0), false);
        assert_glass_shows(&glass, &pane, (0, 0), "resized grid");
    }

    /// Selection changes paint the right inversion both ways, forced (how
    /// copy mode repaints) and on an ordinary dirty row.
    #[test]
    fn selection_changes_repaint_the_inverted_cells_correctly() {
        let mut pane = fresh(12, 2);
        pane.vt_write(b"selectme now\r\nsecond");
        let extent = pane_extent(&pane);
        let mut renderer = TerminalRenderer::new().expect("renderer");
        let mut glass = Glass::new(12, 2);
        let _ = glass.paint(&mut renderer, &pane, (0, 0), false);
        let sel = SelectionRect {
            start_row: 0,
            start_col: 0,
            end_row: 0,
            end_col: 3,
            rectangle: false,
        };
        let inverted_prefix = |glass: &Glass| {
            glass.seen((0, 0), extent)[..4]
                .iter()
                .all(|cell| cell.attrs.inverse)
        };

        for force in [false, true] {
            renderer.set_selection(Some(sel));
            pane.vt_write(b"\x1b[1;12H!");
            let _ = glass.paint(&mut renderer, &pane, (0, 0), force);
            assert!(
                inverted_prefix(&glass),
                "selected cells are inverted (force={force})"
            );
            assert!(
                !glass.seen((0, 0), extent)[4].attrs.inverse,
                "the cell after the selection is not (force={force})"
            );

            renderer.set_selection(None);
            pane.vt_write(b"\x1b[1;12H?");
            let _ = glass.paint(&mut renderer, &pane, (0, 0), force);
            assert_glass_shows(&glass, &pane, (0, 0), "selection cleared");
        }
    }

    /// Wide glyphs appearing, vanishing, and being overwritten through their
    /// tails keep the glass consistent: a span never half-writes a pair.
    #[test]
    fn wide_glyph_edits_keep_the_glass_consistent() {
        let mut pane = fresh(12, 1);
        let mut renderer = TerminalRenderer::new().expect("renderer");
        let mut glass = Glass::new(12, 1);
        let steps: [&str; 9] = [
            "ab\u{4e16}cd\u{754c}",
            "\x1b[1;2H\u{4e16}",
            "\x1b[1;5Hx",
            "\x1b[1;4Hy",
            "\x1b[1;1H\x1b[P",
            "\x1b[1;3H\x1b[2@",
            "\x1b[1;6H\x1b[31m\u{754c}\x1b[0m",
            "\x1b[1;7H\x1b[1mZ\x1b[0m",
            "\x1b[1;1H\u{1f980}\u{1f980}e\u{301}",
        ];
        for (i, step) in steps.into_iter().enumerate() {
            pane.vt_write(step.as_bytes());
            let _ = glass.paint(&mut renderer, &pane, (0, 0), false);
            assert_glass_shows(&glass, &pane, (0, 0), &format!("wide step {i}"));
        }
    }

    /// A deterministic generator, so the property test needs no dependency
    /// and every failure replays from its seed.
    struct XorShift(u64);

    impl XorShift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
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

    /// One random edit of the kind real programs make: positioned styled
    /// text, erases, inserts and deletes, scrolls, screen switches, and
    /// rewrites that change nothing.
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

    /// One paint target of the property test: its own pane, a renderer, and
    /// the glass it paints. The LEGACY lane forgets its front buffer before
    /// every paint, which is exactly the pre-`phux-esge` dirty-row painter (an
    /// unknown row is emitted whole, byte for byte as before).
    ///
    /// Each lane owns its pane, fed the same bytes, because libghostty keeps
    /// the dirty bits on the TERMINAL: two renderers walking one terminal
    /// would each consume the other's.
    struct Lane {
        pane: GhosttyTerminal<'static, 'static>,
        renderer: TerminalRenderer<'static>,
        glass: Glass,
        legacy: bool,
    }

    impl Lane {
        fn new(pane: (u16, u16), glass: (u16, u16), legacy: bool) -> Self {
            Self {
                pane: fresh(pane.0, pane.1),
                renderer: TerminalRenderer::new().expect("renderer"),
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

    /// The property: any sequence of edits, painted through the diff one frame
    /// at a time — with modals scribbled and invalidated, predictions
    /// forgotten row by row, and forced repaints after clears mixed in — leaves
    /// the glass showing exactly what the pre-diff dirty-row painter shows
    /// after the same frames, and exactly what one full repaint of the final
    /// grid shows.
    ///
    /// The second half is asserted only while the dirty-row painter itself
    /// agrees with a full repaint. It does not always: libghostty can change a
    /// cell without marking its row dirty (erasing the continuation of a wide
    /// glyph that wrapped rewrites the spacer head on the row above, and only
    /// the erased row is reported — `phux-5js7`). No painter that trusts the
    /// dirty bits can see that change, not even a forced paint through the
    /// pooled render state; it predates the diff, and the test re-syncs both
    /// lanes from fresh renderers before carrying on.
    #[test]
    fn random_edits_through_the_diff_painter_match_a_full_repaint() {
        let mut tally = Tally::default();
        for seed in 1..=48u64 {
            let mut rng = XorShift(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let dims = [(16u16, 5u16), (23, 7), (9, 3)][usize::try_from(seed % 3).unwrap_or(0)];
            // Offset panes exercise the origin arithmetic of every jump.
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
        assert!(
            tally.checked > tally.undetected * 20,
            "the full-repaint half must check almost every step: {} checked, {} skipped for \
             changes libghostty did not mark dirty",
            tally.checked,
            tally.undetected
        );
    }

    /// How many property-test steps were held to a full repaint, and how many
    /// were skipped because libghostty never reported the change dirty.
    #[derive(Debug, Default)]
    struct Tally {
        checked: usize,
        undetected: usize,
    }

    /// One seed of the property test: the diff lane, the dirty-row painter it
    /// is held to, and a reference repainted in full every step.
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

        /// Maybe disturb the glass the way the driver's other writers do.
        /// Returns whether the next paint must be forced, as it is after a
        /// clear or a modal's dismissal.
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
                    // A modal over the pane and the whole front forgotten.
                    let at = format!("\x1b[{};{}H\x1b[7m[modal]\x1b[0m", oy + row + 1, ox + 1);
                    self.scribble_both(at.as_bytes(), Some((0, rows)));
                    true
                }
                2 => {
                    // A guess over one row, that row forgotten, and the row
                    // then changed so it is visited.
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
                    // Another writer leaves a non-default pen behind (bold,
                    // underline, red bg) and touches no cell, so nothing is
                    // forgotten. The next pane paint's first span must still
                    // set its own pen rather than inherit this one.
                    self.scribble_both(b"\x1b[1;4;41m", None);
                    false
                }
                _ => false,
            }
        }

        /// Write over both painted glasses behind their renderers, then
        /// forget the rows `[start, end)` of their fronts, if given.
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

        /// Apply `edit` everywhere, paint every lane, and hold the diff lane
        /// to the dirty-row painter always and to a full repaint whenever the
        /// dirty-row painter agrees with one.
        fn step(&mut self, edit: &str, force: bool, label: &str, tally: &mut Tally) {
            let origin = self.origin;
            let region = self.dims;
            self.write_all(edit);
            let _ = self.diff.paint(origin, force);
            let _ = self.legacy.paint(origin, force);
            // A FRESH renderer each step: its render state reads every row
            // from the grid, where a pooled one trusts the dirty bits.
            self.reference.renderer = TerminalRenderer::new().expect("renderer");
            self.reference.glass.scribble(b"\x1b[2J");
            let _ = self.reference.paint(origin, true);

            let seen = self.diff.glass.seen(origin, region);
            let old = self.legacy.glass.seen(origin, region);
            let cols = region.0;
            assert_same_screen(
                &format!("{label} (against the dirty-row painter)"),
                &old,
                &seen,
                cols,
            );

            let full = self.reference.glass.seen(origin, region);
            if old != full {
                tally.undetected += 1;
                for lane in [&mut self.diff, &mut self.legacy] {
                    lane.renderer = TerminalRenderer::new().expect("renderer");
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
                cols,
            );
            let grid = format!("{label} (against the grid)");
            assert_glass_shows(&self.diff.glass, &self.diff.pane, origin, &grid);
        }
    }
}
