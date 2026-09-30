//! Synthesize `TERMINAL_SNAPSHOT` replay bytes from a `libghostty_vt::Terminal`.
//!
//! The replay (ADR-0013, SPEC §8.4) is a self-contained VT sequence that
//! reproduces the grid on a fresh terminal of the same size. Order: reset, rows with SGR
//! deltas (wide-cell tails skipped, soft wraps preserved), then cursor and
//! mode bits. OSC 8 hyperlinks are not re-emitted.

use std::cell::Cell;
use std::io::Write as _;

use base64::Engine as _;
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
        Self::synthesize_bounded(terminal, usize::MAX)
    }

    /// Synthesize without allowing the output buffer to exceed `max_bytes`.
    pub fn synthesize_bounded(
        terminal: &GhosttyTerminal<'alloc, '_>,
        max_bytes: usize,
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

        emit_epilogue(&mut out.bytes, &snapshot, terminal)?;
        out.check()?;
        replay_kitty_graphics_bounded(terminal, &mut out, cols, rows_n)?;

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
        self.synthesize_with_scrollback_bounded(terminal, scrollback, usize::MAX)
    }

    /// Synthesize viewport and scrollback within one aggregate byte ceiling.
    pub fn synthesize_with_scrollback_bounded(
        &self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        scrollback: Option<u32>,
        max_bytes: usize,
    ) -> Result<SnapshotBytes, SynthesisError> {
        self.note_foreign_walk();
        let mut snap = Self::synthesize_bounded(terminal, max_bytes)?;
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

    /// History rows `[start, total)` as styled VT, rows joined by CRLF, then
    /// an SGR reset and an `SU` that scrolls them into the client's
    /// scrollback. Reads via `Point::History`, which never mutates the
    /// terminal.
    fn scrollback_styled_bytes_bounded(
        terminal: &GhosttyTerminal<'alloc, '_>,
        want: u32,
        viewport_rows: u16,
        max_bytes: usize,
    ) -> Result<Vec<u8>, SynthesisError> {
        let total = terminal.scrollback_rows()?;
        if total == 0 {
            return Ok(Vec::new());
        }
        let cols = terminal.cols()?;
        let start = history_window_start(total, want);
        let requested = (total - start)
            .checked_mul(usize::from(cols))
            .unwrap_or(max_bytes);
        let mut out = BoundedSnapshotBytes::with_capacity(max_bytes, requested)?;
        let mut row_count: usize = 0;
        for y in start..total {
            if row_count > 0 {
                out.write_all(b"\r\n")
                    .map_err(|_| SynthesisError::LimitExceeded)?;
            }
            emit_history_row_styled(terminal, cols, y, &mut out)?;
            row_count += 1;
        }
        if row_count == 0 {
            return Ok(Vec::new());
        }
        write_scrollback_scroll_off(&mut out, row_count, viewport_rows)?;
        Ok(out.into_inner())
    }

    /// Project the viewport into a structured [`ScreenState`] (ADR-0022 §2)
    /// for agents: text rows and cursor, no VT bytes, no side effects.
    pub fn screen_state(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        pane: u32,
    ) -> Result<ScreenState, SynthesisError> {
        self.screen_state_with_scrollback(terminal, pane, None, false)
    }

    /// Like [`Self::screen_state`], plus up to `scrollback` history rows
    /// (`None`, [`SCROLLBACK_ALL`], or `Some(n)` as in
    /// [`Self::synthesize_with_scrollback`]). History is read via
    /// `Point::History`, which neither scrolls nor mutates the terminal.
    ///
    /// With `cells`, also collects a sparse [`ScreenState::cells`] of
    /// non-default-style or OSC-133-marked cells (see `collect_cell`).
    ///
    /// Reads through the tick's pooled render state without clearing its
    /// dirty flags (see `project_viewport`), so the next tick stays
    /// incremental.
    pub fn screen_state_with_scrollback(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        pane: u32,
        scrollback: Option<u32>,
        cells: bool,
    ) -> Result<ScreenState, SynthesisError> {
        // Read history before borrowing the render state: `grid_ref`
        // references die at the next terminal operation, so copy eagerly.
        let history = match scrollback {
            None => ScrollbackWindow::default(),
            Some(want) => Self::scrollback_window(terminal, want)?,
        };

        let title = pane_title(terminal);
        let view = self.project_viewport(terminal, cells)?;

        Ok(ScreenState {
            schema_version: SCHEMA_VERSION,
            pane,
            cols: view.cols,
            rows: view.rows,
            cursor: view.cursor,
            lines: view.lines,
            scrollback: history.lines,
            cells: view.cells,
            soft_wrap: Some(SoftWrap {
                lines: view.wrapped,
                scrollback: history.wrapped,
            }),
            truncated: history.truncated,
            truncated_reason: history.truncated.then(|| TRUNCATED_ROW_WINDOW.to_owned()),
            title,
            // Filled by `Self::render_screen` when the request asks for it.
            rendered: None,
            rendered_error: None,
        })
    }

    /// Render through libghostty's Formatter for `GET_SCREEN`'s `rendered`
    /// field. `format`'s low bits select the output (`0` none, `1` HTML,
    /// `2` VT; the caller has already refused others) and the high bit
    /// joins soft-wrapped rows.
    ///
    /// A [`Selection`] is always built: without one the Formatter emits the
    /// whole scrollback, ignoring `scrollback` and the budget. History is
    /// clamped to [`ROW_WINDOW_MAX`] rows, and a capture over
    /// `RENDER_BUDGET_BYTES` is refused with
    /// [`SynthesisError::RenderBudgetExceeded`] rather than truncated.
    #[allow(
        clippy::unused_self,
        reason = "kept as a method on SnapshotSynthesizer for API symmetry \
                  with screen_state_with_scrollback, matching its own \
                  intentionally-stateless rationale"
    )]
    pub fn render_screen(
        &self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        scrollback: Option<u32>,
        format: u8,
    ) -> Result<Option<RenderedScreen>, SynthesisError> {
        Self::render_screen_with_budget(terminal, scrollback, format, RENDER_BUDGET_BYTES)
    }

    /// [`Self::render_screen`] with an explicit budget (a test seam).
    fn render_screen_with_budget(
        terminal: &GhosttyTerminal<'alloc, '_>,
        scrollback: Option<u32>,
        format: u8,
        budget: usize,
    ) -> Result<Option<RenderedScreen>, SynthesisError> {
        let selector = format & phux_protocol::wire::frame::GET_SCREEN_FORMAT_SELECTOR_MASK;
        let unwrap = format & phux_protocol::wire::frame::GET_SCREEN_FORMAT_UNWRAP != 0;
        let target = match selector {
            1 => Format::Html,
            2 => Format::Vt,
            _ => return Ok(None),
        };
        let selection = Self::render_selection(terminal, scrollback)?;
        let mut formatter = Formatter::new(
            terminal,
            FormatterOptions::new()
                .with_format(target)
                .with_trim(true)
                .with_unwrap(unwrap)
                .with_selection(&selection),
        )?;
        let required = formatter.format_len()?;
        if required > budget {
            return Err(SynthesisError::RenderBudgetExceeded { required, budget });
        }
        let bytes = formatter.format_alloc(None)?;
        let (tag, data) = if selector == 1 {
            (
                RENDERED_FORMAT_HTML,
                String::from_utf8_lossy(&bytes).into_owned(),
            )
        } else {
            (
                RENDERED_FORMAT_VT,
                base64::engine::general_purpose::STANDARD.encode(&bytes),
            )
        };
        Ok(Some(RenderedScreen {
            format: tag.to_owned(),
            data,
        }))
    }

    /// The selection [`Self::render_screen`] passes: from as far into history
    /// as `scrollback` asks (else the viewport's top-left) to the viewport's
    /// bottom-right.
    fn render_selection<'t>(
        terminal: &'t GhosttyTerminal<'alloc, '_>,
        scrollback: Option<u32>,
    ) -> Result<Selection<'t>, SynthesisError> {
        let cols = terminal.cols()?;
        let rows = terminal.rows()?;
        let end = terminal.grid_ref(Point::Viewport(PointCoordinate {
            x: cols.saturating_sub(1),
            y: u32::from(rows.saturating_sub(1)),
        }))?;
        let start = Self::render_selection_start(terminal, scrollback)?;
        Ok(Selection::new(start, end, false))
    }

    /// The start endpoint for [`Self::render_selection`], clamped to
    /// [`ROW_WINDOW_MAX`] history rows so `Some(0)` cannot hand the
    /// Formatter the whole scrollback on the single-threaded runtime.
    fn render_selection_start<'t>(
        terminal: &'t GhosttyTerminal<'alloc, '_>,
        scrollback: Option<u32>,
    ) -> Result<GridRef<'t>, SynthesisError> {
        let viewport_origin = Point::Viewport(PointCoordinate { x: 0, y: 0 });
        let Some(want) = scrollback else {
            return Ok(terminal.grid_ref(viewport_origin)?);
        };
        let total = terminal.scrollback_rows()?;
        if total == 0 {
            return Ok(terminal.grid_ref(viewport_origin)?);
        }
        let bounded_want = if want == SCROLLBACK_ALL {
            ROW_WINDOW_MAX
        } else {
            want.min(ROW_WINDOW_MAX)
        };
        let start_row = history_window_start(total, bounded_want);
        Ok(terminal.grid_ref(Point::History(PointCoordinate {
            x: 0,
            y: u32::try_from(start_row).unwrap_or(u32::MAX),
        }))?)
    }

    /// Walk the live viewport into the projection
    /// [`Self::screen_state_with_scrollback`] reports.
    ///
    /// Walks the tick's pool, which is a correct live copy: it rebuilds on a
    /// geometry change or after a foreign walk ([`Self::pool_walk_generation`]).
    /// It clears no dirty flag, row or global, so what the walk pulled from
    /// the terminal is still pending for the next [`Self::prepare_tick`]
    /// (phux-69pq.14: the agent detector's periodic scan no longer turns
    /// the next tick into a full render).
    fn project_viewport(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        cells: bool,
    ) -> Result<ViewportProjection, SynthesisError> {
        let generation = self.pool_walk_generation();
        let RenderWalk {
            snapshot,
            rows: rows_pool,
            cells: cells_pool,
        } = self.pool.begin(terminal, generation)?;
        let (cols, rows_n) = grid_dims(&snapshot)?;

        let cursor = snapshot.cursor_viewport()?.map(|c| CursorState {
            x: c.x,
            y: c.y,
            visible: snapshot.cursor_visible().unwrap_or(true),
        });

        // Only allocate the cells vec when the caller asked; the common
        // `--cells`-absent snapshot pays nothing.
        let mut cell_infos: Option<Vec<CellInfo>> = cells.then(Vec::new);

        let mut lines: Vec<String> = Vec::with_capacity(usize::from(rows_n));
        // Soft-wrap bits (ADR-0077 §2), always reported so a consumer can
        // tell "nothing wraps" from "older server".
        let mut wrapped_lines: Vec<u32> = Vec::new();
        walk_viewport_rows(rows_pool, &snapshot, rows_n, |row_index, row| {
            if viewport_row_is_wrapped(row)? {
                wrapped_lines.push(u32::from(row_index));
            }
            lines.push(project_row_text(
                cells_pool,
                row,
                cols,
                row_index,
                &mut cell_infos,
            )?);
            Ok(())
        })?;

        Ok(ViewportProjection {
            cols,
            rows: rows_n,
            cursor,
            lines,
            wrapped: wrapped_lines,
            cells: cell_infos,
        })
    }

    /// Read history rows above the viewport as right-trimmed strings, oldest
    /// first, with soft-wrap bits. `want` follows [`SCROLLBACK_ALL`]
    /// semantics. History `y = 0` is the oldest retained row.
    ///
    /// `truncated` is true exactly when older retained rows fell outside the
    /// window; it says nothing about rows libghostty already evicted.
    fn scrollback_window(
        terminal: &GhosttyTerminal<'alloc, '_>,
        want: u32,
    ) -> Result<ScrollbackWindow, SynthesisError> {
        let total = terminal.scrollback_rows()?;
        if total == 0 {
            return Ok(ScrollbackWindow::default());
        }
        let cols = terminal.cols()?;
        let start = history_window_start(total, want);

        let mut out: Vec<String> = Vec::with_capacity(total - start);
        let mut wrapped: Vec<u32> = Vec::new();
        for y in start..total {
            // `total` came from a C count; clamp rather than truncate.
            let y = u32::try_from(y).unwrap_or(u32::MAX);
            // Wrap indices are into the returned window, not into history.
            if cols > 0 && history_row_is_wrapped(terminal, y)? {
                wrapped.push(u32::try_from(out.len()).unwrap_or(u32::MAX));
            }
            out.push(history_row_text(terminal, cols, y)?);
        }
        Ok(ScrollbackWindow {
            lines: out,
            wrapped,
            truncated: start > 0,
        })
    }

    /// Synthesize one consumer's incremental diff against its own
    /// [`ConsumerReference`] (phux-ia4).
    ///
    /// libghostty's `RenderState::update` consumes the shared terminal's dirty
    /// bits, so with N consumers on one pane only the first would see changes.
    /// This compares rendered row bodies against the per-consumer reference
    /// instead, so each consumer gets a correct diff regardless of the others.
    ///
    /// A non-empty diff advances `reference` before returning (emit-once). An
    /// empty body means the viewport is byte-identical to the reference.
    pub fn synthesize_against_reference(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        reference: &mut ConsumerReference,
    ) -> Result<SnapshotBytes, SynthesisError> {
        // Single-consumer wrapper; `tick_emit` calls `prepare_tick` +
        // `diff_consumer` directly so N consumers share one render.
        let (cols, rows_n, live_cm) = self.prepare_tick(terminal)?;
        Ok(self.diff_consumer(cols, rows_n, live_cm, reference))
    }

    /// Serve several consumers from one render, the way the state-sync tick
    /// does: one `prepare_tick`, then one `diff_consumer`
    /// per reference, in order. Each non-empty diff advances its reference.
    ///
    /// # Errors
    ///
    /// As [`Self::synthesize_against_reference`].
    pub fn synthesize_tick(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        references: &mut [ConsumerReference],
    ) -> Result<Vec<SnapshotBytes>, SynthesisError> {
        let (cols, rows_n, live_cm) = self.prepare_tick(terminal)?;
        Ok(references
            .iter_mut()
            .map(|reference| self.diff_consumer(cols, rows_n, live_cm, reference))
            .collect())
    }

    /// Refresh informational metadata through the same render cache as ticks.
    /// A separate `RenderState` would consume canonical dirty bits and leave this
    /// pool's row bodies stale when an ACK arrives between PTY output and a tick.
    pub(crate) fn metadata_snapshot(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
    ) -> Result<Snapshot<'alloc, '_>, SynthesisError> {
        let generation = self.pool_walk_generation();
        Ok(self.pool.begin(terminal, generation)?.snapshot)
    }

    /// The generation every pooled walk passes to [`RenderPool::begin`],
    /// bumped first (forcing a rebuild) if a foreign walk drained the
    /// terminal's dirty bits since the last pooled walk. Every pooled reader
    /// goes through here so none serves stale rows; only
    /// [`Self::prepare_tick`] clears the dirty flags a walk accumulates.
    const fn pool_walk_generation(&mut self) -> TerminalGeneration {
        if self.foreign_walk.replace(false) {
            self.pool_generation = self.pool_generation.wrapping_add(1);
        }
        self.pool_generation
    }

    /// Render the grid once per tick into the shared `tick_*` buffers and
    /// return `(cols, rows, live_cm)`. Each consumer's
    /// [`Self::diff_consumer`] then diffs against them, so N consumers cost
    /// one render and one set of cursor/mode FFI reads.
    ///
    /// Only rows the pooled render state rebuilt since the last tick are
    /// re-rendered; the rest keep last tick's bodies, which are byte-identical
    /// because a clean row's cached cells are unchanged. A full render runs
    /// instead whenever that cannot be trusted: the first tick, a pool rebuild
    /// (geometry change, or a foreign walk drained the dirty bits), a
    /// libghostty full redraw (screen switch, viewport move, palette or other
    /// terminal-wide change), or a previous tick that failed midway.
    pub(crate) fn prepare_tick(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
    ) -> Result<(u16, u16, ReferenceCursorMode), SynthesisError> {
        let span = tracing::debug_span!(
            "prepare_tick",
            full = tracing::field::Empty,
            rendered_rows = tracing::field::Empty,
        )
        .entered();
        let generation = self.pool_walk_generation();
        let RenderWalk {
            snapshot,
            rows,
            cells,
        } = self.pool.begin(terminal, generation)?;
        let (cols, rows_n) = grid_dims(&snapshot)?;
        let dirty = snapshot.dirty()?;
        let rows_usize = usize::from(rows_n);
        let full = !self.tick_rows_valid
            || dirty == Dirty::Full
            || self.tick_cols != cols
            || self.tick_rows.len() != rows_usize;
        span.record("full", full);

        // Invalid until every dirty row is rendered and its flag cleared: an
        // error below leaves a partial state the next tick must not trust.
        self.tick_rows_valid = false;
        if full {
            // Clear every in-range buffer so a row the iterator skips cannot
            // leave stale content from a prior tick.
            self.tick_rows.resize_with(rows_usize, Vec::new);
            for body in &mut self.tick_rows {
                body.clear();
            }
        }
        let mut rendered_rows: usize = 0;
        if full || dirty != Dirty::Clean {
            // Fresh pen per row keeps each row body self-contained and
            // comparable across ticks.
            let tick_rows = &mut self.tick_rows;
            walk_viewport_rows(rows, &snapshot, rows_n, |row_index, row| {
                if !full && !row.dirty()? {
                    return Ok(());
                }
                let body = &mut tick_rows[usize::from(row_index)];
                body.clear();
                render_row_body(cells, row, body)?;
                row.set_dirty(false)?;
                rendered_rows += 1;
                Ok(())
            })?;
        }
        span.record("rendered_rows", rendered_rows);
        #[cfg(test)]
        {
            self.last_rendered_rows = rendered_rows;
        }
        // The global flag is independent of the row flags cleared above.
        snapshot.set_dirty(Dirty::Clean)?;
        self.tick_cols = cols;
        self.tick_rows_valid = true;

        // Cursor/mode + epilogue + screen toggle: consumer-independent, so
        // capture/precompute them once while the snapshot is live.
        let live_cm = ReferenceCursorMode::capture(&snapshot, terminal)?;
        self.tick_epilogue.clear();
        emit_epilogue(&mut self.tick_epilogue, &snapshot, terminal)?;
        self.tick_screen_toggle.clear();
        emit_screen_mode(&mut self.tick_screen_toggle, terminal)?;
        Ok((cols, rows_n, live_cm))
    }

    /// Record that `terminal` was walked through a render state other than
    /// the pool. That walk drained the terminal's dirty bits, so the pool's
    /// cached rows may be stale; the next [`Self::prepare_tick`] rebuilds the
    /// pool and renders in full.
    pub(crate) fn note_foreign_walk(&self) {
        self.foreign_walk.set(true);
    }

    /// Diff one consumer against the shared tick buffers from
    /// [`Self::prepare_tick`], advancing its reference (emit-once). An empty
    /// body means the consumer is already current.
    pub(crate) fn diff_consumer(
        &self,
        cols: u16,
        rows_n: u16,
        live_cm: ReferenceCursorMode,
        reference: &mut ConsumerReference,
    ) -> SnapshotBytes {
        let span = tracing::debug_span!(
            "diff_consumer",
            changed_row_count = tracing::field::Empty,
            out_bytes = tracing::field::Empty,
        )
        .entered();
        // A dimension change clears the reference so every row repaints.
        if reference.cols != cols || reference.rows != rows_n {
            reference.reset_geometry(cols, rows_n);
        }

        // Clone changed rows into the reference: `tick_rows` is shared with
        // the other consumers this tick.
        {
            let ConsumerReference {
                rows_body,
                changed_scratch: changed,
                ..
            } = &mut *reference;
            changed.clear();
            for (idx, (rendered, stored)) in
                self.tick_rows.iter().zip(rows_body.iter_mut()).enumerate()
            {
                if *stored != *rendered {
                    stored.clear();
                    stored.extend_from_slice(rendered);
                    changed.push(u16::try_from(idx).unwrap_or(u16::MAX));
                }
            }
        }

        let cursor_mode_changed = reference.cursor_mode != live_cm;
        if reference.changed_scratch.is_empty() && !cursor_mode_changed {
            span.record("changed_row_count", 0_usize);
            span.record("out_bytes", 0_usize);
            return SnapshotBytes {
                cols,
                rows: rows_n,
                bytes: Vec::new(),
                scrollback: Vec::new(),
            };
        }
        let changed_row_count = reference.changed_scratch.len();

        // Screen toggle first, and only on this consumer's alt-screen
        // transition, so content lands on the right buffer.
        let toggle: &[u8] = if reference.cursor_mode.alt_screen_set() == live_cm.alt_screen_set() {
            &[]
        } else {
            &self.tick_screen_toggle
        };
        let out = assemble_diff(
            toggle,
            &reference.changed_scratch,
            &reference.rows_body,
            &self.tick_epilogue,
        );
        reference.cursor_mode = live_cm;

        span.record("changed_row_count", changed_row_count);
        span.record("out_bytes", out.len());
        SnapshotBytes {
            cols,
            rows: rows_n,
            bytes: out,
            scrollback: Vec::new(),
        }
    }

    /// Diff the current tick against `base` without advancing it (ADR-0042).
    /// `base` is the consumer's last-acked reference, so a dropped frame
    /// self-heals: its rows still differ from `base` next tick. Each changed
    /// row is repainted in full, making the delta idempotent. Requires a
    /// preceding [`Self::prepare_tick`]; the caller advances `base` via
    /// [`Self::snapshot_tick_reference`] when a `FRAME_ACK` lands.
    pub(crate) fn diff_against_base(
        &self,
        cols: u16,
        rows_n: u16,
        live_cm: ReferenceCursorMode,
        base: &ConsumerReference,
    ) -> Vec<u8> {
        let span = tracing::debug_span!(
            "diff_against_base",
            changed_row_count = tracing::field::Empty,
            out_bytes = tracing::field::Empty,
        )
        .entered();
        let geometry_mismatch = base.cols != cols || base.rows != rows_n;
        let screen_changed = base.cursor_mode.alt_screen_set() != live_cm.alt_screen_set();
        let cursor_mode_changed = base.cursor_mode != live_cm;

        // Rows that differ from the acked reference, or all rows on a
        // geometry mismatch.
        let mut changed: Vec<u16> = Vec::new();
        for (idx, rendered) in self.tick_rows.iter().enumerate() {
            let differs = geometry_mismatch
                || base
                    .rows_body
                    .get(idx)
                    .is_none_or(|stored| stored != rendered);
            if differs {
                changed.push(u16::try_from(idx).unwrap_or(u16::MAX));
            }
        }

        if changed.is_empty() && !cursor_mode_changed {
            span.record("changed_row_count", 0_usize);
            span.record("out_bytes", 0_usize);
            return Vec::new();
        }

        let toggle: &[u8] = if screen_changed {
            &self.tick_screen_toggle
        } else {
            &[]
        };
        let out = assemble_diff(toggle, &changed, &self.tick_rows, &self.tick_epilogue);
        span.record("changed_row_count", changed.len());
        span.record("out_bytes", out.len());
        out
    }

    /// Snapshot the current tick's rendered grid as a standalone
    /// [`ConsumerReference`], the diff base once the matching ack lands
    /// (ADR-0042). Requires a preceding [`Self::prepare_tick`].
    pub(crate) fn snapshot_tick_reference(
        &self,
        cols: u16,
        rows_n: u16,
        live_cm: ReferenceCursorMode,
    ) -> ConsumerReference {
        let mut reference = ConsumerReference::new();
        reference.cols = cols;
        reference.rows = rows_n;
        reference.rows_body.clone_from(&self.tick_rows);
        reference.cursor_mode = live_cm;
        reference
    }

    /// Prime `reference` to the current `terminal` state without emitting
    /// bytes, so the first diff after attach reports only later changes.
    #[allow(
        clippy::needless_pass_by_ref_mut,
        reason = "`&mut self` is retained for semver compatibility of this externally visible method"
    )]
    pub fn prime_reference(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        reference: &mut ConsumerReference,
    ) -> Result<(), SynthesisError> {
        // The attach snapshot just walked with a fresh render state and
        // consumed the terminal's dirty bits, so the pool may hold older
        // rows. Prime from another fresh walk to match the snapshot's cut;
        // the next tick rebuilds the pool for the same reason.
        self.note_foreign_walk();
        let (mut render_state, mut rows, mut cells) = fresh_render_trio()?;
        let snapshot = render_state.update(terminal)?;
        let (cols, rows_n) = grid_dims(&snapshot)?;
        reference.reset_geometry(cols, rows_n);

        walk_viewport_rows(&mut rows, &snapshot, rows_n, |row_index, row| {
            let mut body: Vec<u8> = Vec::with_capacity(usize::from(cols));
            render_row_body(&mut cells, row, &mut body)?;
            reference.rows_body[usize::from(row_index)] = body;
            Ok(())
        })?;
        reference.cursor_mode = ReferenceCursorMode::capture(&snapshot, terminal)?;
        Ok(())
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

/// Capacity hint for a full bounded paint: two bytes per cell, falling back to
/// the ceiling itself when that product overflows.
fn full_paint_hint(cols: u16, rows_n: u16, max_bytes: usize) -> usize {
    usize::from(cols)
        .checked_mul(usize::from(rows_n))
        .and_then(|cells| cells.checked_mul(2))
        .unwrap_or(max_bytes)
}

/// Reset (`DECSTR + ED 2 + CUP home`) then select the screen buffer, which
/// must precede any cell bytes (see [`emit_screen_mode`]).
fn write_full_paint_prologue(
    out: &mut BoundedSnapshotBytes,
    terminal: &GhosttyTerminal<'_, '_>,
) -> Result<(), SynthesisError> {
    out.write_all(b"\x1b[!p\x1b[2J\x1b[H")
        .map_err(|_| SynthesisError::LimitExceeded)?;
    emit_screen_mode(&mut out.bytes, terminal)?;
    out.check()
}

/// `CUP` to each viewport row and emit its cells within the byte ceiling.
fn paint_all_rows_bounded<'alloc>(
    rows: &mut RowIterator<'alloc>,
    cells: &mut CellIterator<'alloc>,
    snapshot: &Snapshot<'alloc, '_>,
    rows_n: u16,
    out: &mut BoundedSnapshotBytes,
) -> Result<(), SynthesisError> {
    let mut prev_style: Option<Pen> = None;
    walk_viewport_rows(rows, snapshot, rows_n, |row_index, row| {
        write_cup(&mut out.bytes, row_index, 0);
        out.check()?;
        let mut cell_iter = cells.update(row)?;
        while let Some(cell) = cell_iter.next() {
            emit_cell_bounded(cell, out, &mut prev_style)?;
        }
        Ok(())
    })
}

