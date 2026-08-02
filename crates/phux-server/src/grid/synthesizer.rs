//! Synthesize a `TERMINAL_SNAPSHOT` `vt_replay_bytes` blob from a
//! `libghostty_vt::Terminal`.
//!
//! Under [ADR-0013] the wire carries VT bytes, not structured grids.
//! When a client attaches, the server owes it a `TERMINAL_SNAPSHOT`
//! (SPEC §8.4) whose body is a self-contained VT byte sequence that —
//! when `vt_write`-en into a fresh `Terminal` of the matching `cols × rows`
//! — reproduces the current grid. This module owns that synthesis.
//!
//! The walk mirrors `research/2026-05-25-libghostty-renderstate.md` §7:
//!
//! 1. Reset (`DECSTR + ED 2 + CUP home`).
//! 2. For each visible row, emit SGR deltas as cell styles change and
//!    write the row's graphemes. Wide-cell tails (empty grapheme on a
//!    `at_wide_tail` cell) are skipped — the base grapheme advanced the
//!    cursor across both cells. Wrapped rows omit the trailing CRLF so
//!    libghostty's parser preserves the soft wrap.
//! 3. Re-establish cursor position (`CUP`).
//! 4. Re-establish cursor visibility (`DECSET 25` / `DECRST 25`) and
//!    visual style (`DECSCUSR`).
//! 5. Re-establish a small set of mode bits queried from the canonical
//!    `GhosttyTerminal` via [`libghostty_vt::Terminal::mode`].
//!
//! Out-of-band registries (OSC 8 hyperlinks, kitty graphics, etc.) are
//! deferred — they need their own re-emission strategy and don't appear
//! in `RenderState` directly.
//!
//! [ADR-0013]: https://github.com/phall1/phux/blob/main/ADR/0013-libghostty-bytes-on-wire.md

use std::io::Write as _;

use phux_core::screen::{
    CellColor, CellInfo, CellStyle, CursorState, SCHEMA_VERSION, ScreenState, SemanticContent,
};
use phux_protocol::{
    kitty_replay,
    sgr::{write_reset_and_sgr, write_reset_and_sgr_unresolved},
};

use libghostty_vt::{
    RenderState, Terminal as GhosttyTerminal,
    render::{CellIteration, CellIterator, CursorVisualStyle, Dirty, RowIterator, Snapshot},
    screen::{CellSemanticContent, CellWide},
    style::{RgbColor, Style, StyleColor},
    terminal::{Mode, Point, PointCoordinate},
};

use super::reference::{ConsumerReference, ReferenceCursorMode};

/// "All retained history" sentinel for the scrollback request.
///
/// A `Some(0)` scrollback request to
/// [`SnapshotSynthesizer::screen_state_with_scrollback`] means "all
/// available history rows" — the bare `--scrollback` flag with no explicit
/// count (`phux-o1v`). A request of literally zero rows is meaningless, so
/// this reuse is unambiguous.
pub const SCROLLBACK_ALL: u32 = 0;

/// Inline grapheme-cluster buffer size for the scrollback walk.
///
/// Covers the overwhelming-common case (a base codepoint plus a few
/// combining marks) without a heap allocation per cell; deeper clusters
/// fall back to a heap retry on `OutOfSpace`.
pub const GRAPHEME_INLINE: usize = 8;

/// Errors that can occur while synthesising a snapshot.
#[derive(Debug, thiserror::Error)]
pub enum SynthesisError {
    /// Surfaced from libghostty-vt.
    #[error("libghostty: {0}")]
    Ghostty(#[from] libghostty_vt::Error),
    /// A `write!` into the snapshot buffer failed (the buffer is a
    /// `Vec<u8>`, so this is structurally unreachable; we keep the
    /// variant to satisfy the error-propagation contract).
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

    fn check(&self) -> Result<(), SynthesisError> {
        if self.bytes.len() <= self.max_bytes {
            Ok(())
        } else {
            Err(SynthesisError::LimitExceeded)
        }
    }
    fn remaining(&self) -> usize {
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
            std::io::Error::new(std::io::ErrorKind::OutOfMemory, "snapshot allocation failed")
        })?;
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Pooled per-pane snapshot scaffolding.
///
/// Owns the libghostty render iterators ([`RenderState`], [`RowIterator`],
/// [`CellIterator`]) so the synthesis path reuses them across attaches
/// instead of reallocating each time. The free [`synthesize`] function is
/// the one-shot wrapper.
#[derive(Debug)]
pub struct SnapshotSynthesizer<'alloc> {
    render_state: RenderState<'alloc>,
    rows: RowIterator<'alloc>,
    cells: CellIterator<'alloc>,
    /// phux-ahk.2: per-tick rendered row bodies, shared across all
    /// consumers of this pane. [`Self::prepare_tick`] renders every row
    /// ONCE into these buffers; each consumer's [`Self::diff_consumer`]
    /// then compares them against its own [`ConsumerReference`]. Reused
    /// across ticks (each row buffer is `clear()`-ed and refilled, so
    /// steady state allocates nothing). Replaces the prior model where
    /// every consumer re-rendered the whole grid (N full renders + 5N
    /// mode-FFI calls for N consumers on one shared pane).
    tick_rows: Vec<Vec<u8>>,
    /// phux-ahk.2: the cursor/mode epilogue bytes for the current tick,
    /// computed once in [`Self::prepare_tick`] (consumer-independent) and
    /// appended by each consumer's non-empty diff.
    tick_epilogue: Vec<u8>,
    /// phux-ahk.2: the screen-buffer select bytes for the current tick
    /// (enter/leave alt screen), computed once; emitted by a consumer's
    /// diff only when that consumer's reference disagrees with the live
    /// alt-screen state.
    tick_screen_toggle: Vec<u8>,
    /// phux-5pyx: the `(cols, rows)` the pooled `render_state` last walked,
    /// so a resize can be detected and the pool rebuilt. libghostty's per-row
    /// dirty bits live on the `Terminal` and are cleared by whichever
    /// `RenderState` reads a row first; after a `TERMINAL_RESIZE` raced a
    /// fresh-state walk (the `GET_SCREEN` path, which goes fresh per call for
    /// exactly this reason), the pooled cache reports the new dims yet keeps
    /// returning the pre-resize row bodies. Rebuilding the pool on a dims
    /// change hands the next walk a clean cache. `None` until the first walk.
    last_dims: Option<(u16, u16)>,
}

impl<'alloc> SnapshotSynthesizer<'alloc> {
    /// Allocate a fresh pool of render iterators. Do this once per pane.
    pub fn new() -> Result<Self, SynthesisError> {
        Ok(Self {
            render_state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            tick_rows: Vec::new(),
            tick_epilogue: Vec::new(),
            tick_screen_toggle: Vec::new(),
            last_dims: None,
        })
    }

    /// Rebuild the pooled `render_state` + iterators when `terminal`'s
    /// dimensions have changed since the last pooled walk (phux-5pyx).
    ///
    /// Every pooled-state path ([`Self::prepare_tick`],
    /// [`Self::synthesize_incremental`], [`Self::mark_synced`]) calls this
    /// immediately before `self.render_state.update`. After a resize the pooled
    /// cache can serve stale row bodies (see [`Self::last_dims`]); a fresh pool
    /// has no prior cache, so its first `update` observes every row as it is now —
    /// the same root fix the fresh-per-call `GET_SCREEN` path
    /// ([`Self::screen_state_with_scrollback`]) uses, scoped here to the rare
    /// resize tick instead of every call.
    fn refresh_pool_on_resize(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
    ) -> Result<(), SynthesisError> {
        let live = (terminal.cols()?, terminal.rows()?);
        if self.last_dims != Some(live) {
            self.render_state = RenderState::new()?;
            self.rows = RowIterator::new()?;
            self.cells = CellIterator::new()?;
            self.last_dims = Some(live);
        }
        Ok(())
    }

