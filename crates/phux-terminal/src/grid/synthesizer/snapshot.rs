use super::{
    BoundedSnapshotBytes, CellIterator, CellWide, GRAPHEME_INLINE, GhosttyTerminal, GridRef, Pen,
    Point, PointCoordinate, RowIterator, Snapshot, SnapshotSynthesizer, Style, SynthesisError,
    emit_cell_bounded, emit_screen_mode, encode_graphemes_bounded, history_window_start,
    kitty_replay, walk_viewport_rows, write_cup, write_reset_and_sgr_unresolved,
};
use std::io::Write as _;

impl<'alloc> SnapshotSynthesizer<'alloc> {
    /// History rows `[start, total)` as styled VT, rows joined by CRLF, then
    /// an SGR reset and an `SU` that scrolls them into the client's
    /// scrollback. Reads via `Point::History`, which never mutates the
    /// terminal.
    pub(super) fn scrollback_styled_bytes_bounded(
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
}

/// Capacity hint for a full bounded paint: two bytes per cell, falling back to
/// the ceiling itself when that product overflows.
pub(super) fn full_paint_hint(cols: u16, rows_n: u16, max_bytes: usize) -> usize {
    usize::from(cols)
        .checked_mul(usize::from(rows_n))
        .and_then(|cells| cells.checked_mul(2))
        .unwrap_or(max_bytes)
}

/// Reset (`DECSTR + ED 2 + CUP home`) then select the screen buffer, which
/// must precede any cell bytes (see [`emit_screen_mode`]).
pub(super) fn write_full_paint_prologue(
    out: &mut BoundedSnapshotBytes,
    terminal: &GhosttyTerminal<'_, '_>,
) -> Result<(), SynthesisError> {
    out.write_all(b"\x1b[!p\x1b[2J\x1b[H")
        .map_err(|_| SynthesisError::LimitExceeded)?;
    emit_screen_mode(&mut out.bytes, terminal)?;
    out.check()
}

/// `CUP` to each viewport row and emit its cells within the byte ceiling.
pub(super) fn paint_all_rows_bounded<'alloc>(
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

/// Replay supported Kitty state without charging unsupported payloads.
pub(super) fn replay_kitty_graphics_bounded(
    terminal: &GhosttyTerminal<'_, '_>,
    out: &mut BoundedSnapshotBytes,
    image_protocols: phux_protocol::caps::ImageProtocolSet,
) -> Result<(), SynthesisError> {
    if !image_protocols.contains(phux_protocol::caps::ImageProtocol::KittyGraphics) {
        return Ok(());
    }
    let mut kitty_placements = libghostty_vt::kitty::graphics::PlacementIterator::new()?;
    // Full snapshots replace images as well as cells. ED 2 does not delete
    // Kitty placements, and d=A only deletes visible non-virtual placements.
    // The full ID range also removes stale Unicode-placeholder placements.
    out.write_all(b"\x1b_Ga=d,d=R,x=1,y=4294967295,q=2;\x1b\\")
        .map_err(|_| SynthesisError::LimitExceeded)?;
    let _ = kitty_replay::emit_kitty_graphics_snapshot(terminal, &mut kitty_placements, out)
        .map_err(|error| match error {
            kitty_replay::KittyReplayError::Io(_) => SynthesisError::LimitExceeded,
            error @ kitty_replay::KittyReplayError::Ghostty(_) => {
                SynthesisError::KittyReplay(error)
            }
        })?;
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
