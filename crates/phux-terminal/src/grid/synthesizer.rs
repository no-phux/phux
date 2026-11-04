//! Synthesize `TERMINAL_SNAPSHOT` replay bytes from a `libghostty_vt::Terminal`.
//!
//! The replay (ADR-0013, SPEC §8.4) is a self-contained VT sequence that
//! reproduces the grid on a fresh terminal of the same size. Order: reset, rows with SGR
//! deltas (wide-cell tails skipped, soft wraps preserved), then cursor and
//! mode bits. OSC 8 hyperlinks are not re-emitted.

use std::cell::Cell;

use phux_core::screen::{
    CellColor, CellInfo, CellStyle, CursorState, RENDERED_FORMAT_HTML, RENDERED_FORMAT_VT,
    ROW_WINDOW_MAX, RenderedScreen, SCHEMA_VERSION, ScreenState, SemanticContent, SoftWrap,
    TRUNCATED_ROW_WINDOW,
};
use phux_protocol::{
    kitty_replay,
    render_pool::{RenderPool, RenderWalk, TerminalGeneration},
    sgr::{write_reset_and_sgr, write_reset_and_sgr_unresolved},
};

use libghostty_vt::{
    RenderState, Terminal as GhosttyTerminal,
    fmt::{Format, Formatter, FormatterOptions},
    render::{
        CellIteration, CellIterator, CursorVisualStyle, Dirty, RowIteration, RowIterator, Snapshot,
    },
    screen::{CellSemanticContent, CellWide, GridRef},
    selection::Selection,
    style::{RgbColor, Style, StyleColor},
    terminal::{Mode, Point, PointCoordinate},
};

use super::reference::{ConsumerReference, ReferenceCursorMode};

mod encoding;
mod incremental;
mod projection;
mod rendered;
mod snapshot;

#[cfg(test)]
use encoding::cup_len;
use encoding::{
    assemble_diff, emit_cell, emit_cell_bounded, emit_epilogue, emit_screen_mode,
    encode_graphemes_bounded, write_cup,
};
use projection::history_window_start;
use snapshot::{
    full_paint_hint, paint_all_rows_bounded, replay_kitty_graphics_bounded,
    write_full_paint_prologue,
};

/// `Some(0)` scrollback request sentinel: "all retained history".
pub const SCROLLBACK_ALL: u32 = 0;

/// Per-read byte budget for a `GET_SCREEN` rendered capture. Measured before
/// allocating; exceeding it refuses the request rather than truncating.
const RENDER_BUDGET_BYTES: usize = 8 * 1024 * 1024;

/// One history read: rows, their soft-wrap bits, and whether older retained
/// rows fell outside the window (ADR-0077 §§2-3).
#[derive(Debug, Default)]
struct ScrollbackWindow {
    /// History rows in the window, oldest first, right-trimmed.
    lines: Vec<String>,
    /// Indices into [`Self::lines`] whose row continues onto the next.
    wrapped: Vec<u32>,
    /// True when retained history existed above the returned window.
    truncated: bool,
}

/// Inline grapheme-cluster buffer; deeper clusters retry on the heap.
pub const GRAPHEME_INLINE: usize = 8;

