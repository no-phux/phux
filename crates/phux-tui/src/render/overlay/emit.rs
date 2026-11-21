//! Emitting an overlay buffer as VT bytes: the whole viewport, or one
//! clipped box with its drop shadow.

use std::io::{self, Write};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

/// Emit a buffer as VT bytes (width-aware rows, coalesced style runs).
pub(super) fn emit_buffer(out: &mut impl Write, buf: &Buffer) -> io::Result<()> {
    let area = buf.area;
    // Hide cursor for the duration of the modal paint.
    out.write_all(b"\x1b[?25l")?;
    for row in 0..area.height {
        emit_row_span(out, buf, area.y + row, area.x, area.right())?;
    }
    // Park the (hidden) cursor; the pane repaint on dismiss re-shows it.
    out.write_all(b"\x1b[1;1H")?;
    out.flush()
}

/// Emit only the cells of `buf` inside `clip`, plus the drop shadow, each
/// row `CUP`-positioned at its own left edge. The shadow's two outer corners are
/// skipped so it reads as an L.
pub(super) fn emit_buffer_clipped(
    out: &mut impl Write,
    buf: &mut Buffer,
    clip: Rect,
    shadow: Color,
) -> io::Result<()> {
    if clip.width == 0 || clip.height == 0 {
        return Ok(());
    }
    let bands = paint_shadow_bands(buf, clip, shadow);
    out.write_all(b"\x1b[?25l")?;
    emit_clipped_rows(out, buf, clip, bands)?;
    // Park the (hidden) cursor at the modal origin; the next pane repaint on
    // dismiss emits its own DECTCEM.
    write!(out, "\x1b[{};{}H", clip.y + 1, clip.x + 1)?;
    out.flush()
}

/// Which of the drop shadow's two bands the viewport has room for.
#[derive(Clone, Copy)]
struct ShadowBands {
    /// The column just right of the box (`clip.x + clip.width`).
    col: bool,
    /// The row just below the box (`clip.y + clip.height`).
    row: bool,
}

impl ShadowBands {
    /// The shadow bands exist only where there's a pane cell to cast onto.
    const fn for_clip(area: Rect, clip: Rect, shadow: Color) -> Self {
        Self {
            col: !matches!(shadow, Color::Reset) && clip.x + clip.width < area.width,
            row: !matches!(shadow, Color::Reset) && clip.y + clip.height < area.height,
        }
    }
}

/// Paint the drop shadow's bands into `buf` as `shadow`-bg spaces, and report
/// which of them landed.
fn paint_shadow_bands(buf: &mut Buffer, clip: Rect, shadow: Color) -> ShadowBands {
    let vp_w = buf.area.width;
    let vp_h = buf.area.height;
    let bands = ShadowBands::for_clip(buf.area, clip, shadow);
    let rx = clip.x + clip.width; // box right edge (exclusive) = shadow column
    let ry = clip.y + clip.height; // box bottom edge (exclusive) = shadow row
    let style = ratatui::style::Style::default().bg(shadow);
    if bands.col {
        // Right band: beside the box's lower rows + the bottom-right corner.
        for y in (clip.y + 1)..=ry.min(vp_h - 1) {
            fill_shadow_cell(buf, rx, y, style);
        }
    }
    if bands.row {
        // Bottom band: beneath the box, starting one cell in (skip the corner).
        for x in (clip.x + 1)..=rx.min(vp_w - 1) {
            fill_shadow_cell(buf, x, ry, style);
        }
    }
    bands
}

/// Overwrite one in-bounds cell with a blank carrying the shadow style.
fn fill_shadow_cell(buf: &mut Buffer, x: u16, y: u16, style: ratatui::style::Style) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_symbol(" ");
        cell.set_style(style);
    }
}

/// Emit the box's rows plus the bottom shadow band, each with a leading CUP
/// to its own left edge so only the box (and shadow band) cells are written.
fn emit_clipped_rows(
    out: &mut impl Write,
    buf: &Buffer,
    clip: Rect,
    bands: ShadowBands,
) -> io::Result<()> {
    let bx = clip.x;
    let by = clip.y;
    let rx = clip.x + clip.width;
    let ry = clip.y + clip.height;
    // Box rows. The top row omits the right shadow cell (no shadow above the
    // box); lower rows extend one cell right to include the shadow column.
    for row in by..ry {
        let end = if bands.col && row > by { rx + 1 } else { rx };
        emit_row_span(out, buf, row, bx, end)?;
    }
    // Bottom shadow row, skipping the bottom-left corner (start at bx + 1).
    if bands.row {
        let end = if bands.col { rx + 1 } else { rx };
        emit_row_span(out, buf, ry, bx + 1, end)?;
    }
    Ok(())
}

/// Emit cells `[start_col, end_col)` of `row`, `CUP`-positioned at the start, with
/// per-cell SGR deltas.
pub(super) fn emit_row_span(
    out: &mut impl Write,
    buf: &Buffer,
    row: u16,
    start_col: u16,
    end_col: u16,
) -> io::Result<()> {
    write!(out, "\x1b[{};{}H", row + 1, start_col + 1)?;
    out.write_all(b"\x1b[0m")?;
    let mut prev_styled = None;
    let mut col = start_col;
    while col < end_col {
        let cell = &buf[(col, row)];
        crate::render::sgr::emit_cell_sgr(out, cell, &mut prev_styled)?;
        let sym = cell.symbol();
        let width = u16::try_from(crate::render::display_width(sym))
            .unwrap_or(1)
            .max(1);
        if sym.is_empty() || width > end_col - col {
            out.write_all(b" ")?;
            col += 1;
        } else {
            out.write_all(sym.as_bytes())?;
            col += width;
        }
    }
    if prev_styled.is_some() {
        out.write_all(b"\x1b[0m")?;
    }
    Ok(())
}
