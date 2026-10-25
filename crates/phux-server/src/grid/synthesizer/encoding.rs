use super::{
    BoundedSnapshotBytes, CellIteration, CellWide, CursorVisualStyle, GRAPHEME_INLINE,
    GhosttyTerminal, MOUSE_MODES, Mode, Pen, Snapshot, SynthesisError, write_reset_and_sgr,
};
use std::io::Write as _;

/// Emit one cell: SGR delta when the pen changes, then its grapheme (or a
/// space); wide-cell tails emit nothing.
pub(super) fn emit_cell_bounded(
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

pub(super) fn encode_graphemes_bounded(
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
pub(super) fn emit_cell(
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

/// Epilogue shared by snapshots and diffs: SGR reset, cursor position,
/// visibility, style, and the load-bearing mode bits.
///
/// Known, self-healing gaps kept on purpose: the client pen may differ from
/// the server's right after the snapshot, and an off-viewport cursor homes
/// to `ESC[H`. Both reconverge on the next live frame.
pub(super) fn emit_epilogue(
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

/// Emit the alt-screen modes (47 / 1047 / 1049), each queried separately.
///
/// Must precede the row paint and cursor restore: `?1049h` clears the alt
/// buffer and saves the cursor on entry.
pub(super) fn emit_screen_mode(
    out: &mut Vec<u8>,
    terminal: &GhosttyTerminal<'_, '_>,
) -> Result<(), SynthesisError> {
    emit_mode(out, terminal, Mode::ALT_SCREEN_LEGACY, b"47")?;
    emit_mode(out, terminal, Mode::ALT_SCREEN, b"1047")?;
    emit_mode(out, terminal, Mode::ALT_SCREEN_SAVE, b"1049")?;
    Ok(())
}

/// 1-based CUP (`CSI <r+1>;<c+1> H`). Inputs are zero-based.
pub(super) fn write_cup(out: &mut Vec<u8>, row: u16, col: u16) {
    let r = row.saturating_add(1);
    let c = col.saturating_add(1);
    let _ = write!(out, "\x1b[{r};{c}H");
}

/// Encoded length of [`write_cup`]'s `ESC [ r ; c H`.
pub(super) fn cup_len(row: u16, col: u16) -> usize {
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
pub(super) fn assemble_diff(
    toggle: &[u8],
    changed: &[u16],
    bodies: &[Vec<u8>],
    epilogue: &[u8],
) -> Vec<u8> {
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