/// Replay the pane's kitty graphics placements into the bounded buffer.
fn replay_kitty_graphics_bounded(
    terminal: &GhosttyTerminal<'_, '_>,
    out: &mut BoundedSnapshotBytes,
    cols: u16,
    rows_n: u16,
) -> Result<(), SynthesisError> {
    let mut kitty_placements = libghostty_vt::kitty::graphics::PlacementIterator::new()?;
    let _ = kitty_replay::emit_kitty_graphics_replay(
        terminal,
        &mut kitty_placements,
        out,
        (0, 0),
        (cols, rows_n),
    )?;
    Ok(())
}

/// The pane's OSC 0/2 title, or `None` when unset (never `Some("")`).
fn pane_title(terminal: &GhosttyTerminal<'_, '_>) -> Option<String> {
    terminal
        .title()
        .ok()
        .filter(|t| !t.is_empty())
        .map(ToOwned::to_owned)
}

/// Project one viewport row to its right-trimmed plain text, recording each
/// cell's [`CellInfo`] into `cell_infos` when the caller asked for cells.
fn project_row_text<'alloc>(
    cells_pool: &mut CellIterator<'alloc>,
    row: &RowIteration<'alloc, '_>,
    cols: u16,
    row_index: u16,
    cell_infos: &mut Option<Vec<CellInfo>>,
) -> Result<String, SynthesisError> {
    let mut buf = String::with_capacity(usize::from(cols));
    let mut col_index: u16 = 0;
    let mut cell_iter = cells_pool.update(row)?;
    while let Some(cell) = cell_iter.next() {
        let wide = cell.raw_cell()?.wide()?;
        if matches!(wide, CellWide::SpacerTail) {
            // Wide-cell tail: the base glyph already covers this column.
            continue;
        }
        record_cell_info(cell_infos, cell, row_index, col_index)?;
        append_cell_text(&mut buf, cell)?;
        // Advance by display width so columns match the grid (and cursor.x).
        col_index = col_index.saturating_add(if matches!(wide, CellWide::Wide) { 2 } else { 1 });
    }
    Ok(buf.trim_end().to_owned())
}