/// Errors that can occur while synthesising a snapshot.
#[derive(Debug, thiserror::Error)]
pub enum SynthesisError {
    /// Surfaced from libghostty-vt.
    #[error("libghostty: {0}")]
    Ghostty(#[from] libghostty_vt::Error),
    /// A `write!` into the snapshot buffer failed.
    #[error("snapshot buffer write failed")]
    Buffer,
    /// Kitty graphics replay failed while projecting libghostty image state.
    #[error("kitty replay: {0}")]
    KittyReplay(#[from] kitty_replay::KittyReplayError),
    /// Caller-owned aggregate bootstrap byte budget was exhausted.
    #[error("snapshot byte limit exceeded")]
    LimitExceeded,
    /// Host allocation failed while reserving bounded synthesis storage.
    #[error("snapshot allocation failed")]
    OutOfMemory,
    /// The canonical terminal is on loan to a snapshot capture. Transient;
    /// the resync after the capture repaints whatever this read skipped.
    #[error("canonical terminal is on loan to a snapshot capture")]
    TerminalUnavailable,
    /// A rendered `GET_SCREEN` capture would exceed the per-read budget.
    /// Callers must surface this as `RESOURCE_EXHAUSTED`, not an empty reply.
    #[error("rendered capture would be {required} bytes, over the {budget}-byte budget")]
    RenderBudgetExceeded {
        /// The Formatter's own measured byte count for the requested
        /// selection/format.
        required: usize,
        /// The server's per-read budget the request exceeded.
        budget: usize,
    },
}

struct BoundedSnapshotBytes {
    bytes: Vec<u8>,
    max_bytes: usize,
}

impl BoundedSnapshotBytes {
    fn with_capacity(max_bytes: usize, requested: usize) -> Result<Self, SynthesisError> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(requested.min(max_bytes))
            .map_err(|_| SynthesisError::OutOfMemory)?;
        Ok(Self { bytes, max_bytes })
    }

    const fn check(&self) -> Result<(), SynthesisError> {
        if self.bytes.len() <= self.max_bytes {
            Ok(())
        } else {
            Err(SynthesisError::LimitExceeded)
        }
    }
    const fn remaining(&self) -> usize {
        self.max_bytes.saturating_sub(self.bytes.len())
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl std::io::Write for BoundedSnapshotBytes {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let remaining = self.max_bytes.saturating_sub(self.bytes.len());
        if buf.len() > remaining {
            return Err(std::io::Error::other("snapshot byte limit exceeded"));
        }
        self.bytes.try_reserve(buf.len()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::OutOfMemory,
                "snapshot allocation failed",
            )
        })?;
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Pooled per-pane snapshot scaffolding over a [`RenderPool`].
///
/// The free [`synthesize`] function is the one-shot wrapper. Per-tick diffs compare
/// rendered rows against per-consumer references rather than libghostty's
/// shared dirty bits (ADR-0086).
///
/// `prepare_tick` re-renders only the rows the pooled render state
/// rebuilt since the previous tick (its per-row dirty flags, which this type
/// alone clears), so a tick costs in proportion to damage. Any walk of the
/// same terminal through another render state drains the terminal's dirty
/// bits and would leave the pooled rows stale, so every such walk here marks
/// the pool for a rebuild (`note_foreign_walk`).
#[derive(Debug)]
pub struct SnapshotSynthesizer<'alloc> {
    /// Pooled render state + row/cell iterators, rebuilt on a geometry
    /// change (`phux-5pyx`; the rebuild now lives in [`RenderPool::begin`])
    /// or a [`Self::pool_generation`] bump.
    pool: RenderPool<'alloc>,
    /// Generation handed to [`RenderPool::begin`]; bumped to force a rebuild
    /// after a foreign render state consumed the terminal's dirty bits.
    pool_generation: TerminalGeneration,
    /// Set by any walk of a terminal through a render state other than
    /// [`Self::pool`]; the next [`Self::prepare_tick`] rebuilds the pool.
    foreign_walk: Cell<bool>,
    /// Whether [`Self::tick_rows`] holds every row of the pool's current
    /// render state, so clean rows may be kept. Cleared while a render is
    /// in flight so a failed tick falls back to a full render.
    tick_rows_valid: bool,
    /// Width [`Self::tick_rows`] was rendered at.
    tick_cols: u16,
    /// Rows the last [`Self::prepare_tick`] rendered (test observability).
    #[cfg(test)]
    last_rendered_rows: usize,
    /// Per-tick rendered row bodies, rendered by [`Self::prepare_tick`]
    /// (dirty rows only) and shared by every consumer's diff.
    tick_rows: Vec<Vec<u8>>,
    /// Cursor/mode epilogue for the current tick (consumer-independent).
    tick_epilogue: Vec<u8>,
    /// Alt-screen select bytes for the current tick; emitted only to a
    /// consumer whose reference disagrees with the live screen.
    tick_screen_toggle: Vec<u8>,
}

impl<'alloc> SnapshotSynthesizer<'alloc> {
    /// Allocate a fresh pool of render iterators. Do this once per pane.
    pub fn new() -> Result<Self, SynthesisError> {
        Ok(Self {
            pool: RenderPool::new()?,
            pool_generation: 0,
            foreign_walk: Cell::new(false),
            tick_rows_valid: false,
            tick_cols: 0,
            #[cfg(test)]
            last_rendered_rows: 0,
            tick_rows: Vec::new(),
            tick_epilogue: Vec::new(),
            tick_screen_toggle: Vec::new(),
        })
    }