    /// Walk `terminal`'s viewport and emit a VT byte sequence that
    /// reproduces it on a fresh Terminal.
    ///
    /// Returns the synthesised bytes plus the queried `(cols, rows)`
    /// dimensions, since `TERMINAL_SNAPSHOT` carries them alongside the
    /// replay body (SPEC §8.4).
    #[allow(
        clippy::unused_self,
        reason = "a full snapshot is intentionally stateless — it builds a fresh \
                  RenderState each call (phux-uow0) — but stays a method on \
                  SnapshotSynthesizer for API symmetry with the incremental paths"
    )]
    pub fn synthesize(
        &self,
        terminal: &GhosttyTerminal<'alloc, '_>,
    ) -> Result<SnapshotBytes, SynthesisError> {
        self.synthesize_bounded(terminal, usize::MAX)
    }

    /// Synthesize without allowing the output buffer to exceed `max_bytes`.
    pub fn synthesize_bounded(
        &self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        max_bytes: usize,
    ) -> Result<SnapshotBytes, SynthesisError> {
        // phux-uow0: a full snapshot must observe the LIVE grid in its
        // entirety, so it uses a FRESH `RenderState` + iterators rather than
        // the synthesizer's reused state.
        let mut render_state = RenderState::new()?;
        let mut rows = RowIterator::new()?;
        let mut cells = CellIterator::new()?;

        let snapshot = render_state.update(terminal)?;
        let cols = snapshot.cols()?;
        let rows_n = snapshot.rows()?;
        let requested = usize::from(cols)
            .checked_mul(usize::from(rows_n))
            .and_then(|cells| cells.checked_mul(2))
            .unwrap_or(max_bytes);
        let mut out = BoundedSnapshotBytes::with_capacity(max_bytes, requested)?;

        out.write_all(b"\x1b[!p\x1b[2J\x1b[H")
            .map_err(|_| SynthesisError::LimitExceeded)?;
        emit_screen_mode(&mut out.bytes, terminal)?;
        out.check()?;

        let mut prev_style: Option<Pen> = None;
        let mut row_iter = rows.update(&snapshot)?;
        let mut row_index: u16 = 0;
        while let Some(row) = row_iter.next() {
            if row_index >= rows_n {
                break;
            }
            write_cup(&mut out.bytes, row_index, 0);
            out.check()?;
            let mut cell_iter = cells.update(row)?;
            while let Some(cell) = cell_iter.next() {
                emit_cell_bounded(cell, &mut out, &mut prev_style)?;
            }
            row_index += 1;
        }

        emit_epilogue(&mut out.bytes, &snapshot, terminal)?;
        out.check()?;
        let mut kitty_placements = libghostty_vt::kitty::graphics::PlacementIterator::new()?;
        let _ = kitty_replay::emit_kitty_graphics_replay(
            terminal,
            &mut kitty_placements,
            &mut out,
            (0, 0),
            (cols, rows_n),
        )?;

        Ok(SnapshotBytes {
            cols,
            rows: rows_n,
            bytes: out.into_inner(),
            scrollback: Vec::new(),
        })
    }

    /// Like [`Self::synthesize`], but additionally primes the client's
    /// scrollback with up to `scrollback` retained history rows (`phux-9q5f`).
    ///
    /// `scrollback` follows the [`Self::screen_state_with_scrollback`]
    /// convention: `None` ⇒ viewport only (identical to [`Self::synthesize`]);
    /// [`SCROLLBACK_ALL`] (`0`) ⇒ every retained row; `Some(n)` ⇒ the most
    /// recent `n` rows.
    ///
    /// The history rows are emitted into [`SnapshotBytes::scrollback`] as the
    /// pane's plain text, one row per line, then a `CSI <k> S` (SU) scrolls
    /// them off the top into the client's scrollback so the viewport replay
    /// (`bytes`, which opens with `ED 2`) lands on a clean screen without
    /// erasing the most-recent history rows. The client applies `scrollback`
    /// then `bytes`. History styling is not reproduced in v1 (plain text only,
    /// tracked as a follow-up); the live viewport keeps full SGR fidelity.
    ///
    /// Alt-screen panes retain no history (`scrollback_rows() == 0`), so this
    /// degrades to a viewport-only snapshot there automatically.
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
        let mut snap = self.synthesize_bounded(terminal, max_bytes)?;
        let Some(want) = scrollback else {
            return Ok(snap);
        };
        let remaining = max_bytes
            .checked_sub(snap.bytes.len())
            .ok_or(SynthesisError::LimitExceeded)?;
        snap.scrollback =
            Self::scrollback_styled_bytes_bounded(terminal, want, snap.rows, remaining)?;
        Ok(snap)
    }

    /// Reproduce history rows `[start, total)` as a styled VT byte sequence:
    /// per-cell SGR deltas (via [`write_reset_and_sgr_unresolved`]) plus
    /// graphemes, rows joined by CRLF, terminated by an SGR reset and a
    /// `min(rows, history)` `SU` so the still-visible remainder scrolls off the
    /// top into the client's scrollback (leaving a blank viewport for the
    /// `bytes` replay). Empty when the pane has no history (alt-screen panes
    /// retain none). Side-effect-free: reads via `grid_ref(Point::History)`
    /// which neither scrolls nor mutates the canonical Terminal (phux-q0x7).
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
        let start = if want == SCROLLBACK_ALL {
            0
        } else {
            total.saturating_sub(usize::try_from(want).unwrap_or(usize::MAX))
        };
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
                let mut inline = [char::from(0u8); GRAPHEME_INLINE];
                match grid_ref.graphemes(&mut inline) {
                    Ok(0) => out
                        .write_all(b" ")
                        .map_err(|_| SynthesisError::LimitExceeded)?,
                    Ok(n) => encode_graphemes_bounded(&mut out, &inline[..n])?,
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
                        encode_graphemes_bounded(&mut out, &heap[..n])?;
                    }
                    Err(err) => return Err(err.into()),
                }
            }
            row_count += 1;
        }
        if row_count == 0 {
            return Ok(Vec::new());
        }
        out.write_all(b"\x1b[0m")
            .map_err(|_| SynthesisError::LimitExceeded)?;
        let visible = u16::try_from(row_count)
            .unwrap_or(viewport_rows)
            .min(viewport_rows);
        if visible > 0 {
            write!(out, "\x1b[{visible}S").map_err(|_| SynthesisError::LimitExceeded)?;
        }
        Ok(out.into_inner())
    }

    /// Walk `terminal`'s viewport into a structured [`ScreenState`] — the
    /// agent surface's read shape (ADR-0022 §2, `phux-oki`).
    ///
    /// Unlike [`Self::synthesize`], this does not emit VT bytes; it
    /// projects the grid to plain text rows + cursor, exactly what a
    /// reasoning agent (rather than a rendering terminal) wants. It runs
    /// on the server's own `Terminal`, so the read is side-effect-free —
    /// no attach, no resize. `pane` is the wire-local id stamped into the
    /// result for the caller.
    pub fn screen_state(
        &self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        pane: u32,
    ) -> Result<ScreenState, SynthesisError> {
        self.screen_state_with_scrollback(terminal, pane, None, false)
    }

    /// Like [`Self::screen_state`], but additionally projects up to
    /// `scrollback` rows of history *above* the viewport into the
    /// [`ScreenState::scrollback`] field (`phux-o1v`).
    ///
    /// `scrollback` semantics:
    /// - `None` — viewport only; `scrollback` is left empty (identical to
    ///   [`Self::screen_state`]).
    /// - `Some(0)` (the [`SCROLLBACK_ALL`] sentinel) — every retained
    ///   history row (the bare `--scrollback` flag).
    /// - `Some(n)` — the most-recent `n` history rows (those nearest the
    ///   viewport); fewer if less history exists.
    ///
    /// History is read cell-by-cell via `Terminal::grid_ref` with
    /// `Point::History` coordinates. That path is side-effect-free: it
    /// neither scrolls the viewport nor mutates the canonical Terminal,
    /// so the read stays safe to poll against a live pane. The viewport
    /// walk is unchanged from [`Self::screen_state`] and still uses the
    /// pooled render iterators.
    ///
    /// When `cells` is `true`, the viewport walk additionally collects a
    /// sparse [`ScreenState::cells`] vec: per-cell OSC-133 semantic marks
    /// (via `Cell::semantic_content`) and styles (via `CellIteration::style`
    /// plus the resolved foreground/background). Only cells with a
    /// non-default style or a semantic mark are emitted, in row-major order,
    /// skipping wide-cell tails — see the private `collect_cell`. When `false`,
    /// `cells` is left `None` and the walk pays nothing (`phux-8yl`).
    #[allow(
        clippy::unused_self,
        reason = "intentionally stateless — reads through a fresh RenderState \
                  each call (the pooled cache served stale rows after a resize; \
                  see the body comment) — but stays a method on \
                  SnapshotSynthesizer for API symmetry, matching `synthesize`"
    )]
    pub fn screen_state_with_scrollback(
        &self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        pane: u32,
        scrollback: Option<u32>,
        cells: bool,
    ) -> Result<ScreenState, SynthesisError> {
        // Read history first, before borrowing `render_state` for the
        // viewport snapshot: `grid_ref` borrows `terminal` immutably and
        // its references are invalidated by the next terminal operation,
        // so we read each row's text eagerly into owned `String`s here.
        let scrollback_lines = match scrollback {
            None => Vec::new(),
            Some(want) => Self::scrollback_lines(terminal, want)?,
        };

        // Fresh render state + iterators per call, NOT the pooled
        // `self.render_state`/`self.rows`/`self.cells`. The pooled state
        // can serve stale rows: after a `TERMINAL_RESIZE` raced an
        // attach/resync snapshot (which walks the grid through its own
        // fresh state), the pooled cache reported the new dims yet kept
        // returning the pre-write (empty) row bodies for every later
        // update — a `GET_SCREEN` poller then never saw content that a
        // fresh state read back correctly microseconds later (the
        // route_input_no_resize CI flake; same failure class as the
        // attach_detach_churn flakes fixed in `synthesize`, phux-uow0).
        // A fresh state has no prior cache, so its first `update`
        // observes every row as it is now. GET_SCREEN is an agent-paced
        // control call (a few Hz), so the extra FFI allocation is noise.
        let mut render_state = RenderState::new()?;
        let mut rows_pool = RowIterator::new()?;
        let mut cells_pool = CellIterator::new()?;
        let snapshot = render_state.update(terminal)?;
        let cols = snapshot.cols()?;
        let rows_n = snapshot.rows()?;

        let cursor = snapshot.cursor_viewport()?.map(|c| CursorState {
            x: c.x,
            y: c.y,
            visible: snapshot.cursor_visible().unwrap_or(true),
        });

        // Only allocate the cells vec when the caller asked; the common
        // `--cells`-absent snapshot pays nothing.
        let mut cell_infos: Option<Vec<CellInfo>> = cells.then(Vec::new);

        let mut lines: Vec<String> = Vec::with_capacity(usize::from(rows_n));
        let mut row_iter = rows_pool.update(&snapshot)?;
        let mut row_index: u16 = 0;
        while let Some(row) = row_iter.next() {
            if row_index >= rows_n {
                break;
            }
            let mut buf = String::with_capacity(usize::from(cols));
            let mut col_index: u16 = 0;
            let mut cell_iter = cells_pool.update(row)?;
            while let Some(cell) = cell_iter.next() {
                let wide = cell.raw_cell()?.wide()?;
                if matches!(wide, CellWide::SpacerTail) {
                    // Wide-cell tail: the base glyph spans both columns and
                    // advances col_index by its display width below, so skip
                    // the tail for both the text and the cells projection
                    // (no column emitted).
                    continue;
                }
                if let Some(infos) = cell_infos.as_mut()
                    && let Some(info) = collect_cell(cell, row_index, col_index)?
                {
                    infos.push(info);
                }
                let graphemes = cell.graphemes()?;
                if graphemes.is_empty() {
                    buf.push(' ');
                } else {
                    buf.extend(graphemes);
                }
                // Advance by the cell's display width so a styled/marked cell
                // to the right of a double-width (CJK/emoji) glyph reports the
                // true grid column — the same space cursor.x lives in. A Wide
                // base occupies two columns; its SpacerTail is skipped above.
                col_index =
                    col_index.saturating_add(if matches!(wide, CellWide::Wide) { 2 } else { 1 });
            }
            lines.push(buf.trim_end().to_owned());
            row_index += 1;
        }

        Ok(ScreenState {
            schema_version: SCHEMA_VERSION,
            pane,
            cols,
            rows: rows_n,
            cursor,
            lines,
            scrollback: scrollback_lines,
            cells: cell_infos,
        })
    }

    /// Read the history (scrollback) rows above the active viewport into
    /// owned, right-trimmed strings, oldest first.
    ///
    /// `want` follows the [`Self::screen_state_with_scrollback`] convention:
    /// [`SCROLLBACK_ALL`] (`0`) means every retained history row, any other
    /// value caps the result to the most-recent `want` rows.
    ///
    /// Each cell is read via [`libghostty_vt::Terminal::grid_ref`] in the
    /// [`Point::History`] coordinate space, mirroring the viewport walk's
    /// wide-cell-tail handling (`SpacerTail` cells advance no column and
    /// are skipped). History coordinates are local to the history region:
    /// `y = 0` is the oldest retained row, `y = scrollback_rows - 1` is the
    /// row just above the viewport.
    fn scrollback_lines(
        terminal: &GhosttyTerminal<'alloc, '_>,
        want: u32,
    ) -> Result<Vec<String>, SynthesisError> {
        let total = terminal.scrollback_rows()?;
        if total == 0 {
            return Ok(Vec::new());
        }
        let cols = terminal.cols()?;

        // Resolve the [start, total) window of history rows to emit. For a
        // bounded request we keep the rows nearest the viewport (the most
        // recent history), which is what an agent reading "the last N lines
        // of scrollback" expects.
        let start = if want == SCROLLBACK_ALL {
            0
        } else {
            total.saturating_sub(usize::try_from(want).unwrap_or(usize::MAX))
        };

        let mut out: Vec<String> = Vec::with_capacity(total - start);
        for y in start..total {
            // History `y` is a `u32` in libghostty's coordinate space.
            // `total` comes from `scrollback_rows()` (also originally a C
            // count); clamp defensively rather than truncate.
            let y = u32::try_from(y).unwrap_or(u32::MAX);
            let mut buf = String::with_capacity(usize::from(cols));
            for x in 0..cols {
                let point = Point::History(PointCoordinate { x, y });
                let grid_ref = terminal.grid_ref(point)?;
                if matches!(grid_ref.cell()?.wide()?, CellWide::SpacerTail) {
                    continue;
                }
                // Read the grapheme cluster; an empty cluster is a blank
                // cell, which advances one column with a space. A cluster
                // longer than the inline buffer (deep combining sequence)
                // surfaces as `OutOfSpace { required }`; retry on the heap.
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
            }
            out.push(buf.trim_end().to_owned());
        }
        Ok(out)
    }

    /// Mark this consumer's `RenderState` as fully in sync with the
    /// canonical Terminal — clears the snapshot-level dirty state and
    /// every per-row dirty bit.
    ///
    /// Per ADR-0018 (Lazy state synchronization), this is the operation
    /// the tick driver (phux-q0e.3) invokes when a `FRAME_ACK` for the
    /// matching `seq` arrives from the consumer. It is deliberately
    /// **not** called inside [`Self::synthesize_incremental`]: an unacked
    /// diff must remain re-emittable so a lost packet causes the next
    /// tick to re-diff against the same older reference rather than
    /// returning a Clean-but-incorrect empty body.
    ///
    /// After this returns successfully, the next `synthesize_incremental`
    /// against an unchanged terminal will observe `Dirty::Clean` and emit
    /// an empty body, saving wire bytes.
    pub fn mark_synced(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
    ) -> Result<(), SynthesisError> {
        self.refresh_pool_on_resize(terminal)?;
        let snapshot = self.render_state.update(terminal)?;
        let rows_n = snapshot.rows()?;
        // Walk rows and clear each dirty bit. The row-level clear is
        // separate from the snapshot-level clear — see render.h's "Dirty
        // Tracking" section: both must be reset to bring this consumer
        // back to Clean on the next `update`.
        let mut row_iter = self.rows.update(&snapshot)?;
        let mut row_index: u16 = 0;
        while let Some(row) = row_iter.next() {
            if row_index >= rows_n {
                break;
            }
            row.set_dirty(false)?;
            row_index += 1;
        }
        snapshot.set_dirty(Dirty::Clean)?;
        Ok(())
    }

    /// Synthesize the **incremental** VT diff: the bytes that, applied via
    /// `vt_write` to a mirror that's in sync with the per-consumer
    /// `RenderState`'s last-acked reference, advance the mirror to match
    /// the canonical Terminal now.
    ///
    /// Per ADR-0018 (Lazy state synchronization) and its 2026-05-26
    /// Addendum, this is the per-tick emission primitive. It follows the
    /// 5-step algorithm from `research/archive/2026-05-26-state-sync-algorithm.md`
    /// Dependencies §2:
    ///
    /// 1. `render_state.update(terminal)` to refresh dirty state.
    /// 2. Consult [`Snapshot::dirty`]:
    ///    - `Dirty::Clean` → empty `replay_bytes`.
    ///    - `Dirty::Full` → identical output to [`Self::synthesize`] (fall
    ///      back to the full reset + paint path).
    ///    - `Dirty::Partial` → walk rows, skip those with
    ///      `Row::dirty() == false`, CUP to each dirty row and emit the
    ///      same per-cell loop the full path uses.
    /// 3. Re-emit cursor position + visibility + visual style + mode bits.
    /// 4. **Do not clear dirty bits.** The tick driver (phux-q0e.3)
    ///    clears bits only when a `FRAME_ACK` arrives (phux-q0e.4). An
    ///    unacked diff must remain re-emittable; that is the loss-tolerance
    ///    invariant ADR-0018 rests on.
    pub fn synthesize_incremental(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
    ) -> Result<SnapshotBytes, SynthesisError> {
        self.refresh_pool_on_resize(terminal)?;
        let snapshot = self.render_state.update(terminal)?;
        let cols = snapshot.cols()?;
        let rows_n = snapshot.rows()?;

        let dirty = snapshot.dirty()?;
        match dirty {
            Dirty::Clean => Ok(SnapshotBytes {
                cols,
                rows: rows_n,
                bytes: Vec::new(),
                scrollback: Vec::new(),
            }),
            Dirty::Full => {
                // Full reset + paint everything. Identical bytes to the
                // existing [`Self::synthesize`] path; replicate the prologue
                // here rather than re-entering `synthesize` so we keep
                // `render_state` borrowed by `snapshot` for the row walk.
                let mut out: Vec<u8> =
                    Vec::with_capacity(usize::from(cols) * usize::from(rows_n) * 2);
                out.extend_from_slice(b"\x1b[!p\x1b[2J\x1b[H");
                // Select the screen buffer before painting (phux-99n).
                emit_screen_mode(&mut out, terminal)?;

                let mut prev_style: Option<Pen> = None;
                let mut row_iter = self.rows.update(&snapshot)?;
                let mut row_index: u16 = 0;
                while let Some(row) = row_iter.next() {
                    if row_index >= rows_n {
                        break;
                    }
                    write_cup(&mut out, row_index, 0);
                    let mut cell_iter = self.cells.update(row)?;
                    while let Some(cell) = cell_iter.next() {
                        emit_cell(cell, &mut out, &mut prev_style)?;
                    }
                    row_index += 1;
                }

                emit_epilogue(&mut out, &snapshot, terminal)?;
                Ok(SnapshotBytes {
                    cols,
                    rows: rows_n,
                    bytes: out,
                    scrollback: Vec::new(),
                })
            }
            Dirty::Partial => {
                // Walk rows; emit only those whose `Row::dirty() == true`.
                // No reset preamble — the mirror's state outside the dirty
                // rows is unchanged.
                let mut out: Vec<u8> = Vec::with_capacity(usize::from(cols) * usize::from(rows_n));
                let mut prev_style: Option<Pen> = None;
                let mut row_iter = self.rows.update(&snapshot)?;
                let mut row_index: u16 = 0;
                while let Some(row) = row_iter.next() {
                    if row_index >= rows_n {
                        break;
                    }
                    if !row.dirty()? {
                        row_index += 1;
                        continue;
                    }
                    write_cup(&mut out, row_index, 0);
                    let mut cell_iter = self.cells.update(row)?;
                    while let Some(cell) = cell_iter.next() {
                        emit_cell(cell, &mut out, &mut prev_style)?;
                    }
                    row_index += 1;
                }

                // Always re-emit the cursor + mode epilogue. Cursor
                // position can change without any row being marked dirty
                // (e.g. a bare CUP into a position whose cell is
                // unchanged), and mode bits are diffed flat against the
                // mirror's state, so we re-emit them on every non-empty
                // tick to keep the algorithm simple. This matches the
                // research note's step 3 + 4.
                emit_epilogue(&mut out, &snapshot, terminal)?;

                // CRITICAL: do not call `snapshot.set_dirty(Clean)` or
                // `row.set_dirty(false)` here. The tick driver clears
                // bits only when FRAME_ACK arrives; an unacked diff must
                // stay re-emittable so the next tick can re-diff against
                // the same older reference if this packet is lost.

                Ok(SnapshotBytes {
                    cols,
                    rows: rows_n,
                    bytes: out,
                    scrollback: Vec::new(),
                })
            }
        }
    }

    /// Synthesize the per-consumer incremental diff by comparing the live
    /// `terminal` against a caller-owned [`ConsumerReference`] (phux-ia4).
    ///
    /// # Why this exists (the per-consumer dirty-isolation fix)
    ///
    /// libghostty's `RenderState::update` **consumes** the shared
    /// `Terminal`'s dirty state: it clears `t.flags.dirty`, the active
    /// screen's dirty flags, and the per-page / per-row dirty bits
    /// (`render.zig` `update`, lines ~440-461 and ~647-648 of the pinned
    /// `acc4b87` checkout). A `RenderState`'s own `Snapshot::dirty()` /
    /// `Row::dirty()` are only *populated* from those shared bits during
    /// `update`. So with N consumers sharing one pane, the FIRST
    /// consumer's `update` in a tick consumes the shared dirty bits and
    /// every OTHER consumer's `update` that tick observes `Dirty::Clean`
    /// — starving all-but-one. [`Self::synthesize_incremental`] (which
    /// reads `Snapshot::dirty()`) is therefore only correct for a single
    /// consumer per tick.
    ///
    /// This method sidesteps the shared dirty bits entirely. It renders
    /// each viewport row's cell body into bytes and compares it against
    /// the per-consumer reference's stored row body. Rows whose rendered
    /// body differs from the reference are re-emitted (CUP + cells);
    /// unchanged rows are skipped. The reference is independent per
    /// consumer, so consumers that have diverged (different ack/sync
    /// points, dropped frames) each get their own correct diff regardless
    /// of what any other consumer did to the shared `Terminal` this tick.
    ///
    /// # Emit-once semantics
    ///
    /// On a non-empty diff this advances `reference` to the just-rendered
    /// state *before returning the bytes* (the caller commits by shipping
    /// the frame). A given change is therefore emitted exactly once and
    /// not re-emitted on subsequent ticks until the content changes again.
    /// This matches the v0.1 reliable-transport emission model (the
    /// broadcast pump ships each PTY byte once) and keeps a non-acking
    /// consumer from re-receiving the same diff every tick. The
    /// loss-tolerance "re-diff against an older reference" property
    /// (ADR-0018) belongs to the future lossy-transport path and is not
    /// v0.1 normative (proto.md §8); `FRAME_ACK` remains wired for
    /// backpressure accounting and forward compatibility.
    ///
    /// Returns the synthesized bytes plus the queried `(cols, rows)`. An
    /// empty body means the viewport is byte-identical to the reference.
    pub fn synthesize_against_reference(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        reference: &mut ConsumerReference,
    ) -> Result<SnapshotBytes, SynthesisError> {
        // phux-ahk.2: render the tick once (consumer-independent), then diff
        // this single consumer against it. `tick_emit` uses `prepare_tick` +
        // `diff_consumer` directly so a pane with N consumers renders ONCE;
        // this wrapper keeps the original one-call API for single-consumer
        // callers and the unit tests.
        let (cols, rows_n, live_cm) = self.prepare_tick(terminal)?;
        Ok(self.diff_consumer(cols, rows_n, live_cm, reference))
    }

    /// phux-ahk.2: render the current grid ONCE per tick into the shared
    /// `tick_*` buffers; returns the consumer-independent `(cols, rows,
    /// live_cm)`. Each consumer's [`Self::diff_consumer`] then diffs against
    /// these buffers, so a pane with N state-sync consumers renders the grid
    /// once (not N times) and runs the cursor/mode FFI once (not 5N times).
    ///
    /// Hoisted here (all consumer-independent): the `RenderState::update`
    /// snapshot, the full per-row cell render, the cursor/mode capture, and
    /// the epilogue + screen-toggle byte precompute. Both the epilogue and the
    /// screen-toggle reflect live state and are byte-identical for every
    /// consumer; only *whether* to emit the screen toggle is per-consumer and
    /// stays in [`Self::diff_consumer`].
    pub(crate) fn prepare_tick(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
    ) -> Result<(u16, u16, ReferenceCursorMode), SynthesisError> {
        let _span = tracing::debug_span!("prepare_tick").entered();
        self.refresh_pool_on_resize(terminal)?;
        let snapshot = self.render_state.update(terminal)?;
        let cols = snapshot.cols()?;
        let rows_n = snapshot.rows()?;

        // Size the shared row buffer to the grid and clear every in-range
        // buffer (capacity retained) so a row the iterator does not yield
        // can't leave stale content from a prior tick.
        let rows_usize = usize::from(rows_n);
        if self.tick_rows.len() < rows_usize {
            self.tick_rows.resize_with(rows_usize, Vec::new);
        } else {
            self.tick_rows.truncate(rows_usize);
        }
        for body in &mut self.tick_rows {
            body.clear();
        }

        // Render each row body into its shared buffer with a fresh SGR pen so
        // the per-row byte sequence is self-contained and comparable across
        // ticks regardless of neighbouring rows.
        {
            let tick_rows = &mut self.tick_rows;
            let mut row_iter = self.rows.update(&snapshot)?;
            let mut row_index: usize = 0;
            while let Some(row) = row_iter.next() {
                if row_index >= rows_usize {
                    break;
                }
                let body = &mut tick_rows[row_index];
                let mut prev_style: Option<Pen> = None;
                let mut cell_iter = self.cells.update(row)?;
                while let Some(cell) = cell_iter.next() {
                    emit_cell(cell, body, &mut prev_style)?;
                }
                row_index += 1;
            }
        }

        // Cursor/mode + epilogue + screen toggle: consumer-independent, so
        // capture/precompute them once while the snapshot is live.
        let live_cm = ReferenceCursorMode::capture(&snapshot, terminal)?;
        self.tick_epilogue.clear();
        emit_epilogue(&mut self.tick_epilogue, &snapshot, terminal)?;
        self.tick_screen_toggle.clear();
        emit_screen_mode(&mut self.tick_screen_toggle, terminal)?;
        Ok((cols, rows_n, live_cm))
    }

    /// phux-ahk.2: diff one consumer against the shared `tick_*` buffers
    /// produced by the preceding [`Self::prepare_tick`]. Advances the
    /// consumer's reference (emit-once: the reference reflects the rendered
    /// state before the frame ships) and returns the per-consumer delta. An
    /// empty body means the consumer is byte-identical to the rendered tick.
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
        // A dimension change clears the reference so every row repaints (a
        // mid-resize diff falls back to a full repaint rather than a stale
        // partial diff; the resize resync path emits a fresh snapshot anyway).
        if reference.cols != cols || reference.rows != rows_n {
            reference.reset_geometry(cols, rows_n);
        }

        // Diff each rendered row against the reference and commit changed rows
        // into the reference. Unlike the prior single-consumer swap, this
        // copies (clone) because `tick_rows` is shared and must survive for
        // the other consumers in this tick. `tick_rows` and `rows_body` are
        // both `rows_n` long (prepare_tick + reset_geometry), so the zip walks
        // every row.
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

        let mut out: Vec<u8> = Vec::new();
        // Screen-buffer toggle FIRST, only on an actual alt-screen transition
        // for THIS consumer (phux-99n ordering). The toggle bytes were
        // precomputed in `prepare_tick`.
        if reference.cursor_mode.alt_screen_set() != live_cm.alt_screen_set() {
            out.extend_from_slice(&self.tick_screen_toggle);
        }
        let ConsumerReference {
            rows_body,
            changed_scratch: changed,
            ..
        } = &*reference;
        for &ri in changed {
            write_cup(&mut out, ri, 0);
            // Reset SGR at the start of each emitted row so the row body
            // (which itself starts from a fresh pen) lands on a clean pen.
            out.extend_from_slice(b"\x1b[0m");
            out.extend_from_slice(&rows_body[usize::from(ri)]);
        }
        // Always re-emit the cursor/mode epilogue on a non-empty tick (the
        // changed rows moved the cursor as a side effect of painting).
        out.extend_from_slice(&self.tick_epilogue);
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

    /// phux-v45.8 / ADR-0042: diff the current tick's rendered rows against
    /// `base` **without advancing `base`**, emitting every row that differs
    /// from it (an absolute, cumulative delta). This is the loss-tolerant
    /// emission primitive: `base` is the consumer's last-*acked* reference, so
    /// each tick re-diffs from the last state the consumer provably has, and a
    /// dropped/un-acked frame self-heals because the next emission re-includes
    /// its rows (they still differ from `base`).
    ///
    /// Requires a preceding [`Self::prepare_tick`] this tick (the shared
    /// `tick_rows` / `tick_epilogue` / `tick_screen_toggle` buffers hold the
    /// rendered grid). Unlike [`Self::diff_consumer`] it does not mutate the
    /// reference — the caller advances the acked reference only when a
    /// `FRAME_ACK` lands (via [`Self::snapshot_tick_reference`]). Each emitted
    /// row is repainted in full (`CUP` + SGR reset + row body), so applying the
    /// delta is idempotent regardless of which intermediate deltas the consumer
    /// did or did not receive: the only requirement is that the consumer's
    /// mirror is at some grid on the `base`→live path (guaranteed on the
    /// reliable-delivery-with-drops model — the relay drops whole frames but
    /// never reorders delivered ones; see ADR-0042 for the residual bound).
    ///
    /// Returns the delta bytes; empty when live is byte-identical to `base`.
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

        // Absolute changed-row set: any row whose freshly rendered body differs
        // from the acked reference (or every row on a geometry mismatch — the
        // acked reference is a different size, so the whole viewport repaints).
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

        let mut out: Vec<u8> = Vec::new();
        if screen_changed {
            out.extend_from_slice(&self.tick_screen_toggle);
        }
        for &ri in &changed {
            write_cup(&mut out, ri, 0);
            out.extend_from_slice(b"\x1b[0m");
            out.extend_from_slice(&self.tick_rows[usize::from(ri)]);
        }
        out.extend_from_slice(&self.tick_epilogue);
        span.record("changed_row_count", changed.len());
        span.record("out_bytes", out.len());
        out
    }

    /// phux-v45.8 / ADR-0042: snapshot the current tick's rendered grid into a
    /// standalone [`ConsumerReference`] so a loss-tolerant consumer can advance
    /// its *acked* reference to exactly the grid state a later `FRAME_ACK`
    /// covers. Requires a preceding [`Self::prepare_tick`] this tick.
    ///
    /// The returned reference holds a clone of the rendered row bodies plus the
    /// cursor/mode capture; it is the diff base a subsequent
    /// [`Self::diff_against_base`] uses once the matching ack lands.
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
    /// any bytes (phux-ia4). Used at consumer registration so the first
    /// `synthesize_against_reference` only reports deltas that occur
    /// *after* attach (the `TERMINAL_SNAPSHOT` already brought the
    /// consumer's mirror to this same reference point).
    pub fn prime_reference(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        reference: &mut ConsumerReference,
    ) -> Result<(), SynthesisError> {
        // A full ATTACH snapshot immediately precedes this call and deliberately
        // uses a fresh RenderState. That walk consumes libghostty's terminal-wide
        // dirty bits, so the pooled RenderState can legally return its older
        // cached rows here. Prime from another fresh walk: the bootstrap bytes
        // and this reference then observe the same canonical actor cut even when
        // a previous consumer populated the pool.
        let mut render_state = RenderState::new()?;
        let mut rows = RowIterator::new()?;
        let mut cells = CellIterator::new()?;
        let snapshot = render_state.update(terminal)?;
        let cols = snapshot.cols()?;
        let rows_n = snapshot.rows()?;
        reference.reset_geometry(cols, rows_n);

        let mut row_iter = rows.update(&snapshot)?;
        let mut row_index: u16 = 0;
        while let Some(row) = row_iter.next() {
            if row_index >= rows_n {
                break;
            }
            let mut body: Vec<u8> = Vec::with_capacity(usize::from(cols));
            let mut prev_style: Option<Pen> = None;
            let mut cell_iter = cells.update(row)?;
            while let Some(cell) = cell_iter.next() {
                emit_cell(cell, &mut body, &mut prev_style)?;
            }
            reference.rows_body[usize::from(row_index)] = body;
            row_index += 1;
        }
        reference.cursor_mode = ReferenceCursorMode::capture(&snapshot, terminal)?;
        Ok(())
    }
}

