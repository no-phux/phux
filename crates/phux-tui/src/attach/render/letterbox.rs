//! Letterboxing (ADR-0027): centre an undersized mirror in its render rect
//! and blank the margin bars around it.

use std::io::{self, Write};

use super::write_cup;

/// The centred placement of a mirror within a render rect (ADR-0027): the
/// content's `inner_origin`/`inner_clip` for the core paint, and the four
/// margin bars [`emit_letterbox_margins`] blanks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Letterbox {
    /// Centred top-left of the mirror content (rect origin + pad).
    pub(super) inner_origin: (u16, u16),
    /// Painted extent `min(mirror, rect)` on each axis — the clamp the
    /// existing `render_at` already applies, so a mirror >= the rect is
    /// confined to the rect with no pad.
    pub(super) inner_clip: (u16, u16),
    /// Left pad width in columns (`= inner_origin.0 - rect_origin.0`).
    pub(super) margin_left: u16,
    /// Right pad width in columns (the floor split's extra cell lands here).
    pub(super) margin_right: u16,
    /// Top pad height in rows (`= inner_origin.1 - rect_origin.1`).
    pub(super) margin_top: u16,
    /// Bottom pad height in rows (the floor split's extra cell lands here).
    pub(super) margin_bottom: u16,
    /// The rect's outer origin `(x, y)`, retained so the margin emitter can
    /// position the bars without re-deriving it from the pads.
    rect_origin: (u16, u16),
    /// The rect's full extent `(cols, rows)`, retained for the same reason.
    rect_clip: (u16, u16),
}

impl Letterbox {
    /// Whether any margin bar exists (`false` in the clamp case, which paints
    /// byte-identically to [`super::TerminalRenderer::render_at`]).
    pub(super) const fn has_pad(self) -> bool {
        self.margin_left > 0
            || self.margin_right > 0
            || self.margin_top > 0
            || self.margin_bottom > 0
    }
}

/// Centre `mirror` within the rect at `rect_origin` spanning `rect_clip`.
/// Per axis a smaller mirror's gap is floor-split (extra cell bottom/right);
/// a larger or equal one gets no pad and a clip clamped to the rect.
pub(super) fn letterbox_rect(
    rect_origin: (u16, u16),
    rect_clip: (u16, u16),
    mirror: (u16, u16),
) -> Letterbox {
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
pub(super) fn emit_letterbox_margins(out: &mut impl Write, lb: Letterbox) -> io::Result<()> {
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