    /// Emit a VT sequence that reproduces `terminal`'s viewport on a fresh
    /// terminal, plus the queried `(cols, rows)`.
    ///
    /// A full snapshot builds a fresh render state each call (phux-uow0), so
    /// it marks the tick's pool for a rebuild.
    pub fn synthesize(
        &self,
        terminal: &GhosttyTerminal<'alloc, '_>,
    ) -> Result<SnapshotBytes, SynthesisError> {
        self.note_foreign_walk();
        Self::synthesize_bounded(
            terminal,
            usize::MAX,
            phux_protocol::caps::ImageProtocolSet::all(),
        )
    }

    /// Synthesize supported image protocols without exceeding `max_bytes`.
    pub(crate) fn synthesize_bounded(
        terminal: &GhosttyTerminal<'alloc, '_>,
        max_bytes: usize,
        image_protocols: phux_protocol::caps::ImageProtocolSet,
    ) -> Result<SnapshotBytes, SynthesisError> {
        // A full snapshot must see the whole live grid, so it uses a fresh
        // render state rather than the pooled one.
        let (mut render_state, mut rows, mut cells) = fresh_render_trio()?;

        let snapshot = render_state.update(terminal)?;
        let (cols, rows_n) = grid_dims(&snapshot)?;
        let mut out = BoundedSnapshotBytes::with_capacity(
            max_bytes,
            full_paint_hint(cols, rows_n, max_bytes),
        )?;

        write_full_paint_prologue(&mut out, terminal)?;
        paint_all_rows_bounded(&mut rows, &mut cells, &snapshot, rows_n, &mut out)?;

        replay_kitty_graphics_bounded(terminal, &mut out, image_protocols)?;
        emit_epilogue(&mut out.bytes, &snapshot, terminal)?;
        out.check()?;

        Ok(SnapshotBytes {
            cols,
            rows: rows_n,
            bytes: out.into_inner(),
            scrollback: Vec::new(),
        })
    }

    /// Like [`Self::synthesize`], but also primes the client's scrollback with
    /// up to `scrollback` history rows (`None` viewport only, [`SCROLLBACK_ALL`]
    /// everything, `Some(n)` the most recent `n`).
    ///
    /// History goes into [`SnapshotBytes::scrollback`], followed by an `SU`
    /// that scrolls it off the top so the viewport replay's `ED 2` cannot
    /// erase it. The client applies `scrollback` then `bytes`.
    pub fn synthesize_with_scrollback(
        &self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        scrollback: Option<u32>,
    ) -> Result<SnapshotBytes, SynthesisError> {
        self.synthesize_with_scrollback_bounded(
            terminal,
            scrollback,
            usize::MAX,
            phux_protocol::caps::ImageProtocolSet::all(),
        )
    }