/// Convenience wrapper: allocate a fresh [`SnapshotSynthesizer`] for a
/// one-shot synthesis. Per-pane hot loops should reuse a
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
    /// Optional scrollback-priming VT bytes (`phux-9q5f`). When the ATTACH
    /// requested scrollback, these reproduce the pane's retained history rows
    /// on the client's fresh `Terminal` *before* `bytes` repaints the
    /// viewport — the client `vt_write`s `scrollback` then `bytes`, per
    /// SPEC §8.4. Empty when no scrollback was requested or none is retained.
    pub scrollback: Vec<u8>,
}

/// The active SGR pen tracked across cells: the libghostty [`Style`] plus the
/// resolved foreground/background.
///
/// Colors are part of the key (not just the style's attribute flags) because
/// adjacent cells that differ *only* in color — multi-color `ls`, syntax
/// highlighting, a p10k prompt — must re-emit an SGR delta, or the second
/// color is lost. The live path forwards raw PTV bytes (byte-faithful), so a
/// color-only gate miss surfaced only as a color glitch right after the
/// attach/resize snapshot resync.
type Pen = (Style, Option<RgbColor>, Option<RgbColor>);

/// Per-cell emission shared by the full ([`SnapshotSynthesizer::synthesize`])
/// and incremental ([`SnapshotSynthesizer::synthesize_incremental`]) paths.
///
/// Tracks the active SGR pen via `prev` (see [`Pen`]), skips wide-cell tails
/// (`CellWide::SpacerTail`, see the comment in the body), and emits the
/// cell's grapheme cluster (or a space for genuinely-blank cells).
fn emit_cell_bounded(
    cell: &CellIteration<'_, '_>,
    out: &mut BoundedSnapshotBytes,
    prev: &mut Option<Pen>,
) -> Result<(), SynthesisError> {
    if matches!(cell.raw_cell()?.wide()?, CellWide::SpacerTail) {
        return Ok(());
    }
    let len = cell.graphemes_len()?;
    let style = cell.style()?;
    let fg = cell.fg_color()?;
    let bg = cell.bg_color()?;
    let pen = (style, fg, bg);
    if prev.as_ref() != Some(&pen) {
        write_reset_and_sgr(&mut out.bytes, &style, fg, bg);
        out.check()?;
        *prev = Some(pen);
    }
    if len == 0 {
        out.write_all(b" ")
            .map_err(|_| SynthesisError::LimitExceeded)?;
        return Ok(());
    }
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

    // Discriminate wide-cell tails (the right half of a double-width
    // glyph) from genuinely-blank cells. The base grapheme on the wide
    // cell already advanced the cursor across both columns, so the tail
    // must NOT emit a space (which would clobber the right half of the
    // wide glyph). See libghostty's `CellWide`: `SpacerTail` is
    // documented as "do not render".
    let wide = cell.raw_cell()?.wide()?;
    if matches!(wide, CellWide::SpacerTail) {
        return Ok(());
    }

    let len = cell.graphemes_len()?;

    // Reproduce the cell's pen (attributes + resolved fg/bg) whenever it
    // differs from the previous cell. This runs for blank cells too: a
    // colored-but-glyphless cell (a `colorcolumn` fill, a statusline tail, a
    // p10k right-prompt pad) must carry its background, and emitting the space
    // before reading the style — as this did previously — dropped that
    // background on the synthesized snapshot.
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

    // Read the grapheme cluster into a stack buffer rather than the
    // allocating [`CellIteration::graphemes`] (`vec!['\0'; len]` per cell).
    // The emit path visits every cell of every changed row each tick under
    // heavy output; a heap allocation per cell dominated the hot path
    // (~50 allocations per row in the bursty-colored-output stress probe).
    // `GRAPHEME_INLINE` covers the common base-codepoint-plus-a-few-marks
    // case; deeper clusters fall back to a one-shot heap retry.
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

/// Project one viewport cell into a [`CellInfo`], or `None` when the cell
/// carries neither a non-default style nor an OSC-133 semantic mark
/// (`phux-8yl`).
///
/// Returning `None` for plain cells keeps the [`ScreenState::cells`] vec
/// sparse: a mostly-blank grid emits almost nothing, so the JSON stays
/// small while every styled or semantically-marked cell is still reported.
/// `row`/`col` are the viewport-relative, zero-based coordinates of the
/// cell's left edge (wide-cell tails are skipped by the caller).
///
/// This is one of three copies of the libghostty cell projection; the other
/// two are `phux-client`'s `attach::render`'s `to_cell_style` and
/// `phux-record`'s `replay::project_cell`. The duplication is deliberate —
/// sharing would force `phux-core` to depend on `libghostty-vt` — and
/// `crates/phux/tests/cell_projection_conformance.rs` is what keeps the three
/// honest, running one VT corpus through all of them (`phux-h5hj.2`). That
/// test also pins the two places this projection CANNOT match the dense ones:
/// it emits no entry for a wide glyph's spacer tail, and it alone carries
/// OSC-133 semantics.
fn collect_cell(
    cell: &CellIteration<'_, '_>,
    row: u16,
    col: u16,
) -> Result<Option<CellInfo>, SynthesisError> {
    let style = cell.style()?;
    // libghostty defaults every cell's semantic content to `Output`,
    // whether or not the shell emitted any OSC-133 marks — so `Output` is
    // the *absence* of a meaningful mark, not a signal. Collapse it to
    // `None` and surface only `Input` / `Prompt`, the marks an agent can
    // actually act on; this also keeps the cells projection sparse (a grid
    // with no shell integration emits no semantic field at all).
    let semantic = match cell.raw_cell()?.semantic_content()? {
        CellSemanticContent::Output => None,
        CellSemanticContent::Input => Some(SemanticContent::Input),
        CellSemanticContent::Prompt => Some(SemanticContent::Prompt),
    };

    // Resolve fg/bg via the iteration's color helpers (which apply the
    // palette/default), falling back to the raw `StyleColor` so a palette
    // index survives as a palette index in the projection.
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

/// Project a cell color to [`CellColor`].
///
/// Prefers the cell's explicit per-cell [`StyleColor`] so a palette index
/// keeps its identity (`Palette { index }`) rather than collapsing to RGB.
/// When the cell sets no explicit color (`StyleColor::None`) but the
/// iteration still resolves a concrete RGB (`resolved` — e.g. a non-default
/// background inherited from the terminal palette), that RGB is surfaced;
/// otherwise the projection is [`CellColor::Default`].
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

/// Post-row-walk epilogue shared by both synthesis paths: reset SGR,
/// re-establish cursor position + visibility + visual style, and replay
/// the load-bearing mode bits queried from the canonical
/// [`libghostty_vt::Terminal`].
///
/// Identical to the tail of `synthesize` from before the
/// full/incremental split was introduced.
///
/// # Snapshot/resync fidelity is grid-content-authoritative by design (phux-e3mo)
///
/// This epilogue is the SHARED contract between the full-snapshot path and
/// the per-consumer state-sync diff. Two known low-severity gaps are
/// intentionally NOT corrected here:
///
/// * Post-snapshot SGR continuity: the snapshot epilogue resets SGR to 0
///   and the live broadcast pump then resumes raw relative PTY deltas;
///   there is no guarantee the client mirror's pen matches the server's at
///   the snapshot boundary (a plausible narrow-window color glitch right
///   after attach that self-heals on the next prompt redraw).
/// * Off-viewport cursor: a snapshot whose cursor is not in the viewport
///   homes to `ESC[H` (a possible top-left flash) rather than hiding it.
///
/// Both self-heal on the next live frame. Correcting them means changing
/// this shared contract — which the state-sync path also depends on — and
/// is deliberately deferred until there is a reproducing capture (e.g. a
/// real p10k session) proving the mechanism, rather than churning a
/// load-bearing path speculatively. Snapshot fidelity is grid-content
/// authoritative; transient pen/cursor reconvergence is the live stream's
/// job.
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

    // Remaining load-bearing mode bits. Bracketed paste and focus-event
    // reporting are independent of the screen buffer, so their order
    // relative to the cursor does not matter. The alt-screen modes are
    // NOT emitted here — they must precede the row paint (see
    // [`emit_screen_mode`]), or the content lands on the wrong buffer and
    // a `?1049h` after it would clear what we just painted.
    emit_mode(out, terminal, Mode::BRACKETED_PASTE, b"2004")?;
    emit_mode(out, terminal, Mode::FOCUS_EVENT, b"1004")?;
    emit_mouse_modes(out, terminal)?;
    Ok(())
}

/// Emit the mouse-reporting DEC modes: tracking level (9 / 1000 / 1002 /
/// 1003), report encoding (1005 / 1006 / 1015 / 1016), and the wheel policy
/// bit (1007, xterm "alternate scroll").
///
/// These are screen-independent, so they ride the epilogue next to bracketed
/// paste and focus-event reporting.
///
/// Load-bearing, not cosmetic (phux-mlo5). The client keeps
/// its own libghostty mirror and gates wheel handling on it: a pane whose
/// program asked for the mouse gets the wheel forwarded as `INPUT_MOUSE`,
/// and a pane that did not gets the local scrollback viewport scroll — or,
/// on the alt screen with 1007 set, wheel-to-arrow-key translation. A
/// snapshot that omits these bits leaves the mirror believing NO program
/// ever wanted the mouse, so every attach and every resync silently
/// downgraded scrolling in mouse-tracking TUIs (opencode, Claude Code,
/// htop, vim) to arrow keys — which those apps route to their editor/list
/// cursor rather than scrolling. The mode bytes are diffed flat against the
/// mirror, so they re-emit unconditionally on every non-empty tick like the
/// rest of the epilogue's mode bits.
fn emit_mouse_modes(
    out: &mut Vec<u8>,
    terminal: &GhosttyTerminal<'_, '_>,
) -> Result<(), SynthesisError> {
    for (mode, code) in MOUSE_MODES {
        emit_mode(out, terminal, mode, code)?;
    }
    Ok(())
}

/// The mouse-reporting DEC modes the epilogue replays, paired with their
/// numeric codes.
///
/// [`ReferenceCursorMode`] captures this same list (by index) so the diff
/// path re-emits the epilogue when any of them flips. Keep the two in step:
/// the array length is the reference field's width.
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
    // Wheel policy. libghostty defaults 1007 ON, so the interesting case is
    // an app that opted OUT (`?1007l`) and must not receive synthesized
    // arrow keys after a reattach.
    (Mode::ALT_SCROLL, b"1007"),
];