/// Record this cell's sparse [`CellInfo`] projection, if the caller asked for
/// cells and the cell carries a style or semantic mark worth reporting.
fn record_cell_info(
    cell_infos: &mut Option<Vec<CellInfo>>,
    cell: &CellIteration<'_, '_>,
    row_index: u16,
    col_index: u16,
) -> Result<(), SynthesisError> {
    let Some(infos) = cell_infos.as_mut() else {
        return Ok(());
    };
    if let Some(info) = collect_cell(cell, row_index, col_index)? {
        infos.push(info);
    }
    Ok(())
}

/// Append the cell's grapheme cluster to `buf`, or a space for a blank cell.
fn append_cell_text(buf: &mut String, cell: &CellIteration<'_, '_>) -> Result<(), SynthesisError> {
    let graphemes = cell.graphemes()?;
    if graphemes.is_empty() {
        buf.push(' ');
    } else {
        buf.extend(graphemes);
    }
    Ok(())
}

/// Start of the history window: the `want` rows nearest the viewport.
fn history_window_start(total: usize, want: u32) -> usize {
    if want == SCROLLBACK_ALL {
        0
    } else {
        total.saturating_sub(usize::try_from(want).unwrap_or(usize::MAX))
    }
}

/// Soft-wrap bit of one history row: true when it continues onto the next.
fn history_row_is_wrapped(
    terminal: &GhosttyTerminal<'_, '_>,
    y: u32,
) -> Result<bool, SynthesisError> {
    let head = Point::History(PointCoordinate { x: 0, y });
    Ok(terminal.grid_ref(head)?.row()?.is_wrapped()?)
}

/// Read one history row into right-trimmed plain text, skipping wide-cell
/// tails (their base glyph already claimed both columns).
fn history_row_text(
    terminal: &GhosttyTerminal<'_, '_>,
    cols: u16,
    y: u32,
) -> Result<String, SynthesisError> {
    let mut buf = String::with_capacity(usize::from(cols));
    for x in 0..cols {
        let point = Point::History(PointCoordinate { x, y });
        let grid_ref = terminal.grid_ref(point)?;
        if matches!(grid_ref.cell()?.wide()?, CellWide::SpacerTail) {
            continue;
        }
        append_history_grapheme(&mut buf, &grid_ref)?;
    }
    Ok(buf.trim_end().to_owned())
}