    /// Synthesize viewport and scrollback within one aggregate byte ceiling,
    /// omitting unsupported image protocols before allocating replay payloads.
    pub(crate) fn synthesize_with_scrollback_bounded(
        &self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        scrollback: Option<u32>,
        max_bytes: usize,
        image_protocols: phux_protocol::caps::ImageProtocolSet,
    ) -> Result<SnapshotBytes, SynthesisError> {
        self.note_foreign_walk();
        let mut snap = Self::synthesize_bounded(terminal, max_bytes, image_protocols)?;
        let Some(want) = scrollback else {
            return Ok(snap);
        };
        let remaining = max_bytes
            .checked_sub(snap.bytes.capacity())
            .ok_or(SynthesisError::LimitExceeded)?;
        snap.scrollback =
            Self::scrollback_styled_bytes_bounded(terminal, want, snap.rows, remaining)?;
        Ok(snap)
    }
}

/// One-shot synthesis; per-pane hot loops should reuse a
/// [`SnapshotSynthesizer`].
pub fn synthesize(terminal: &GhosttyTerminal<'_, '_>) -> Result<SnapshotBytes, SynthesisError> {
    SnapshotSynthesizer::new()?.synthesize(terminal)
}

/// Result of one snapshot synthesis: the dimensions and the VT byte body.
#[derive(Debug, Clone)]
pub struct SnapshotBytes {
    /// Grid width in cells at the moment of synthesis.
    pub cols: u16,
    /// Grid height in cells at the moment of synthesis.
    pub rows: u16,
    /// VT byte sequence; opaque, mosh-style, fed to the client's `Terminal`.
    pub bytes: Vec<u8>,
    /// Scrollback-priming VT bytes the client applies before `bytes`; empty
    /// when no scrollback was requested or none is retained.
    pub scrollback: Vec<u8>,
}

/// The active SGR pen: style plus resolved fg/bg. Colors are part of the key
/// so color-only changes between adjacent cells still emit an SGR delta.
type Pen = (Style, Option<RgbColor>, Option<RgbColor>);

/// The viewport half of a [`ScreenState`] projection.
struct ViewportProjection {
    /// Grid width in cells at the moment of the walk.
    cols: u16,
    /// Grid height in cells at the moment of the walk.
    rows: u16,
    /// Viewport-resident cursor, or `None` when it is scrolled out.
    cursor: Option<CursorState>,
    /// Plain-text rows, right-trimmed, top first.
    lines: Vec<String>,
    /// Indices into [`Self::lines`] whose row continues onto the next.
    wrapped: Vec<u32>,
    /// Sparse per-cell projection, `None` when the caller did not ask.
    cells: Option<Vec<CellInfo>>,
}

/// A fresh, unpooled render trio, for walks that must observe the whole
/// live grid (a pooled state can serve pre-resize rows).
fn fresh_render_trio<'alloc>() -> Result<
    (
        RenderState<'alloc>,
        RowIterator<'alloc>,
        CellIterator<'alloc>,
    ),
    SynthesisError,
> {
    Ok((
        RenderState::new()?,
        RowIterator::new()?,
        CellIterator::new()?,
    ))
}

/// The snapshot's `(cols, rows)` as one read.
fn grid_dims(snapshot: &Snapshot<'_, '_>) -> Result<(u16, u16), SynthesisError> {
    Ok((snapshot.cols()?, snapshot.rows()?))
}

/// Walk viewport rows top-down, stopping at the snapshot's reported height
/// (the size every consumer's mirror is built to).
fn walk_viewport_rows<'alloc, F>(
    rows: &mut RowIterator<'alloc>,
    snapshot: &Snapshot<'alloc, '_>,
    rows_n: u16,
    mut visit: F,
) -> Result<(), SynthesisError>
where
    F: FnMut(u16, &RowIteration<'alloc, '_>) -> Result<(), SynthesisError>,
{
    let mut row_iter = rows.update(snapshot)?;
    let mut row_index: u16 = 0;
    while let Some(row) = row_iter.next() {
        if row_index >= rows_n {
            break;
        }
        visit(row_index, row)?;
        row_index += 1;
    }
    Ok(())
}

/// Soft-wrap bit of one viewport row: true when it continues onto the next.
fn viewport_row_is_wrapped(row: &RowIteration<'_, '_>) -> Result<bool, SynthesisError> {
    Ok(row.raw_row()?.is_wrapped()?)
}

/// Render one row's cells into `body` with a fresh SGR pen, so the row's byte
/// sequence is self-contained and comparable regardless of its neighbours.
fn render_row_body<'alloc>(
    cells: &mut CellIterator<'alloc>,
    row: &RowIteration<'alloc, '_>,
    body: &mut Vec<u8>,
) -> Result<(), SynthesisError> {
    let mut prev_style: Option<Pen> = None;
    let mut cell_iter = cells.update(row)?;
    while let Some(cell) = cell_iter.next() {
        emit_cell(cell, body, &mut prev_style)?;
    }
    Ok(())
}

/// Mouse DEC modes the epilogue replays. [`ReferenceCursorMode`] captures
/// the same list by index; keep them in step.
pub(crate) const MOUSE_MODES: [(Mode, &[u8]); 9] = [
    // Tracking level: which events the program asked to receive.
    (Mode::X10_MOUSE, b"9"),
    (Mode::NORMAL_MOUSE, b"1000"),
    (Mode::BUTTON_MOUSE, b"1002"),
    (Mode::ANY_MOUSE, b"1003"),
    // Report encoding: how those events are framed on the way back in.
    (Mode::UTF8_MOUSE, b"1005"),
    (Mode::SGR_MOUSE, b"1006"),
    (Mode::URXVT_MOUSE, b"1015"),
    (Mode::SGR_PIXELS_MOUSE, b"1016"),
    // libghostty defaults 1007 on; an app that opted out must stay out.
    (Mode::ALT_SCROLL, b"1007"),
];

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;

#[cfg(test)]
mod differential_tests;