/// Emit the alt-screen DEC mode toggles (47 / 1047 / 1049) that select
/// which screen buffer subsequent content paints into.
///
/// libghostty tracks 47 (`ALT_SCREEN_LEGACY`), 1047 (`ALT_SCREEN`), and
/// 1049 (`ALT_SCREEN_SAVE`) as three independent bits; a full-screen
/// program (vim/less/man/htop/tmux) typically sets 1049, which on entry
/// saves the cursor and clears the alt buffer. Each is queried
/// independently so the synthesis reproduces the terminal's exact
/// alt-screen state rather than forcing the primary screen via a stale
/// `?47l`.
///
/// CRITICAL ordering: this MUST be emitted BEFORE the row paint and the
/// cursor re-establishment. `?1049h` clears the alt buffer and saves the
/// cursor on entry, so emitting it after painting would wipe the content
/// and clobber the restored cursor. Both the full-reset prologue and the
/// per-row diff therefore call this ahead of any cell bytes.
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

fn emit_cursor_style(out: &mut Vec<u8>, style: CursorVisualStyle, blinking: bool) {
    // DECSCUSR: `CSI <n> SP q`. Block/blink=1, Block/steady=2,
    // Underline/blink=3, steady=4, Bar/blink=5, steady=6. BlockHollow has
    // no DECSCUSR encoding; map to Block-steady.
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
    use libghostty_vt::{Terminal as GhosttyTerminal, TerminalOptions};

    fn fresh(cols: u16, rows: u16) -> GhosttyTerminal<'static, 'static> {
        GhosttyTerminal::new(TerminalOptions {
            cols,
            rows,
            max_scrollback: 100,
        })
        .expect("Terminal::new")
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
        let mut synth = SnapshotSynthesizer::new().expect("synth");
        assert!(matches!(
            synth.synthesize_bounded(&terminal, 4),
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

    /// phux-5pyx: behavioural lock for the resize → pooled-tick path.
    ///
    /// Drives two `prepare_tick`s across a resize, with a fresh-state walk
    /// (the `GET_SCREEN` path) interleaved, and asserts the second tick reports
    /// the new dims and the post-resize content. NOTE: this is a forward-
    /// looking correctness assertion, not a fail-without-the-fix guard — the
    /// original staleness was a timing-dependent shared-dirty-bit race that
    /// does not reproduce deterministically in a single-threaded unit test
    /// (the bead notes "No repro today"). It pins the contract the
    /// dims-change pool rebuild upholds for the `StateSync` consumers to come.
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
        let _ = synth.screen_state(&term, 0).expect("fresh walk");

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
        assert_eq!(synth.last_dims, Some((10, 4)), "pool tracks the live dims");
    }

    /// phux-9q5f: a scrollback-bearing snapshot, applied to a fresh client
    /// `Terminal` exactly as the wire client does (`vt_write(scrollback)` then
    /// `vt_write(bytes)`), reconstructs both the viewport and the retained
    /// history — every history row, none lost to the viewport replay's `ED 2`.
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
        let sb_synth = SnapshotSynthesizer::new().expect("synth2");
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

        let sb_synth = SnapshotSynthesizer::new().expect("synth2");
        let client_hist = sb_synth
            .screen_state_with_scrollback(&client, 0, Some(SCROLLBACK_ALL), false)
            .expect("client history")
            .scrollback;
        // Exactly the 2 most-recent history rows (line6, line7), no more.
        assert_eq!(client_hist, vec!["line6".to_owned(), "line7".to_owned()]);
        assert_eq!(render_grid(&client), render_grid(&source));
    }

    /// Walk the viewport of `t` and collect each row as a string,
    /// reproducing wide-cell tail handling so the comparison is grid-
    /// equivalent rather than byte-equivalent.
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

    /// A reconstructed cell's `(grapheme, fg, bg, underline, overline)` —
    /// enough to assert *color* fidelity, which `render_grid` (graphemes only)
    /// cannot.
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

    /// Drive `source` with `vt`, synthesize a full snapshot, replay it into a
    /// fresh client terminal exactly as the wire client does, and return the
    /// reconstructed first row's styled cells.
    fn round_trip_row0(vt: &[u8]) -> Vec<StyledCell> {
        let mut source = fresh(40, 4);
        source.vt_write(vt);
        let snap = synthesize(&source).expect("synth");
        let mut client = fresh(40, 4);
        client.vt_write(&snap.bytes);
        row0_styled(&client)
    }

    /// A pure color change between two same-attribute runs (red text then blue
    /// text, as `ls --color` / syntax highlighting emit) must survive the
    /// snapshot — the delta gate previously keyed on attribute flags only and
    /// dropped the second color, so it reappeared in the first run's color.
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

    /// A colored-but-blank region (e.g. a `colorcolumn` fill or p10k prompt
    /// pad: `\x1b[44m` then spaces) must keep its background through the
    /// snapshot. `emit_cell` previously pushed the space before reading the
    /// style, dropping the background of glyphless cells.
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
        let synth = SnapshotSynthesizer::new().expect("synth");
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
        let synth = SnapshotSynthesizer::new().expect("synth");
        let screen = synth.screen_state(&t, 1).expect("screen_state");
        assert!(screen.cells.is_none(), "cells = false leaves cells None");
    }

    #[test]
    fn screen_state_cells_collects_styles_sparsely() {
        // A bold-red "HI" followed by plain "ok": the styled cells must
        // surface in the cells projection, the plain cells must NOT (the
        // vec is sparse), and the styled cells must carry the right
        // attributes + RGB-resolved color (phux-8yl).
        let mut t = fresh(20, 2);
        // ESC[1;31m = bold + red fg; "HI"; ESC[0m reset; " ok".
        t.vt_write(b"\x1b[1;31mHI\x1b[0m ok");

        let synth = SnapshotSynthesizer::new().expect("synth");
        let screen = synth
            .screen_state_with_scrollback(&t, 1, None, true)
            .expect("screen_state_with_scrollback");
        let cells = screen.cells.expect("cells = true populates Some(..)");

        // Exactly the two styled cells (H, I) are emitted; the blank and
        // the plain "ok" cells are dropped by the sparse filter (no style,
        // no mark).
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
        // OSC-133 shell-integration marks classify cells as prompt / input
        // / output. Emit a prompt mark (`OSC 133 ; A`), prompt text, then a
        // command-start mark (`OSC 133 ; B`) and typed input. The cells
        // projection must surface the per-cell semantic content (phux-8yl).
        let mut t = fresh(40, 2);
        // OSC 133 ; A  -> prompt start. Then "$ " is prompt text.
        t.vt_write(b"\x1b]133;A\x07$ ");
        // OSC 133 ; B  -> command (input) start. Then "ls" is input.
        t.vt_write(b"\x1b]133;B\x07ls");

        let synth = SnapshotSynthesizer::new().expect("synth");
        let screen = synth
            .screen_state_with_scrollback(&t, 1, None, true)
            .expect("screen_state_with_scrollback");
        let cells = screen.cells.expect("cells = true populates Some(..)");

        // The prompt glyph "$" must carry Prompt; the input glyph "l"/"s"
        // must carry Input. We assert on the marks rather than exact cell
        // counts so the test is robust to how libghostty attributes the
        // trailing space.
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
        // A double-width CJK glyph occupies two grid columns; libghostty
        // emits its second column as a SpacerTail. The cells projection must
        // advance its column counter by the glyph's full display width so a
        // styled cell to its right reports the true grid column — the same
        // coordinate space cursor.x lives in. Regression: the phux-8yl walk
        // advanced col_index by 1 per cell, under-counting after wide glyphs.
        let mut t = fresh(20, 2);
        // Unstyled wide glyph (你, two columns) then a bold "X" at col 2.
        t.vt_write("你".as_bytes());
        t.vt_write(b"\x1b[1mX");

        let synth = SnapshotSynthesizer::new().expect("synth");
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
        // A wide glyph that does not fit in the final column of a row
        // soft-wraps to the next row; libghostty fills the vacated final
        // column with a `CellWide::SpacerHead` (grid width 1, empty
        // grapheme) and places the wide glyph at column 0 of the next row.
        //
        // The cells walk skips `SpacerTail` (a wide glyph's second column)
        // but treats `SpacerHead` as a normal width-1 cell, advancing
        // col_index by 1 — which matches libghostty's column model, where
        // `Cell.gridWidth()` returns 1 for `spacer_head` and 2 only for
        // `wide`. This test pins that accounting across the wrap boundary
        // (phux-ja1).
        //
        // Layout on a 4-column grid for bold "abc你d":
        //   row 0:  a(0) b(1) c(2) SpacerHead(3)
        //   row 1:  你(0,wide) SpacerTail(1) d(2)
        // Bold applies to every cell, so each surfaces in the projection.
        let mut t = fresh(4, 3);
        t.vt_write(b"\x1b[1m");
        t.vt_write("abc你d".as_bytes());

        let synth = SnapshotSynthesizer::new().expect("synth");
        let screen = synth
            .screen_state_with_scrollback(&t, 1, None, true)
            .expect("screen_state_with_scrollback");
        let cells = screen.cells.expect("cells = true populates Some(..)");

        // The (row, col) coordinates every bold cell reports. The SpacerHead
        // occupies the soft-wrap row's final column (row 0, col 3): it is a
        // real width-1 cell, not skipped like a SpacerTail, so it both
        // surfaces here and advances col_index by exactly 1.
        let coords: Vec<(u16, u16)> = cells.iter().map(|c| (c.row, c.col)).collect();
        assert_eq!(
            coords,
            vec![(0, 0), (0, 1), (0, 2), (0, 3), (1, 0), (1, 2)],
            "SpacerHead is a width-1 cell at the wrapped row's last column; \
             the wide glyph restarts column accounting at col 0 of the next \
             row and its SpacerTail (row 1, col 1) is skipped, got {cells:?}",
        );

        // The wrap resets column accounting per row: the wide glyph that the
        // SpacerHead displaced lands at col 0 of row 1, and the trailing "d"
        // reports col 2 (the wide glyph advanced two columns, its SpacerTail
        // contributing none). The SpacerHead did not leak a column into the
        // next row.
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
        // A 3-row viewport with 5 written lines pushes the oldest two into
        // scrollback. Requesting all history (Some(SCROLLBACK_ALL)) must
        // surface them above an unchanged viewport (phux-o1v).
        let mut t = fresh(20, 3);
        t.vt_write(b"line1\r\nline2\r\nline3\r\nline4\r\nline5");
        // Sanity: libghostty must actually be retaining the two scrolled rows.
        assert_eq!(t.scrollback_rows().expect("scrollback_rows"), 2);

        let synth = SnapshotSynthesizer::new().expect("synth");
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

        let synth = SnapshotSynthesizer::new().expect("synth");
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

        let synth = SnapshotSynthesizer::new().expect("synth");
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

        let synth = SnapshotSynthesizer::new().expect("synth");
        let screen = synth
            .screen_state_with_scrollback(&t, 0, Some(SCROLLBACK_ALL), false)
            .expect("screen_state_with_scrollback");
        assert!(screen.scrollback.is_empty());
        assert_eq!(screen.lines[0], "only one line");
    }

    #[test]
    fn synthesizer_round_trips_via_libghostty() {
        // Feed bytes into a Terminal, synthesise a snapshot, feed the
        // snapshot into a fresh Terminal — the cursor position must match.
        // We assert cursor position rather than full grid equality because
        // the byte synthesis is best-effort fidelity, not perfect diff;
        // the snapshot algorithm is allowed to use a different (but
        // equivalent) sequence of bytes.
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

    /// phux-uow0: the actor reuses ONE `SnapshotSynthesizer` across every
    /// attach (`self.synth`). A client attaching AFTER content was written
    /// must receive that content in its snapshot — even though a prior
    /// `synthesize()` already ran and consumed libghostty's shared per-row
    /// dirty bits. `synthesize()` is a FULL snapshot: it must repaint every
    /// row unconditionally, not emit a delta against the consumed dirty state.
    /// Pre-fix, the second snapshot came back blank (the dirty bits were gone),
    /// so a snapshot-reliant re-attach saw an empty screen and hung waiting for
    /// live output that never came (the `attach_detach_churn` / `both_axes` flakes).
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

    /// phux-uow0 REAL root cause: libghostty's per-row dirty bits live on the
    /// `Terminal` and are CONSUMED by `RenderState::update`. The actor has
    /// MULTIPLE `RenderState` consumers on one Terminal — the shared snapshot
    /// synthesizer (`self.synth`, reused across attaches) plus the per-consumer
    /// state-sync references primed in `register_consumer`. If a per-consumer
    /// `update` eats the dirty bits BEFORE the full snapshot runs, `synthesize()`
    /// must STILL emit the complete grid: it is a FULL snapshot, not a delta.
    /// This mirrors the actor sequence that blanked a re-attach's snapshot:
    /// client-1 attach (`synthesize`), marker arrives (`vt_write`), client-2
    /// attach (`register_consumer`'s update consumes dirty), client-2 snapshot.
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
        let other = SnapshotSynthesizer::new().expect("other");
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

    /// Regression test for phux-073: a wide CJK glyph (here `你`) takes
    /// two columns; libghostty marks the second column as
    /// `CellWide::SpacerTail`. Before the fix the synthesizer treated
    /// that tail as a blank cell and emitted a space, producing the
    /// wrong layout on replay. After the fix the tail is skipped and
    /// the grid round-trips exactly.
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

    /// Companion to `synthesizer_skips_wide_cell_tails`: exercise the
    /// wide-tail discriminator across a heterogeneous mix of CJK,
    /// emoji, and ASCII on multiple rows. Each emoji codepoint
    /// (e.g. `😀`, U+1F600) is double-width and produces a
    /// `CellWide::SpacerTail` neighbor in libghostty's grid, the same
    /// way CJK does. The round-trip must reproduce the source grid
    /// cell-for-cell.
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

        // The source row containing `東` must carry the literal glyph,
        // not a leading space that would indicate the wide tail leaked
        // into the leading position.
        assert!(
            src_grid[0].starts_with('東'),
            "source row 0 should start with 東, got {:?}",
            src_grid[0]
        );
    }

    /// phux-99n: a snapshot taken while a 1049-alt-screen program
    /// (vim/less/man/htop/tmux) is running MUST re-establish the alt
    /// screen via `?1049h` so the receiving mirror lands on the alt
    /// buffer — and must NOT force the primary screen via a stale `?47l`.
    ///
    /// This is the FLIP of the audit pin test
    /// `audit_snapshot_drops_alt_screen_1049_mode`: the pin asserted the
    /// buggy status quo (`?1049h` absent, `?47l` emitted); we assert the
    /// fix. 1049 also saves the cursor + clears on entry, so it must be
    /// emitted BEFORE the cursor re-establishment — verified by checking
    /// `?1049h` precedes the cursor-home/CUP in the byte stream.
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
        // 47 is off, so the snapshot reports its true (off) state. The bug
        // was emitting ?47l as the ONLY alt-screen signal; now ?1049h
        // carries the screen and ?47l is merely the honest 47 state.
        assert!(
            !bytes.contains("?47h"),
            "47 is off; snapshot must not assert ?47h; bytes={bytes:?}",
        );
        // Ordering: 1049 saves cursor + clears on entry, so it must come
        // before the epilogue's cursor re-establishment, else the CUP we
        // emit to restore the cursor is clobbered by the screen switch.
        // The epilogue's cursor-visibility CSI (`?25h`/`?25l`) is emitted
        // immediately after that CUP and only there, so it is a reliable
        // landmark for "the cursor has been re-established".
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

    /// The mouse-tracking DEC modes must survive the snapshot boundary.
    ///
    /// Regression for phux-mlo5 ("can't scroll in a TUI after attaching"): the
    /// epilogue replayed bracketed paste, focus events, and the alt-screen
    /// bits but never the mouse modes. The client keeps its own libghostty
    /// mirror and gates the wheel on it — a mirror seeded from a snapshot
    /// that omits `?1000h`/`?1002h`/`?1003h` believes no program wants the
    /// mouse, so it swallowed the wheel into wheel-to-arrow translation
    /// instead of forwarding `INPUT_MOUSE`. Asserted through a REPLAY into
    /// a fresh terminal, because it is the mirror's resulting mode state —
    /// not the byte spelling — that the client actually reads.
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

    /// The inverse: a pane whose program never asked for the mouse must
    /// leave the mirror with tracking OFF, so the wheel keeps driving the
    /// local scrollback viewport. An unconditional `?1000h` in the epilogue
    /// would break scrollback in every plain shell pane.
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

    /// DEC 1007 (xterm alternate scroll) is the bit that decides whether an
    /// alt-screen pane WITHOUT mouse tracking gets wheel-to-arrow-key
    /// translation. libghostty defaults it on, so the load-bearing case is
    /// an app that opted OUT: `?1007l` must survive the snapshot, or a
    /// reattach silently resurrects arrow keys the app asked not to get.
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

    /// phux-99n: the legacy 47 alt-screen mode still round-trips. A
    /// program that uses bare `?47h` (rare, but valid) must have the
    /// snapshot re-emit `?47h`, not silently drop it.
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

    /// phux-99n: on the PRIMARY screen the snapshot must report all three
    /// alt-screen modes as off (`?47l ?1047l ?1049l`) — i.e. it must not
    /// accidentally assert any alt-screen mode.
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

    /// phux-99n: the per-consumer reference diff trips on a 47<->1049
    /// transition. The audit noted that tracking only the legacy-47 bit in
    /// `ReferenceCursorMode` would miss a transition between the two
    /// distinct alt-screen modes. After priming on 1049, switching to the
    /// primary screen must produce a non-empty diff that re-emits
    /// `?1049l`.
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

    /// A program can enable mouse tracking without touching a single row
    /// or moving the cursor (`:set mouse=a` in vim, a TUI arming the mouse
    /// for a modal). `ReferenceCursorMode` therefore tracks the mouse mode
    /// bits: without them the diff is empty, the consumer's mirror keeps
    /// the stale "nobody wants the mouse" state, and the wheel stays
    /// mis-routed until something else happens to dirty a row.
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

    /// phux-4l0 (correctness half): an unchanged terminal diffs to an
    /// empty body across repeated calls (emit-once / steady state). The
    /// behavioral idle short-circuit lives in the actor's `tick_emit`
    /// (see `terminal_actor.rs`); this pins the synthesis-level invariant
    /// the short-circuit relies on — a clean terminal yields nothing.
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
}