/// Append a history cell's grapheme cluster (a space for a blank cell),
/// retrying on the heap for clusters deeper than the inline buffer.
fn append_history_grapheme(buf: &mut String, grid_ref: &GridRef<'_>) -> Result<(), SynthesisError> {
    let mut inline = [char::from(0u8); GRAPHEME_INLINE];
    match grid_ref.graphemes(&mut inline) {
        Ok(0) => buf.push(' '),
        Ok(n) => buf.extend(&inline[..n]),
        Err(libghostty_vt::Error::OutOfSpace { required }) => {
            let mut heap = vec![char::from(0u8); required];
            let n = grid_ref.graphemes(&mut heap)?;
            buf.extend(&heap[..n]);
        }
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

/// Emit one history row as styled VT. The pen restarts every row.
fn emit_history_row_styled(
    terminal: &GhosttyTerminal<'_, '_>,
    cols: u16,
    y: usize,
    out: &mut BoundedSnapshotBytes,
) -> Result<(), SynthesisError> {
    // History `y` is a `u32` in libghostty's coordinate space; clamp
    // defensively rather than truncate.
    let y = u32::try_from(y).unwrap_or(u32::MAX);
    let mut prev_style: Option<Style> = None;
    for x in 0..cols {
        let point = Point::History(PointCoordinate { x, y });
        let grid_ref = terminal.grid_ref(point)?;
        if matches!(grid_ref.cell()?.wide()?, CellWide::SpacerTail) {
            continue;
        }
        let style = grid_ref.style()?;
        if prev_style.as_ref() != Some(&style) {
            write_reset_and_sgr_unresolved(&mut out.bytes, &style);
            out.check()?;
            prev_style = Some(style);
        }
        write_history_grapheme_bounded(&grid_ref, out)?;
    }
    Ok(())
}

/// Write a history cell's grapheme (or a space) into the bounded buffer.
/// The heap retry is charged against the budget before allocating.
fn write_history_grapheme_bounded(
    grid_ref: &GridRef<'_>,
    out: &mut BoundedSnapshotBytes,
) -> Result<(), SynthesisError> {
    let mut inline = [char::from(0u8); GRAPHEME_INLINE];
    match grid_ref.graphemes(&mut inline) {
        Ok(0) => out
            .write_all(b" ")
            .map_err(|_| SynthesisError::LimitExceeded)?,
        Ok(n) => encode_graphemes_bounded(out, &inline[..n])?,
        Err(libghostty_vt::Error::OutOfSpace { required }) => {
            let allocation_bytes = required
                .checked_mul(std::mem::size_of::<char>())
                .ok_or(SynthesisError::LimitExceeded)?;
            if allocation_bytes > out.remaining() {
                return Err(SynthesisError::LimitExceeded);
            }
            let mut heap = Vec::new();
            heap.try_reserve_exact(required)
                .map_err(|_| SynthesisError::OutOfMemory)?;
            heap.resize(required, char::from(0_u8));
            let n = grid_ref.graphemes(&mut heap)?;
            encode_graphemes_bounded(out, &heap[..n])?;
        }
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

/// End the scrollback prologue: SGR reset, then an `SU` scrolling history
/// into the client's scrollback.
fn write_scrollback_scroll_off(
    out: &mut BoundedSnapshotBytes,
    row_count: usize,
    viewport_rows: u16,
) -> Result<(), SynthesisError> {
    out.write_all(b"\x1b[0m")
        .map_err(|_| SynthesisError::LimitExceeded)?;
    let visible = u16::try_from(row_count)
        .unwrap_or(viewport_rows)
        .min(viewport_rows);
    if visible > 0 {
        write!(out, "\x1b[{visible}S").map_err(|_| SynthesisError::LimitExceeded)?;
    }
    Ok(())
}

/// Emit one cell: SGR delta when the pen changes, then its grapheme (or a
/// space); wide-cell tails emit nothing.
fn emit_cell_bounded(
    cell: &CellIteration<'_, '_>,
    out: &mut BoundedSnapshotBytes,
    prev: &mut Option<Pen>,
) -> Result<(), SynthesisError> {
    if matches!(cell.raw_cell()?.wide()?, CellWide::SpacerTail) {
        return Ok(());
    }
    let len = cell.graphemes_len()?;
    apply_pen_bounded(cell, out, prev)?;
    if len == 0 {
        out.write_all(b" ")
            .map_err(|_| SynthesisError::LimitExceeded)?;
        return Ok(());
    }
    write_cell_graphemes_bounded(cell, out, len)
}

/// Reproduce the cell's pen (attributes + resolved fg/bg) into `out` whenever
/// it differs from `prev`, then adopt it as the new active pen.
fn apply_pen_bounded(
    cell: &CellIteration<'_, '_>,
    out: &mut BoundedSnapshotBytes,
    prev: &mut Option<Pen>,
) -> Result<(), SynthesisError> {
    let style = cell.style()?;
    let fg = cell.fg_color()?;
    let bg = cell.bg_color()?;
    let pen = (style, fg, bg);
    if prev.as_ref() != Some(&pen) {
        write_reset_and_sgr(&mut out.bytes, &style, fg, bg);
        out.check()?;
        *prev = Some(pen);
    }
    Ok(())
}

/// Write a cell's grapheme cluster, via the inline buffer when it fits. The
/// worst case is charged against the budget before reading.
fn write_cell_graphemes_bounded(
    cell: &CellIteration<'_, '_>,
    out: &mut BoundedSnapshotBytes,
    len: usize,
) -> Result<(), SynthesisError> {
    let allocation_bytes = len
        .checked_mul(std::mem::size_of::<char>())
        .ok_or(SynthesisError::LimitExceeded)?;
    if allocation_bytes > out.remaining() {
        return Err(SynthesisError::LimitExceeded);
    }
    let mut inline = [char::from(0u8); GRAPHEME_INLINE];
    if len <= GRAPHEME_INLINE {
        cell.graphemes_buf(&mut inline[..len])?;
        encode_graphemes_bounded(out, &inline[..len])?;
    } else {
        let mut heap = Vec::new();
        heap.try_reserve_exact(len)
            .map_err(|_| SynthesisError::OutOfMemory)?;
        heap.resize(len, char::from(0_u8));
        cell.graphemes_buf(&mut heap)?;
        encode_graphemes_bounded(out, &heap)?;
    }
    Ok(())
}

fn encode_graphemes_bounded(
    out: &mut BoundedSnapshotBytes,
    graphemes: &[char],
) -> Result<(), SynthesisError> {
    for ch in graphemes {
        let mut buf = [0_u8; 4];
        out.write_all(ch.encode_utf8(&mut buf).as_bytes())
            .map_err(|_| SynthesisError::LimitExceeded)?;
    }
    Ok(())
}
fn emit_cell(
    cell: &CellIteration<'_, '_>,
    out: &mut Vec<u8>,
    prev: &mut Option<Pen>,
) -> Result<(), SynthesisError> {
    // A wide glyph's tail must not emit a space: that would clobber the
    // glyph's right half.
    let wide = cell.raw_cell()?.wide()?;
    if matches!(wide, CellWide::SpacerTail) {
        return Ok(());
    }

    let len = cell.graphemes_len()?;

    // Pen before glyph, blank cells included: a colored blank cell must keep
    // its background.
    let style = cell.style()?;
    let fg = cell.fg_color()?;
    let bg = cell.bg_color()?;
    let pen = (style, fg, bg);
    if prev.as_ref() != Some(&pen) {
        write_reset_and_sgr(out, &style, fg, bg);
        *prev = Some(pen);
    }

    if len == 0 {
        // Genuinely blank cell — emit a space so the column advances and the
        // background set above fills it. (Wide-tail case was handled above.)
        out.push(b' ');
        return Ok(());
    }

    // Stack buffer: a heap allocation per cell dominated the hot path.
    let mut inline = [char::from(0u8); GRAPHEME_INLINE];
    if len <= GRAPHEME_INLINE {
        cell.graphemes_buf(&mut inline[..len])?;
        encode_graphemes(out, &inline[..len]);
    } else {
        let mut heap = vec![char::from(0u8); len];
        cell.graphemes_buf(&mut heap)?;
        encode_graphemes(out, &heap);
    }
    Ok(())
}

/// UTF-8 encode a grapheme cluster's codepoints into `out`.
fn encode_graphemes(out: &mut Vec<u8>, graphemes: &[char]) {
    for ch in graphemes {
        let mut buf = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    }
}

/// Project a viewport cell into a [`CellInfo`], or `None` for a plain cell
/// (no non-default style, no OSC-133 mark), keeping [`ScreenState::cells`]
/// sparse. Wide-cell tails are skipped by the caller.
///
/// Two sibling projections live in the TUI render path and `phux-record`;
/// `cell_projection_conformance.rs` keeps the three in agreement.
fn collect_cell(
    cell: &CellIteration<'_, '_>,
    row: u16,
    col: u16,
) -> Result<Option<CellInfo>, SynthesisError> {
    let style = cell.style()?;
    // libghostty defaults every cell to `Output`, so only `Input`/`Prompt`
    // carry information.
    let semantic = match cell.raw_cell()?.semantic_content()? {
        CellSemanticContent::Output => None,
        CellSemanticContent::Input => Some(SemanticContent::Input),
        CellSemanticContent::Prompt => Some(SemanticContent::Prompt),
    };

    // Resolved colors first; fall back to the raw color so a palette index
    // survives as a palette index.
    let fg = cell_color(cell.fg_color()?, style.fg_color);
    let bg = cell_color(cell.bg_color()?, style.bg_color);

    let cell_style = CellStyle {
        bold: style.bold,
        faint: style.faint,
        italic: style.italic,
        underline: !matches!(style.underline, libghostty_vt::style::Underline::None),
        blink: style.blink,
        inverse: style.inverse,
        invisible: style.invisible,
        strikethrough: style.strikethrough,
        overline: style.overline,
        fg,
        bg,
    };

    // Sparse: drop cells that carry nothing an agent could act on.
    if semantic.is_none() && cell_style == DEFAULT_CELL_STYLE {
        return Ok(None);
    }

    Ok(Some(CellInfo {
        col,
        row,
        semantic,
        style: cell_style,
    }))
}

/// The all-off, all-default [`CellStyle`] — the sentinel `collect_cell`
/// compares against to keep the cells projection sparse.
const DEFAULT_CELL_STYLE: CellStyle = CellStyle {
    bold: false,
    faint: false,
    italic: false,
    underline: false,
    blink: false,
    inverse: false,
    invisible: false,
    strikethrough: false,
    overline: false,
    fg: CellColor::Default,
    bg: CellColor::Default,
};

/// Prefer the cell's explicit color (palette indices keep their identity),
/// then a resolved RGB, else [`CellColor::Default`].
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

/// Epilogue shared by snapshots and diffs: SGR reset, cursor position,
/// visibility, style, and the load-bearing mode bits.
///
/// Known, self-healing gaps kept on purpose: the client pen may differ from
/// the server's right after the snapshot, and an off-viewport cursor homes
/// to `ESC[H`. Both reconverge on the next live frame.
fn emit_epilogue(
    out: &mut Vec<u8>,
    snapshot: &Snapshot<'_, '_>,
    terminal: &GhosttyTerminal<'_, '_>,
) -> Result<(), SynthesisError> {
    // Reset SGR before cursor placement so the cursor's visual style
    // isn't tainted by the last cell's attributes.
    out.extend_from_slice(b"\x1b[0m");

    // Cursor position.
    if let Some(viewport) = snapshot.cursor_viewport()? {
        write_cup(out, viewport.y, viewport.x);
    } else {
        // No viewport-resident cursor; leave at home.
        out.extend_from_slice(b"\x1b[H");
    }

    // Cursor visibility + visual style.
    if snapshot.cursor_visible()? {
        out.extend_from_slice(b"\x1b[?25h");
    } else {
        out.extend_from_slice(b"\x1b[?25l");
    }
    emit_cursor_style(
        out,
        snapshot.cursor_visual_style()?,
        snapshot.cursor_blinking()?,
    );

    // Alt-screen modes are emitted before the row paint
    // (see [`emit_screen_mode`]), not here.
    emit_mode(out, terminal, Mode::BRACKETED_PASTE, b"2004")?;
    emit_mode(out, terminal, Mode::FOCUS_EVENT, b"1004")?;
    emit_mouse_modes(out, terminal)?;
    Ok(())
}

/// Emit the mouse-reporting DEC modes (tracking level, encoding, and 1007
/// wheel policy). The client routes the wheel from its mirror's mode
/// state, so omitting these after a snapshot turns wheel scrolling in
/// mouse-tracking TUIs into arrow keys.
fn emit_mouse_modes(
    out: &mut Vec<u8>,
    terminal: &GhosttyTerminal<'_, '_>,
) -> Result<(), SynthesisError> {
    for (mode, code) in MOUSE_MODES {
        emit_mode(out, terminal, mode, code)?;
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

/// Emit the alt-screen modes (47 / 1047 / 1049), each queried separately.
///
/// Must precede the row paint and cursor restore: `?1049h` clears the alt
/// buffer and saves the cursor on entry.
fn emit_screen_mode(
    out: &mut Vec<u8>,
    terminal: &GhosttyTerminal<'_, '_>,
) -> Result<(), SynthesisError> {
    emit_mode(out, terminal, Mode::ALT_SCREEN_LEGACY, b"47")?;
    emit_mode(out, terminal, Mode::ALT_SCREEN, b"1047")?;
    emit_mode(out, terminal, Mode::ALT_SCREEN_SAVE, b"1049")?;
    Ok(())
}

/// 1-based CUP (`CSI <r+1>;<c+1> H`). Inputs are zero-based.
fn write_cup(out: &mut Vec<u8>, row: u16, col: u16) {
    let r = row.saturating_add(1);
    let c = col.saturating_add(1);
    let _ = write!(out, "\x1b[{r};{c}H");
}

/// Encoded length of [`write_cup`]'s `ESC [ r ; c H`.
fn cup_len(row: u16, col: u16) -> usize {
    let digits = |n: u16| n.checked_ilog10().map_or(1, |log| log as usize + 1);
    4 + digits(row.saturating_add(1)) + digits(col.saturating_add(1))
}

/// SGR reset opening every emitted diff row, so a row body (which starts
/// from a fresh pen) lands on a clean pen.
const ROW_PEN_RESET: &[u8] = b"\x1b[0m";

/// Assemble one diff: `toggle`, then each `changed` row of `bodies` as
/// `CUP + pen reset + body`, then the cursor/mode `epilogue` (always
/// re-emitted: painting rows moved the cursor). The buffer is sized exactly
/// up front, so it never regrows and becomes `Bytes` without a copy.
fn assemble_diff(toggle: &[u8], changed: &[u16], bodies: &[Vec<u8>], epilogue: &[u8]) -> Vec<u8> {
    let rows_len: usize = changed
        .iter()
        .map(|&ri| cup_len(ri, 0) + ROW_PEN_RESET.len() + bodies[usize::from(ri)].len())
        .sum();
    let len = toggle.len() + rows_len + epilogue.len();
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(toggle);
    for &ri in changed {
        write_cup(&mut out, ri, 0);
        out.extend_from_slice(ROW_PEN_RESET);
        out.extend_from_slice(&bodies[usize::from(ri)]);
    }
    out.extend_from_slice(epilogue);
    debug_assert_eq!(out.len(), len, "diff length was exact");
    out
}

fn emit_cursor_style(out: &mut Vec<u8>, style: CursorVisualStyle, blinking: bool) {
    // DECSCUSR `CSI <n> SP q`: 1/2 block, 3/4 underline, 5/6 bar
    // (blinking/steady). BlockHollow has no encoding; use steady block.
    let code: u8 = match (style, blinking) {
        (CursorVisualStyle::Block, true) => 1,
        (CursorVisualStyle::Underline, true) => 3,
        (CursorVisualStyle::Underline, false) => 4,
        (CursorVisualStyle::Bar, true) => 5,
        (CursorVisualStyle::Bar, false) => 6,
        // Steady block, hollow block, and any future variant — treat as steady block.
        _ => 2,
    };
    let _ = write!(out, "\x1b[{code} q");
}

/// Query `mode` on `terminal`; emit `CSI ? <code> h/l` accordingly.
fn emit_mode(
    out: &mut Vec<u8>,
    terminal: &GhosttyTerminal<'_, '_>,
    mode: Mode,
    code: &[u8],
) -> Result<(), SynthesisError> {
    let on = terminal.mode(mode)?;
    out.extend_from_slice(b"\x1b[?");
    out.extend_from_slice(code);
    out.push(if on { b'h' } else { b'l' });
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use libghostty_vt::Terminal as GhosttyTerminal;

    fn fresh(cols: u16, rows: u16) -> GhosttyTerminal<'static, 'static> {
        {
            let mut terminal = GhosttyTerminal::new(cols, rows).expect("Terminal::new");
            terminal
                .set_scrollback_max_lines(Some(100))
                .expect("Terminal::new");
            terminal
        }
    }

    /// `cup_len` matches what `write_cup` writes at every digit boundary, so
    /// a diff buffer sized from it is exact.
    #[test]
    fn cup_len_matches_the_written_sequence() {
        for row in [0, 8, 9, 98, 99, 998, 999, 9_998, 9_999, u16::MAX] {
            for col in [0, 9, 99, u16::MAX] {
                let mut out = Vec::new();
                write_cup(&mut out, row, col);
                assert_eq!(cup_len(row, col), out.len(), "row {row} col {col}");
            }
        }
    }

    /// Assembled diffs keep their byte layout: toggle, then each changed row
    /// as CUP + pen reset + body, then the epilogue.
    #[test]
    fn assemble_diff_lays_out_toggle_rows_and_epilogue() {
        let bodies = vec![b"zero".to_vec(), b"one".to_vec(), b"two".to_vec()];
        let out = assemble_diff(b"T", &[0, 2], &bodies, b"E");
        assert_eq!(out, b"T\x1b[1;1H\x1b[0mzero\x1b[3;1H\x1b[0mtwoE");
    }

    #[test]
    fn synthesizer_returns_dimensions() {
        let terminal = fresh(80, 24);
        let snap = synthesize(&terminal).expect("synth");
        assert_eq!(snap.cols, 80);
        assert_eq!(snap.rows, 24);
        // First bytes should be the reset prelude.
        assert!(snap.bytes.starts_with(b"\x1b[!p\x1b[2J\x1b[H"));
        // No scrollback requested ⇒ no scrollback-priming bytes.
        assert!(snap.scrollback.is_empty());
    }

    #[test]
    fn bounded_synthesis_rejects_tiny_source_budget() {
        let terminal = fresh(80, 24);
        assert!(matches!(
            SnapshotSynthesizer::synthesize_bounded(&terminal, 4),
            Err(SynthesisError::LimitExceeded)
        ));
    }

    #[test]
    fn snapshot_replays_kitty_graphics_placements() {
        let mut terminal = fresh(10, 5);
        phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut terminal)
            .expect("kitty config");
        terminal.resize(10, 5, 8, 16).expect("cell geometry");
        terminal.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,c=1,r=1,i=9,q=2;AAECAw==\x1b\\");

        let snap = synthesize(&terminal).expect("synth");
        let replay = String::from_utf8_lossy(&snap.bytes);
        assert!(
            replay.contains("\x1b_Ga=T,f=32,s=1,v=1,i=9,q=2,c=1,r=1,m=0;AAECAw==\x1b\\"),
            "snapshot must replay stored Kitty image placement, got {replay:?}"
        );
    }

    /// Resize between two pooled ticks, with a fresh-state walk interleaved:
    /// the second tick must report the new dims and content.
    #[test]
    fn prepare_tick_serves_fresh_rows_after_resize() {
        let mut term = fresh(10, 2);
        term.vt_write(b"AA");
        let mut synth = SnapshotSynthesizer::new().expect("synth");

        let (c0, r0, _) = synth.prepare_tick(&term).expect("tick0");
        assert_eq!((c0, r0), (10, 2));
        assert!(
            synth.tick_rows.iter().any(|b| b.contains(&b'A')),
            "first tick should render the 'A' content"
        );

        // Grow the grid and overwrite row 0, then race a fresh-state walk
        // that consumes the terminal's per-row dirty bits.
        term.resize(10, 4, 0, 0).expect("resize");
        term.vt_write(b"\x1b[1;1HZZ");
        let _ = synth.synthesize(&term).expect("fresh walk");

        let (c1, r1, _) = synth.prepare_tick(&term).expect("tick1");
        assert_eq!((c1, r1), (10, 4), "second tick must observe the new dims");
        assert_eq!(
            synth.tick_rows.len(),
            4,
            "row buffer resized to the new grid"
        );
        assert!(
            synth.tick_rows.iter().any(|b| b.contains(&b'Z')),
            "post-resize tick must serve the fresh 'ZZ', not the stale cache"
        );
        assert_eq!(
            synth.pool.last_dims(),
            Some((10, 4)),
            "pool tracks the live dims"
        );
    }

    /// A scrollback-bearing snapshot applied as the client does reconstructs
    /// both the viewport and every history row.
    #[test]
    fn scrollback_snapshot_round_trips_history_and_viewport() {
        // 4-row grid; 10 numbered lines push 6 into history (10 - 4 visible).
        let mut source = fresh(20, 4);
        for i in 1..=10 {
            source.vt_write(format!("line{i}\r\n").as_bytes());
        }
        // After the trailing CRLF the cursor sits on a fresh blank row, so the
        // viewport is [line8, line9, line10, ""] and history holds line1..=7.
        let synth = SnapshotSynthesizer::new().expect("synth");
        let snap = synth
            .synthesize_with_scrollback(&source, Some(SCROLLBACK_ALL))
            .expect("synthesize_with_scrollback");
        assert!(
            !snap.scrollback.is_empty(),
            "history present ⇒ scrollback-priming bytes emitted"
        );

        // Replay onto a fresh client terminal, wire order: scrollback, viewport.
        let mut client = fresh(20, 4);
        client.vt_write(&snap.scrollback);
        client.vt_write(&snap.bytes);

        // Viewport matches.
        assert_eq!(render_grid(&client), render_grid(&source));

        // History matches, row for row, with nothing dropped.
        let mut sb_synth = SnapshotSynthesizer::new().expect("synth2");
        let source_hist = sb_synth
            .screen_state_with_scrollback(&source, 0, Some(SCROLLBACK_ALL), false)
            .expect("source history")
            .scrollback;
        let client_hist = sb_synth
            .screen_state_with_scrollback(&client, 0, Some(SCROLLBACK_ALL), false)
            .expect("client history")
            .scrollback;
        assert_eq!(
            client_hist, source_hist,
            "reconstructed history must equal the source's retained rows"
        );
        assert!(
            source_hist.iter().any(|l| l == "line7"),
            "sanity: line7 is the most-recent history row and must survive"
        );
    }

    /// A bounded request keeps only the most-recent `n` history rows.
    #[test]
    fn scrollback_snapshot_honors_bounded_limit() {
        let mut source = fresh(20, 4);
        for i in 1..=10 {
            source.vt_write(format!("line{i}\r\n").as_bytes());
        }
        let synth = SnapshotSynthesizer::new().expect("synth");
        let snap = synth
            .synthesize_with_scrollback(&source, Some(2))
            .expect("synthesize_with_scrollback");

        let mut client = fresh(20, 4);
        client.vt_write(&snap.scrollback);
        client.vt_write(&snap.bytes);

        let mut sb_synth = SnapshotSynthesizer::new().expect("synth2");
        let client_hist = sb_synth
            .screen_state_with_scrollback(&client, 0, Some(SCROLLBACK_ALL), false)
            .expect("client history")
            .scrollback;
        // Exactly the 2 most-recent history rows (line6, line7), no more.
        assert_eq!(client_hist, vec!["line6".to_owned(), "line7".to_owned()]);
        assert_eq!(render_grid(&client), render_grid(&source));
    }

    /// Each viewport row as a string, skipping wide-cell tails.
    fn render_grid(t: &GhosttyTerminal<'_, '_>) -> Vec<String> {
        let mut rs = RenderState::new().expect("RenderState::new");
        let snap = rs.update(t).expect("update");
        let rows_n = snap.rows().expect("rows");
        let mut row_iter_storage = RowIterator::new().expect("RowIterator::new");
        let mut cell_iter_storage = CellIterator::new().expect("CellIterator::new");
        let mut row_iter = row_iter_storage.update(&snap).expect("row update");
        let mut grid: Vec<String> = Vec::with_capacity(usize::from(rows_n));
        let mut i: u16 = 0;
        while let Some(row) = row_iter.next() {
            if i >= rows_n {
                break;
            }
            let mut line = String::new();
            let mut cell_iter = cell_iter_storage.update(row).expect("cell update");
            while let Some(cell) = cell_iter.next() {
                let wide = cell.raw_cell().expect("raw_cell").wide().expect("wide");
                if matches!(wide, CellWide::SpacerTail) {
                    continue;
                }
                let graphemes = cell.graphemes().expect("graphemes");
                if graphemes.is_empty() {
                    line.push(' ');
                } else {
                    for ch in &graphemes {
                        line.push(*ch);
                    }
                }
            }
            grid.push(line);
            i += 1;
        }
        grid
    }

    /// `(grapheme, fg, bg, underline, overline)` of a reconstructed cell.
    type StyledCell = (char, Option<RgbColor>, Option<RgbColor>, bool, bool);

    /// Per-cell styled view of the first row, for color round-trip asserts.
    fn row0_styled(t: &GhosttyTerminal<'_, '_>) -> Vec<StyledCell> {
        let mut rs = RenderState::new().expect("RenderState::new");
        let snap = rs.update(t).expect("update");
        let mut row_iter_storage = RowIterator::new().expect("RowIterator::new");
        let mut cell_iter_storage = CellIterator::new().expect("CellIterator::new");
        let mut row_iter = row_iter_storage.update(&snap).expect("row update");
        let row = row_iter.next().expect("at least one row");
        let mut cell_iter = cell_iter_storage.update(row).expect("cell update");
        let mut out = Vec::new();
        while let Some(cell) = cell_iter.next() {
            let wide = cell.raw_cell().expect("raw_cell").wide().expect("wide");
            if matches!(wide, CellWide::SpacerTail) {
                continue;
            }
            let g = cell.graphemes().expect("graphemes");
            let ch = g.first().copied().unwrap_or(' ');
            let style = cell.style().expect("style");
            out.push((
                ch,
                cell.fg_color().expect("fg"),
                cell.bg_color().expect("bg"),
                !matches!(style.underline, libghostty_vt::style::Underline::None),
                style.overline,
            ));
        }
        out
    }

    /// Snapshot `vt`, replay it into a fresh terminal, return row 0's cells.
    fn round_trip_row0(vt: &[u8]) -> Vec<StyledCell> {
        let mut source = fresh(40, 4);
        source.vt_write(vt);
        let snap = synthesize(&source).expect("synth");
        let mut client = fresh(40, 4);
        client.vt_write(&snap.bytes);
        row0_styled(&client)
    }

    /// A pure color change between same-attribute runs survives the snapshot.
    #[test]
    fn snapshot_preserves_adjacent_color_change() {
        let cells = round_trip_row0(b"\x1b[31mAB\x1b[34mCD\x1b[0m");
        let fg = |c: char| cells.iter().find(|x| x.0 == c).map(|x| x.1);
        assert_ne!(
            fg('A'),
            fg('C'),
            "red AB and blue CD must reconstruct as different foregrounds"
        );
        // Both runs are colored (neither collapsed to the default).
        assert!(fg('A').flatten().is_some(), "AB keeps a foreground");
        assert!(fg('C').flatten().is_some(), "CD keeps a foreground");
    }

    /// A colored blank region keeps its background through the snapshot.
    #[test]
    fn snapshot_preserves_blank_cell_background() {
        let cells = round_trip_row0(b"X\x1b[44m   \x1b[0mY");
        // Cells 1..=3 are blue-background spaces between X and Y.
        let blank_bg = cells[1].2;
        assert!(
            blank_bg.is_some(),
            "blue-background blanks must reconstruct with a background, got {blank_bg:?}"
        );
        assert_eq!(cells[1].0, ' ', "the colored region is blank");
        assert_eq!(cells[1].2, cells[2].2, "the whole blue run shares one bg");
    }

    /// Underline and overline must survive the snapshot — both emitters used
    /// to drop them, flattening neovim undercurls and p10k underlined segments.
    #[test]
    fn snapshot_preserves_underline_and_overline() {
        // SGR 4 = underline, 53 = overline.
        let cells = round_trip_row0(b"\x1b[4mU\x1b[0m\x1b[53mO\x1b[0m");
        let u = cells.iter().find(|x| x.0 == 'U').expect("U cell");
        assert!(u.3, "underline must reconstruct");
        let o = cells.iter().find(|x| x.0 == 'O').expect("O cell");
        assert!(o.4, "overline must reconstruct");
    }

    #[test]
    fn screen_state_projects_text_lines_and_dims() {
        // The agent-surface read path: walk the grid into structured text.
        let mut t = fresh(20, 5);
        t.vt_write(b"hello\r\nworld");
        let mut synth = SnapshotSynthesizer::new().expect("synth");
        let screen = synth.screen_state(&t, 7).expect("screen_state");

        assert_eq!(screen.schema_version, SCHEMA_VERSION);
        assert_eq!(screen.pane, 7, "pane id is stamped from the argument");
        assert_eq!((screen.cols, screen.rows), (20, 5));
        assert_eq!(screen.lines.len(), 5, "one entry per grid row");
        assert_eq!(screen.lines[0], "hello");
        assert_eq!(screen.lines[1], "world");
        // Trailing blank rows trim to empty strings.
        assert_eq!(screen.lines[4], "");
        // Cursor lands just past "world" on row 1 (0-based).
        let cursor = screen.cursor.expect("cursor resolvable in viewport");
        assert_eq!((cursor.x, cursor.y), (5, 1));
    }

    #[test]
    fn screen_state_without_cells_leaves_cells_none() {
        // The default read path (cells = false) must not allocate the
        // cells projection — back-compat with the pre-phux-8yl shape.
        let mut t = fresh(20, 3);
        t.vt_write(b"hello");
        let mut synth = SnapshotSynthesizer::new().expect("synth");
        let screen = synth.screen_state(&t, 1).expect("screen_state");
        assert!(screen.cells.is_none(), "cells = false leaves cells None");
    }

    #[test]
    fn screen_state_cells_collects_styles_sparsely() {
        // Bold-red "HI" then plain "ok": only the styled cells appear.
        let mut t = fresh(20, 2);
        // ESC[1;31m = bold + red fg; "HI"; ESC[0m reset; " ok".
        t.vt_write(b"\x1b[1;31mHI\x1b[0m ok");

        let mut synth = SnapshotSynthesizer::new().expect("synth");
        let screen = synth
            .screen_state_with_scrollback(&t, 1, None, true)
            .expect("screen_state_with_scrollback");
        let cells = screen.cells.expect("cells = true populates Some(..)");

        assert_eq!(
            cells.len(),
            2,
            "only the two bold-red cells are emitted, got {cells:?}",
        );
        for (i, cell) in cells.iter().enumerate() {
            assert_eq!((cell.row, cell.col), (0, u16::try_from(i).unwrap()));
            assert!(cell.style.bold, "bold cell {i}");
            assert!(!cell.style.italic, "not italic {i}");
            // ANSI `31` is palette slot 1 (red); the explicit per-cell
            // palette index is preserved rather than collapsed to RGB.
            assert_eq!(
                cell.style.fg,
                CellColor::Palette { index: 1 },
                "ANSI red keeps its palette identity",
            );
            assert_eq!(cell.style.bg, CellColor::Default, "no explicit bg");
            assert!(
                cell.semantic.is_none(),
                "no OSC-133 marks written -> Output collapses to None",
            );
        }
    }

    #[test]
    fn screen_state_cells_captures_osc133_semantic_marks() {
        // OSC 133 A (prompt) then B (input): cells carry the semantic mark.
        let mut t = fresh(40, 2);
        // OSC 133 ; A  -> prompt start. Then "$ " is prompt text.
        t.vt_write(b"\x1b]133;A\x07$ ");
        // OSC 133 ; B  -> command (input) start. Then "ls" is input.
        t.vt_write(b"\x1b]133;B\x07ls");

        let mut synth = SnapshotSynthesizer::new().expect("synth");
        let screen = synth
            .screen_state_with_scrollback(&t, 1, None, true)
            .expect("screen_state_with_scrollback");
        let cells = screen.cells.expect("cells = true populates Some(..)");

        let prompt_marked = cells
            .iter()
            .any(|c| matches!(c.semantic, Some(SemanticContent::Prompt)));
        let input_marked = cells
            .iter()
            .any(|c| matches!(c.semantic, Some(SemanticContent::Input)));
        assert!(
            prompt_marked,
            "OSC-133 ;A region must surface a Prompt cell, got {cells:?}",
        );
        assert!(
            input_marked,
            "OSC-133 ;B region must surface an Input cell, got {cells:?}",
        );
    }

    #[test]
    fn screen_state_cells_reports_true_column_after_wide_glyph() {
        // Styled cell right of a wide glyph reports its true grid column.
        let mut t = fresh(20, 2);
        // Unstyled wide glyph (你, two columns) then a bold "X" at col 2.
        t.vt_write("你".as_bytes());
        t.vt_write(b"\x1b[1mX");

        let mut synth = SnapshotSynthesizer::new().expect("synth");
        let screen = synth
            .screen_state_with_scrollback(&t, 1, None, true)
            .expect("screen_state_with_scrollback");
        let cells = screen.cells.expect("cells = true populates Some(..)");

        // The wide glyph is unstyled and dropped by the sparse filter, so the
        // bold X is the only emitted cell; it must report col 2, not col 1.
        let x = cells
            .iter()
            .find(|c| c.style.bold)
            .expect("bold X after the wide glyph must be emitted");
        assert_eq!(
            (x.row, x.col),
            (0, 2),
            "styled cell after a double-width glyph must report true column 2, got {cells:?}",
        );
    }

    #[test]
    fn screen_state_cells_accounts_for_spacer_head_at_soft_wrap() {
        // A wide glyph that does not fit wraps, leaving a width-1 SpacerHead:
        //   row 0:  a(0) b(1) c(2) SpacerHead(3)
        //   row 1:  你(0,wide) SpacerTail(1) d(2)
        let mut t = fresh(4, 3);
        t.vt_write(b"\x1b[1m");
        t.vt_write("abc你d".as_bytes());

        let mut synth = SnapshotSynthesizer::new().expect("synth");
        let screen = synth
            .screen_state_with_scrollback(&t, 1, None, true)
            .expect("screen_state_with_scrollback");
        let cells = screen.cells.expect("cells = true populates Some(..)");

        let coords: Vec<(u16, u16)> = cells.iter().map(|c| (c.row, c.col)).collect();
        assert_eq!(
            coords,
            vec![(0, 0), (0, 1), (0, 2), (0, 3), (1, 0), (1, 2)],
            "SpacerHead is a width-1 cell at the wrapped row's last column; \
             the wide glyph restarts column accounting at col 0 of the next \
             row and its SpacerTail (row 1, col 1) is skipped, got {cells:?}",
        );

        assert!(
            cells.iter().any(|c| (c.row, c.col) == (1, 0)),
            "wide glyph wrapped to row 1 must report col 0, got {cells:?}",
        );
        assert!(
            cells.iter().any(|c| (c.row, c.col) == (1, 2)),
            "cell after the wrapped wide glyph must report true col 2, got {cells:?}",
        );
    }

    #[test]
    fn screen_state_with_scrollback_collects_history() {
        // Five lines on a 3-row grid: two land in scrollback.
        let mut t = fresh(20, 3);
        t.vt_write(b"line1\r\nline2\r\nline3\r\nline4\r\nline5");
        // Sanity: libghostty must actually be retaining the two scrolled rows.
        assert_eq!(t.scrollback_rows().expect("scrollback_rows"), 2);

        let mut synth = SnapshotSynthesizer::new().expect("synth");
        let screen = synth
            .screen_state_with_scrollback(&t, 7, Some(SCROLLBACK_ALL), false)
            .expect("screen_state_with_scrollback");

        assert_eq!(screen.schema_version, SCHEMA_VERSION);
        assert_eq!(
            screen.scrollback,
            vec!["line1".to_owned(), "line2".to_owned()],
            "all history, oldest first",
        );
        assert_eq!(screen.lines.len(), 3, "viewport stays full height");
        assert_eq!(screen.lines[0], "line3");
        assert_eq!(screen.lines[1], "line4");
        assert_eq!(screen.lines[2], "line5");
    }

    #[test]
    fn screen_state_with_scrollback_bounds_to_recent_rows() {
        // A bounded request keeps the rows nearest the viewport (the most
        // recent history), not the oldest.
        let mut t = fresh(20, 2);
        // 5 lines, 2-row viewport -> 3 rows of scrollback (line1..line3).
        t.vt_write(b"line1\r\nline2\r\nline3\r\nline4\r\nline5");
        assert_eq!(t.scrollback_rows().expect("scrollback_rows"), 3);

        let mut synth = SnapshotSynthesizer::new().expect("synth");
        let screen = synth
            .screen_state_with_scrollback(&t, 1, Some(2), false)
            .expect("screen_state_with_scrollback");

        assert_eq!(
            screen.scrollback,
            vec!["line2".to_owned(), "line3".to_owned()],
            "the most-recent 2 of 3 history rows, oldest-first",
        );
    }

    #[test]
    fn screen_state_without_scrollback_leaves_history_empty() {
        // None must reproduce the legacy viewport-only shape exactly.
        let mut t = fresh(20, 3);
        t.vt_write(b"a\r\nb\r\nc\r\nd\r\ne");
        assert!(t.scrollback_rows().expect("scrollback_rows") > 0);

        let mut synth = SnapshotSynthesizer::new().expect("synth");
        let none = synth
            .screen_state_with_scrollback(&t, 0, None, false)
            .expect("with None");
        let legacy = synth.screen_state(&t, 0).expect("screen_state");

        assert!(none.scrollback.is_empty(), "no scrollback requested");
        assert_eq!(none.lines, legacy.lines, "viewport unchanged by None path");
        assert!(legacy.scrollback.is_empty());
    }

    #[test]
    fn screen_state_with_scrollback_empty_when_no_history() {
        // Requesting scrollback on a pane with no history yields an empty
        // vec, not an error.
        let mut t = fresh(20, 5);
        t.vt_write(b"only one line");
        assert_eq!(t.scrollback_rows().expect("scrollback_rows"), 0);

        let mut synth = SnapshotSynthesizer::new().expect("synth");
        let screen = synth
            .screen_state_with_scrollback(&t, 0, Some(SCROLLBACK_ALL), false)
            .expect("screen_state_with_scrollback");
        assert!(screen.scrollback.is_empty());
        assert_eq!(screen.lines[0], "only one line");
    }

    /// A long line soft-wraps; unwrapping joins it back into the written
    /// text, which no single painted row contains.
    #[test]
    fn screen_state_reports_soft_wrap_and_unwraps_to_the_written_line() {
        let mut t = fresh(10, 4);
        t.vt_write(b"abcdefghijklmnop");

        let mut synth = SnapshotSynthesizer::new().expect("synth");
        let screen = synth.screen_state(&t, 1).expect("screen_state");

        let wrap = screen.soft_wrap.as_ref().expect("wrap info is reported");
        assert_eq!(
            wrap.lines,
            vec![0],
            "row 0 continues onto row 1, got {screen:?}",
        );
        assert_eq!(screen.lines[0], "abcdefghij");
        assert_eq!(screen.lines[1], "klmnop");
        assert!(
            !screen.lines.iter().any(|l| l.contains("ijkl")),
            "the straddling substring is absent from the rows as painted",
        );
        assert_eq!(
            screen.unwrapped_rows()[0],
            "abcdefghijklmnop",
            "unwrapping restores the line as written",
        );
    }

    /// A wide glyph wrapped past a `SpacerHead` still reports the wrap, and the
    /// join does not include the spacer's blank.
    #[test]
    fn screen_state_unwraps_across_a_wide_glyph_at_the_wrap_boundary() {
        let mut t = fresh(4, 3);
        t.vt_write("abc你d".as_bytes());

        let mut synth = SnapshotSynthesizer::new().expect("synth");
        let screen = synth.screen_state(&t, 1).expect("screen_state");

        let wrap = screen.soft_wrap.as_ref().expect("wrap info is reported");
        assert_eq!(wrap.lines, vec![0], "got {screen:?}");
        assert_eq!(
            screen.lines[0], "abc",
            "the SpacerHead is a blank final column and trims away",
        );
        assert_eq!(screen.lines[1], "你d");
        assert_eq!(
            screen.unwrapped_rows()[0],
            "abc你d",
            "the wide glyph rejoins its line with no stray spacer column",
        );
    }

    /// History rows report wrap bits too, and a run across the seam joins.
    #[test]
    fn screen_state_reports_soft_wrap_in_scrollback() {
        // 3-row viewport; a long first line wraps into two rows and the
        // later lines push both into history.
        let mut t = fresh(10, 3);
        t.vt_write(b"abcdefghijklmnop\r\nsecond\r\nthird\r\nfourth\r\nfifth");
        assert!(t.scrollback_rows().expect("scrollback_rows") >= 2);

        let mut synth = SnapshotSynthesizer::new().expect("synth");
        let screen = synth
            .screen_state_with_scrollback(&t, 1, Some(SCROLLBACK_ALL), false)
            .expect("screen_state_with_scrollback");

        let wrap = screen.soft_wrap.as_ref().expect("wrap info is reported");
        assert_eq!(
            wrap.scrollback,
            vec![0],
            "history row 0 continues onto history row 1, got {screen:?}",
        );
        assert_eq!(
            screen.unwrapped_rows()[0],
            "abcdefghijklmnop",
            "the wrapped history line rejoins",
        );
    }

    /// `truncated` is set only when older retained rows were left out.
    #[test]
    fn screen_state_reports_truncated_only_when_the_window_clipped() {
        let mut t = fresh(20, 2);
        // 5 lines, 2-row viewport -> 3 rows of history.
        t.vt_write(b"line1\r\nline2\r\nline3\r\nline4\r\nline5");
        assert_eq!(t.scrollback_rows().expect("scrollback_rows"), 3);
        let mut synth = SnapshotSynthesizer::new().expect("synth");

        let clipped = synth
            .screen_state_with_scrollback(&t, 1, Some(2), false)
            .expect("screen_state_with_scrollback");
        assert!(
            clipped.truncated,
            "2 of 3 retained history rows: the window dropped an older row",
        );
        assert_eq!(
            clipped.truncated_reason.as_deref(),
            Some(TRUNCATED_ROW_WINDOW),
        );

        let whole = synth
            .screen_state_with_scrollback(&t, 1, Some(SCROLLBACK_ALL), false)
            .expect("screen_state_with_scrollback");
        assert!(!whole.truncated, "all retained history: nothing dropped");
        assert!(whole.truncated_reason.is_none());

        let exact = synth
            .screen_state_with_scrollback(&t, 1, Some(3), false)
            .expect("screen_state_with_scrollback");
        assert!(!exact.truncated, "a window that fits drops nothing");

        let viewport_only = synth
            .screen_state_with_scrollback(&t, 1, None, false)
            .expect("screen_state_with_scrollback");
        assert!(
            !viewport_only.truncated,
            "history that was never requested was not dropped from a window",
        );
    }

    /// The OSC 0/2 title rides back when set, and is absent — not an empty
    /// string — when the pane never set one (ADR-0077 §3).
    #[test]
    fn screen_state_reports_the_osc_title_when_set() {
        let mut t = fresh(20, 2);
        let mut synth = SnapshotSynthesizer::new().expect("synth");
        assert!(
            synth
                .screen_state(&t, 0)
                .expect("screen_state")
                .title
                .is_none(),
            "an unset title is absence, not an empty string",
        );

        t.vt_write(b"\x1b]0;claude - phux\x07");
        assert_eq!(
            synth.screen_state(&t, 0).expect("screen_state").title,
            Some("claude - phux".to_owned()),
        );
    }

    #[test]
    fn synthesizer_round_trips_via_libghostty() {
        // Replayed snapshot reproduces the cursor position.
        let mut a = fresh(20, 5);
        a.vt_write(b"hello\r\nworld");
        let synth = synthesize(&a).expect("synth");

        let mut b = fresh(synth.cols, synth.rows);
        b.vt_write(&synth.bytes);

        // Both terminals should report cursor at the end of "world" on row 1.
        let ax = a.cursor_x().expect("cursor_x a");
        let ay = a.cursor_y().expect("cursor_y a");
        let bx = b.cursor_x().expect("cursor_x b");
        let by = b.cursor_y().expect("cursor_y b");
        assert_eq!((ax, ay), (bx, by), "cursor position should round-trip");
    }

    /// Reusing one synthesizer across attaches still yields the full grid
    /// after a prior `synthesize()` consumed the dirty bits.
    #[test]
    fn synthesize_reused_across_calls_emits_full_snapshot_each_time() {
        let mut t = fresh(20, 5);
        let synth = SnapshotSynthesizer::new().expect("synth");

        // First attach: blank grid (no content yet).
        let snap1 = synth.synthesize(&t).expect("synth1");
        assert!(
            !String::from_utf8_lossy(&snap1.bytes).contains("MARKER"),
            "blank grid should not carry MARKER yet",
        );

        // Content arrives with NO trailing newline (like `printf MARKER`).
        t.vt_write(b"MARKER");

        // Second attach via the SAME synthesizer: the snapshot MUST be a full
        // repaint carrying MARKER, not a delta against the consumed dirty bits.
        let snap2 = synth.synthesize(&t).expect("synth2");
        let body = String::from_utf8_lossy(&snap2.bytes);
        assert!(
            body.contains("MARKER"),
            "phux-uow0: reused synthesizer must emit a FULL snapshot including \
             content written after the first call; got: {body:?}",
        );

        // And it must round-trip into a fresh terminal's grid.
        let mut b = fresh(snap2.cols, snap2.rows);
        b.vt_write(&snap2.bytes);
        assert_eq!(render_grid(&b)[0], "MARKER              ");
    }

    /// Another render state consuming the dirty bits before the snapshot
    /// must not blank it: `synthesize()` is full, not a delta.
    #[test]
    fn full_snapshot_survives_another_consumer_eating_dirty_bits() {
        let mut t = fresh(20, 5);
        let snap_synth = SnapshotSynthesizer::new().expect("snap_synth");

        // Client 1 attaches: synthesize the (blank) grid; this consumes dirty
        // and leaves snap_synth's reference clean.
        let _ = snap_synth.synthesize(&t).expect("synth client1");

        // Content arrives with no trailing newline (printf-style).
        t.vt_write(b"MARKER");

        // Client 2's register_consumer primes a SEPARATE per-consumer reference,
        // whose update consumes the Terminal's freshly-set dirty bits.
        let mut other = SnapshotSynthesizer::new().expect("other");
        let _ = other.screen_state(&t, 0).expect("other screen_state");

        // Client 2's snapshot via the SHARED synthesizer must still carry MARKER.
        let snap = snap_synth.synthesize(&t).expect("synth client2");
        let body = String::from_utf8_lossy(&snap.bytes);
        assert!(
            body.contains("MARKER"),
            "phux-uow0: a full snapshot must emit the whole grid even when another \
             consumer already consumed the per-row dirty bits; got: {body:?}",
        );
    }

    /// A wide glyph's tail is skipped, not replayed as a space.
    #[test]
    fn synthesizer_skips_wide_cell_tails() {
        let mut a = fresh(10, 2);
        // Two CJK glyphs (4 columns wide total) followed by ASCII.
        a.vt_write("你好ab".as_bytes());

        // Sanity: the source grid should contain both wide glyphs.
        let src_grid = render_grid(&a);
        assert_eq!(src_grid[0], "你好ab    ", "source grid layout");

        let synth = synthesize(&a).expect("synth");
        let mut b = fresh(synth.cols, synth.rows);
        b.vt_write(&synth.bytes);

        let dst_grid = render_grid(&b);
        assert_eq!(
            src_grid, dst_grid,
            "grid must round-trip through synthesizer for wide glyphs"
        );

        // Bytes must NOT contain a stray space between the two wide
        // glyphs (`你` followed by ` ` would be the wide-tail bug).
        let bytes_str = String::from_utf8_lossy(&synth.bytes);
        assert!(
            bytes_str.contains("你好"),
            "synthesized bytes should contain consecutive wide glyphs, got: {bytes_str:?}"
        );
        assert!(
            !bytes_str.contains("你 好"),
            "synthesized bytes must not insert a space between wide glyphs (wide-tail bug)"
        );
    }

    /// Mixed CJK, emoji, and ASCII round-trip cell-for-cell.
    #[test]
    fn synthesizer_round_trips_cjk_and_emoji() {
        let mut a = fresh(20, 4);
        // Row 0: CJK + ASCII. Row 1: pure emoji. Row 2: mixed
        // emoji/ASCII. The CRLF sequences keep row layout deterministic.
        a.vt_write("東 hello\r\n".as_bytes());
        a.vt_write("😀😀😀\r\n".as_bytes());
        a.vt_write("a😀b".as_bytes());

        let src_grid = render_grid(&a);

        let synth = synthesize(&a).expect("synth");
        let mut b = fresh(synth.cols, synth.rows);
        b.vt_write(&synth.bytes);

        let dst_grid = render_grid(&b);
        assert_eq!(
            src_grid, dst_grid,
            "CJK + emoji content must round-trip through the synthesizer"
        );

        assert!(
            src_grid[0].starts_with('東'),
            "source row 0 should start with 東, got {:?}",
            src_grid[0]
        );
    }

    /// A 1049 alt-screen snapshot re-establishes `?1049h`, before the
    /// cursor restore.
    #[test]
    fn snapshot_reestablishes_alt_screen_1049() {
        let mut t = fresh(20, 4);
        t.vt_write(b"\x1b[?1049h");
        t.vt_write(b"alt-screen body");

        // libghostty keeps 47 and 1049 as distinct bits.
        assert!(
            t.mode(Mode::ALT_SCREEN_SAVE).expect("mode 1049"),
            "?1049h should set ALT_SCREEN_SAVE",
        );
        assert!(
            !t.mode(Mode::ALT_SCREEN_LEGACY).expect("mode 47"),
            "1049 must not set the legacy-47 bit",
        );

        let snap = synthesize(&t).expect("synth");
        let bytes = String::from_utf8_lossy(&snap.bytes);

        assert!(
            bytes.contains("?1049h"),
            "snapshot must re-emit ?1049h so the mirror lands on the alt screen; bytes={bytes:?}",
        );
        assert!(
            !bytes.contains("?47h"),
            "47 is off; snapshot must not assert ?47h; bytes={bytes:?}",
        );
        // `?25h`/`?25l` follows the cursor restore, so it marks "cursor
        // re-established".
        let pos_1049 = bytes.find("?1049h").expect("?1049h present");
        let pos_cursor_vis = bytes
            .find("?25h")
            .or_else(|| bytes.find("?25l"))
            .expect("epilogue cursor-visibility present");
        assert!(
            pos_1049 < pos_cursor_vis,
            "?1049h (at {pos_1049}) must precede the epilogue cursor block (at {pos_cursor_vis}); bytes={bytes:?}",
        );
    }

    /// Mouse-tracking modes survive the snapshot, checked on the replayed
    /// mirror's mode state.
    #[test]
    fn snapshot_reestablishes_mouse_tracking_modes() {
        // The DECSET set opencode / Claude Code were probed to use.
        let mut t = fresh(20, 4);
        t.vt_write(b"\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h");
        t.vt_write(b"tui body");

        let snap = synthesize(&t).expect("synth");
        let mut mirror = fresh(snap.cols, snap.rows);
        mirror.vt_write(&snap.bytes);

        for (mode, code) in [
            (Mode::NORMAL_MOUSE, "1000"),
            (Mode::BUTTON_MOUSE, "1002"),
            (Mode::ANY_MOUSE, "1003"),
            (Mode::SGR_MOUSE, "1006"),
        ] {
            assert!(
                mirror.mode(mode).expect("mirror mode"),
                "mirror must have ?{code}h after replaying the snapshot",
            );
        }
    }

    /// A pane that never asked for the mouse replays with tracking off.
    #[test]
    fn snapshot_leaves_mouse_tracking_off_for_a_plain_pane() {
        let mut t = fresh(20, 4);
        t.vt_write(b"plain shell");

        let snap = synthesize(&t).expect("synth");
        let mut mirror = fresh(snap.cols, snap.rows);
        mirror.vt_write(&snap.bytes);

        for (mode, code) in [
            (Mode::X10_MOUSE, "9"),
            (Mode::NORMAL_MOUSE, "1000"),
            (Mode::BUTTON_MOUSE, "1002"),
            (Mode::ANY_MOUSE, "1003"),
        ] {
            assert!(
                !mirror.mode(mode).expect("mirror mode"),
                "?{code} must stay off for a pane that never enabled the mouse",
            );
        }
    }

    /// An app's `?1007l` opt-out survives the snapshot.
    #[test]
    fn snapshot_preserves_an_alt_scroll_opt_out() {
        let mut t = fresh(20, 4);
        t.vt_write(b"\x1b[?1049h\x1b[?1007l");
        assert!(
            !t.mode(Mode::ALT_SCROLL).expect("mode 1007"),
            "?1007l should clear ALT_SCROLL",
        );

        let snap = synthesize(&t).expect("synth");
        let mut mirror = fresh(snap.cols, snap.rows);
        mirror.vt_write(&snap.bytes);

        assert!(
            !mirror.mode(Mode::ALT_SCROLL).expect("mirror mode 1007"),
            "the mirror must inherit the ?1007l opt-out",
        );
    }

    /// Legacy `?47h` round-trips.
    #[test]
    fn snapshot_reestablishes_alt_screen_47_legacy() {
        let mut t = fresh(20, 4);
        t.vt_write(b"\x1b[?47h");
        t.vt_write(b"legacy alt");
        assert!(t.mode(Mode::ALT_SCREEN_LEGACY).expect("mode 47"));

        let snap = synthesize(&t).expect("synth");
        let bytes = String::from_utf8_lossy(&snap.bytes);
        assert!(
            bytes.contains("?47h"),
            "snapshot must re-emit ?47h for a legacy alt-screen program; bytes={bytes:?}",
        );
    }

    /// On the primary screen every alt-screen mode is reported off.
    #[test]
    fn snapshot_primary_screen_emits_all_alt_modes_off() {
        let mut t = fresh(20, 4);
        t.vt_write(b"primary content");

        let snap = synthesize(&t).expect("synth");
        let bytes = String::from_utf8_lossy(&snap.bytes);
        assert!(bytes.contains("?47l"), "47 off on primary; bytes={bytes:?}");
        assert!(
            bytes.contains("?1047l"),
            "1047 off on primary; bytes={bytes:?}",
        );
        assert!(
            bytes.contains("?1049l"),
            "1049 off on primary; bytes={bytes:?}",
        );
    }

    /// The reference diff trips on a 1049 -> primary transition.
    #[test]
    fn reference_diff_trips_on_alt_screen_transition() {
        let mut t = fresh(20, 4);
        t.vt_write(b"\x1b[?1049h");
        t.vt_write(b"alt body");

        let mut synth = SnapshotSynthesizer::new().expect("synth");
        let mut reference = ConsumerReference::new();
        synth
            .prime_reference(&t, &mut reference)
            .expect("prime_reference");

        // Leave the alt screen: a mode-only change, no row content edits
        // beyond what 1049's restore does.
        t.vt_write(b"\x1b[?1049l");

        let diff = synth
            .synthesize_against_reference(&t, &mut reference)
            .expect("diff");
        assert!(
            !diff.bytes.is_empty(),
            "a 1049->primary transition must produce a non-empty diff",
        );
        let bytes = String::from_utf8_lossy(&diff.bytes);
        assert!(
            bytes.contains("?1049l"),
            "the diff epilogue must re-emit ?1049l on leaving the alt screen; bytes={bytes:?}",
        );
    }

    /// A bare mouse-mode toggle (no row or cursor change) still diffs.
    #[test]
    fn reference_diff_trips_on_a_bare_mouse_mode_toggle() {
        let mut t = fresh(20, 4);
        t.vt_write(b"steady body");

        let mut synth = SnapshotSynthesizer::new().expect("synth");
        let mut reference = ConsumerReference::new();
        synth
            .prime_reference(&t, &mut reference)
            .expect("prime_reference");

        // Mode-only change: no glyphs, no cursor movement.
        t.vt_write(b"\x1b[?1000h\x1b[?1006h");

        let diff = synth
            .synthesize_against_reference(&t, &mut reference)
            .expect("diff");
        let bytes = String::from_utf8_lossy(&diff.bytes);
        assert!(
            bytes.contains("?1000h") && bytes.contains("?1006h"),
            "a bare mouse-mode toggle must re-emit the epilogue; bytes={bytes:?}",
        );
    }

    /// An unchanged terminal diffs to nothing, repeatedly.
    #[test]
    fn reference_diff_empty_when_unchanged() {
        let mut t = fresh(40, 10);
        t.vt_write(b"steady state line one\r\nand line two");

        let mut synth = SnapshotSynthesizer::new().expect("synth");
        let mut reference = ConsumerReference::new();
        synth
            .prime_reference(&t, &mut reference)
            .expect("prime_reference");

        for n in 0..3 {
            let diff = synth
                .synthesize_against_reference(&t, &mut reference)
                .expect("diff");
            assert!(
                diff.bytes.is_empty(),
                "unchanged terminal must diff empty on call {n}, got {:?}",
                String::from_utf8_lossy(&diff.bytes),
            );
        }
    }

    /// `Some(n)` includes exactly `n` history rows.
    #[test]
    fn render_screen_some_n_boundary_includes_exactly_n_history_rows() {
        // 5 lines, 20x2 viewport -> 3 rows of scrollback (line1..line3),
        // matching `screen_state_with_scrollback_bounds_to_recent_rows`.
        let mut t = fresh(20, 2);
        t.vt_write(b"line1\r\nline2\r\nline3\r\nline4\r\nline5");
        assert_eq!(t.scrollback_rows().expect("scrollback_rows"), 3);

        let synth = SnapshotSynthesizer::new().expect("synth");
        let rendered = synth
            .render_screen(&t, Some(2), 2) // format 2 = vt
            .expect("render_screen")
            .expect("format 2 must render");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&rendered.data)
            .expect("valid base64");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("line2") && text.contains("line3"),
            "the most-recent 2 rows (n=2) must be included, got: {text}",
        );
        assert!(
            !text.contains("line1"),
            "the 3rd-from-end row (n+1) must be excluded, got: {text}",
        );
    }

    /// The rendered history window never exceeds `ROW_WINDOW_MAX` rows.
    #[test]
    fn render_screen_clamps_all_retained_history_to_row_window_max() {
        let mut t = GhosttyTerminal::new(10, 2).expect("Terminal::new");
        let over = usize::try_from(ROW_WINDOW_MAX).unwrap_or(usize::MAX) + 5;
        t.set_scrollback_max_lines(Some(over + 10))
            .expect("set_scrollback_max_lines");
        // Lift the byte cap so the clamp under test, not retention, bounds it.
        t.set_scrollback_max_bytes(None)
            .expect("set_scrollback_max_bytes");
        let mut input = Vec::with_capacity(over * 4);
        for i in 0..over {
            input.extend_from_slice(format!("r{i}\r\n").as_bytes());
        }
        input.extend_from_slice(b"last");
        t.vt_write(&input);
        assert!(
            t.scrollback_rows().expect("scrollback_rows")
                > usize::try_from(ROW_WINDOW_MAX).unwrap_or(0),
            "the terminal must actually retain more than ROW_WINDOW_MAX for this test to mean anything",
        );

        let synth = SnapshotSynthesizer::new().expect("synth");
        // format 1 = html: cheaper to search as plain text than base64-vt.
        let rendered = synth
            .render_screen(&t, Some(SCROLLBACK_ALL), 1)
            .expect("render_screen")
            .expect("format 1 must render");
        assert!(
            rendered.data.contains(&format!("r{}", over - 1)),
            "the most-recent retained row must still be present",
        );
        assert!(
            !rendered.data.contains("r0"),
            "the oldest retained row must have been clamped away by ROW_WINDOW_MAX \
             (data omitted from this message; it is large by construction)",
        );
    }

    /// Over budget is refused, not truncated.
    #[test]
    fn render_screen_over_budget_is_refused_not_truncated() {
        let mut t = fresh(20, 2);
        t.vt_write(b"hi");

        let err = SnapshotSynthesizer::render_screen_with_budget(&t, None, 1, 1)
            .expect_err("a one-byte budget must refuse any non-trivial render");
        match err {
            SynthesisError::RenderBudgetExceeded { required, budget } => {
                assert_eq!(budget, 1);
                assert!(required > budget, "got required={required}");
            }
            other => panic!("expected RenderBudgetExceeded, got {other:?}"),
        }
    }

    /// Within budget renders normally.
    #[test]
    fn render_screen_under_budget_renders_normally() {
        let mut t = fresh(20, 2);
        t.vt_write(b"hi");

        let rendered =
            SnapshotSynthesizer::render_screen_with_budget(&t, None, 1, RENDER_BUDGET_BYTES)
                .expect("render_screen_with_budget")
                .expect("format 1 must render");
        assert_eq!(rendered.format, RENDERED_FORMAT_HTML);
        assert!(rendered.data.contains("hi"));
    }

    /// Every fresh-render-state walk drains the terminal's dirty bits; the
    /// next tick must rebuild the pool rather than serve its cached rows.
    #[test]
    fn prepare_tick_serves_rows_a_foreign_walk_drained() {
        type ForeignWalk =
            fn(&mut SnapshotSynthesizer<'static>, &GhosttyTerminal<'static, 'static>);
        let walks: [(&str, ForeignWalk); 3] = [
            ("synthesize", |s, t| drop(s.synthesize(t).expect("walk"))),
            ("synthesize_with_scrollback", |s, t| {
                drop(s.synthesize_with_scrollback(t, Some(1)).expect("walk"));
            }),
            ("prime_reference", |s, t| {
                s.prime_reference(t, &mut ConsumerReference::new())
                    .expect("walk");
            }),
        ];
        for (name, walk) in walks {
            let mut t = fresh(10, 3);
            t.vt_write(b"AA");
            let mut synth = SnapshotSynthesizer::new().expect("synth");
            synth.prepare_tick(&t).expect("tick0");
            t.vt_write(b"\x1b[2;1HZZ");
            walk(&mut synth, &t);
            synth.prepare_tick(&t).expect("tick1");
            assert!(
                synth.tick_rows[1].contains(&b'Z'),
                "{name}: the tick served the pool's stale row 1: {:?}",
                String::from_utf8_lossy(&synth.tick_rows[1]),
            );
            assert_eq!(synth.last_rendered_rows, 3, "{name}: a full render");
        }
    }

    /// A tick after a one-row write re-renders that row only, and a clean
    /// terminal re-renders nothing, while the metadata reader sharing the
    /// pool leaves the row flags for the tick.
    #[test]
    fn prepare_tick_renders_only_rows_the_pool_rebuilt() {
        let mut t = fresh(10, 4);
        // Park the cursor on row 1 first: moving it dirties the row it leaves.
        t.vt_write(b"one\r\ntwo\r\nthree\x1b[2;1H");
        let mut synth = SnapshotSynthesizer::new().expect("synth");
        synth.prepare_tick(&t).expect("first tick");
        assert_eq!(synth.last_rendered_rows, 4, "the first tick is full");

        synth.prepare_tick(&t).expect("clean tick");
        assert_eq!(synth.last_rendered_rows, 0, "a clean tick renders nothing");

        t.vt_write(b"TWO");
        let _ = synth.metadata_snapshot(&t).expect("metadata read");
        synth.prepare_tick(&t).expect("partial tick");
        assert_eq!(synth.last_rendered_rows, 1, "only the written row");
        assert!(synth.tick_rows[1].starts_with(b"\x1b[0mTWO"));
        assert!(synth.tick_rows[0].starts_with(b"\x1b[0mone"), "kept row 0");
    }

    /// The agent detector's and `GET_SCREEN`'s viewport projection reads the
    /// pool without clearing its flags (phux-69pq.14): it sees the live
    /// write, and the next tick still re-renders exactly the written row.
    #[test]
    fn screen_state_leaves_the_next_tick_incremental() {
        let mut t = fresh(10, 4);
        t.vt_write(b"one\r\ntwo\r\nthree\x1b[2;1H");
        let mut synth = SnapshotSynthesizer::new().expect("synth");
        synth.prepare_tick(&t).expect("first tick");

        t.vt_write(b"TWO");
        let screen = synth.screen_state(&t, 0).expect("projection");
        assert_eq!(screen.lines[1], "TWO", "the projection sees the write");
        // Twice: a clean re-read must not lose the pending row either.
        let _ = synth.screen_state(&t, 0).expect("second projection");
        synth.prepare_tick(&t).expect("partial tick");
        assert_eq!(synth.last_rendered_rows, 1, "only the written row");
        assert!(synth.tick_rows[1].starts_with(b"\x1b[0mTWO"));
    }

    /// A projection after a foreign walk honours the pending rebuild, so it
    /// cannot serve the rows that walk drained, and the rebuild's full
    /// redraw still reaches the next tick.
    #[test]
    fn screen_state_after_a_foreign_walk_serves_live_rows() {
        let mut t = fresh(10, 3);
        t.vt_write(b"AA");
        let mut synth = SnapshotSynthesizer::new().expect("synth");
        synth.prepare_tick(&t).expect("tick0");
        t.vt_write(b"\x1b[2;1HZZ");
        drop(synth.synthesize(&t).expect("foreign walk"));
        let screen = synth.screen_state(&t, 0).expect("projection");
        assert_eq!(screen.lines[1], "ZZ", "the projection served a stale row");
        synth.prepare_tick(&t).expect("tick1");
        assert!(synth.tick_rows[1].contains(&b'Z'));
        assert_eq!(synth.last_rendered_rows, 3, "the rebuild renders in full");
    }
}

#[cfg(test)]
mod differential_tests;
