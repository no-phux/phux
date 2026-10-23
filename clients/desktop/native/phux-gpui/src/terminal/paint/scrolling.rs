//! Recognize whole-row movement and translate retained primitives together.

use super::*;

/// Whole rows the new frame shares with the previous one, moved up or down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scroll {
    /// Content moved toward row 0. The new rows are at the bottom.
    Up(u16),
    /// Content moved toward the last row. The new rows are at the top.
    Down(u16),
}

impl Scroll {
    fn rows(self) -> u16 {
        match self {
            Self::Up(rows) | Self::Down(rows) => rows,
        }
    }
}

pub(super) fn scroll_of(previous: &GridFrame, next: &GridFrame) -> Option<Scroll> {
    if previous.cols != next.cols || previous.rows != next.rows || next.rows < 2 {
        return None;
    }
    // History navigation gives a cheap candidate; each retained row is
    // still checked before reuse. Live output at the history limit falls
    // back to the content search below.
    let delta = i128::from(next.scrollbar.offset) - i128::from(previous.scrollbar.offset);
    if delta != 0 && delta.unsigned_abs() < u128::from(next.rows) {
        let rows = delta.unsigned_abs() as u16;
        return Some(if delta > 0 {
            Scroll::Up(rows)
        } else {
            Scroll::Down(rows)
        });
    }
    let prev = row_fingerprints(previous);
    let next_rows = row_fingerprints(next);
    if let Some(shift) = matching_shift(&prev, &next_rows) {
        let scroll = Scroll::Up(shift);
        if scroll_holds(previous, next, scroll) {
            return Some(scroll);
        }
    }
    if let Some(shift) = matching_shift(&next_rows, &prev) {
        let scroll = Scroll::Down(shift);
        if scroll_holds(previous, next, scroll) {
            return Some(scroll);
        }
    }
    None
}

fn matching_shift(source: &[u64], moved: &[u64]) -> Option<u16> {
    (1..source.len())
        .find(|shift| rows_aligned(source, moved, *shift))
        .and_then(|shift| u16::try_from(shift).ok())
}

fn rows_aligned(source: &[u64], moved: &[u64], shift: usize) -> bool {
    let Some(source) = source.get(shift..) else {
        return false;
    };
    let kept = moved.len().saturating_sub(shift);
    moved[..kept]
        .iter()
        .zip(source)
        .all(|(left, right)| left == right)
}

fn scroll_holds(previous: &GridFrame, next: &GridFrame, scroll: Scroll) -> bool {
    let shift = usize::from(scroll.rows());
    let kept = usize::from(next.rows).saturating_sub(shift);
    (0..kept).all(|row| {
        let (prev_row, next_row) = scrolled_pair(scroll, row);
        rows_paint_alike(previous, next, prev_row, next_row)
    })
}

fn scrolled_pair(scroll: Scroll, row: usize) -> (u16, u16) {
    let shift = usize::from(scroll.rows());
    let (prev, next) = match scroll {
        Scroll::Up(_) => (row + shift, row),
        Scroll::Down(_) => (row, row + shift),
    };
    (
        u16::try_from(prev).unwrap_or(u16::MAX),
        u16::try_from(next).unwrap_or(u16::MAX),
    )
}

/// Rows of the new scene, among the `height` a shift moves, that cannot be
/// moved from the previous scene and must be shaped.
pub(super) fn rows_after_scroll(
    previous: &GridFrame,
    next: &GridFrame,
    settings: &Settings,
    (scroll, height): (Scroll, u16),
    geometry: &Geometry,
) -> Vec<u16> {
    let reverse = match scroll {
        Scroll::Up(rows) => Scroll::Down(rows),
        Scroll::Down(rows) => Scroll::Up(rows),
    };
    let old_cursor = solid_head(previous, settings);
    let new_cursor = solid_head(next, settings);
    (0..height)
        .filter(|row| {
            let Some(source) = destination_row(reverse, height, *row) else {
                return true;
            };
            !fully_built(previous, source, geometry)
                || old_cursor
                    .filter(|(at, _)| *at == source)
                    .map(|(_, col)| col)
                    != new_cursor.filter(|(at, _)| *at == *row).map(|(_, col)| col)
                || !rows_paint_alike(previous, next, source, *row)
        })
        .collect()
}

/// Whether the previous scene holds every primitive of `row`. The builder
/// skips viewport cells outside the pane, so a row cut by the pane's edge
/// is shaped again; the fringe row is always built whole.
fn fully_built(previous: &GridFrame, row: u16, geometry: &Geometry) -> bool {
    if row == previous.rows {
        return true;
    }
    let bounds = geometry.cell_bounds(row, 0, 1);
    bounds.top() >= geometry.bounds.top() && bounds.bottom() <= geometry.bounds.bottom()
}

fn destination_row(scroll: Scroll, height: u16, row: u16) -> Option<u16> {
    match scroll {
        Scroll::Up(by) if row >= by => Some(row - by),
        Scroll::Down(by) if row.checked_add(by).is_some_and(|next| next < height) => Some(row + by),
        _ => None,
    }
}

pub(super) fn shift_scene(scene: &mut Scene, scroll: Scroll, height: u16) {
    let geometry = &scene.geometry;
    shift_quads(&mut scene.backgrounds, scroll, height, geometry);
    shift_quads(&mut scene.decorations, scroll, height, geometry);
    shift_inks(&mut scene.inks, scroll, height, geometry);
    shift_sprites(&mut scene.sprites, scroll, height, geometry);
    shift_glyphs(&mut scene.glyphs, scroll, height, geometry);
}

fn shift_quads(quads: &mut Vec<RowQuad>, scroll: Scroll, height: u16, geometry: &Geometry) {
    quads.retain_mut(|quad| {
        shift_row(&mut quad.row, &mut quad.bounds, scroll, height, geometry).is_some()
    });
}

fn shift_inks(inks: &mut Vec<RowInk>, scroll: Scroll, height: u16, geometry: &Geometry) {
    inks.retain_mut(|ink| {
        shift_row(
            &mut ink.row,
            ink_bounds(&mut ink.ink),
            scroll,
            height,
            geometry,
        )
        .is_some()
    });
}

fn shift_sprites(rows: &mut Vec<RowSprites>, scroll: Scroll, height: u16, geometry: &Geometry) {
    rows.retain_mut(|sprites| {
        let Some(next) = destination_row(scroll, height, sprites.row) else {
            return false;
        };
        sprites.dy +=
            geometry.cell_bounds(next, 0, 1).top() - geometry.cell_bounds(sprites.row, 0, 1).top();
        sprites.row = next;
        true
    });
}

fn shift_glyphs(
    glyphs: &mut Vec<GlyphObservation>,
    scroll: Scroll,
    height: u16,
    geometry: &Geometry,
) {
    glyphs.retain_mut(|glyph| {
        let Some(dy) = shift_row(&mut glyph.row, &mut glyph.bounds, scroll, height, geometry)
        else {
            return false;
        };
        glyph.origin.y += dy;
        glyph.baseline += dy;
        true
    });
}

fn shift_row(
    row: &mut u16,
    bounds: &mut Bounds<Pixels>,
    scroll: Scroll,
    height: u16,
    geometry: &Geometry,
) -> Option<Pixels> {
    let next = destination_row(scroll, height, *row)?;
    let dy = geometry.cell_bounds(next, 0, 1).top() - geometry.cell_bounds(*row, 0, 1).top();
    *row = next;
    bounds.origin.y += dy;
    Some(dy)
}

fn ink_bounds(ink: &mut Ink) -> &mut Bounds<Pixels> {
    match ink {
        Ink::Glyph(glyph) => &mut glyph.bounds,
        Ink::Run(run) => &mut run.bounds,
    }
}

fn row_fingerprints(frame: &GridFrame) -> Vec<u64> {
    (0..frame.rows)
        .map(|row| row_fingerprint(frame, row))
        .collect()
}

fn row_fingerprint(frame: &GridFrame, row: u16) -> u64 {
    let cols = usize::from(frame.cols);
    let Some(start) = usize::from(row).checked_mul(cols) else {
        return 0;
    };
    let mut hash = 0xcbf29ce484222325_u64;
    for index in start..start + cols {
        hash = mix_cell(hash, frame, index);
    }
    hash
}

fn mix_cell(mut hash: u64, frame: &GridFrame, index: usize) -> u64 {
    let Some(cell) = frame.buffer.cells.get(index) else {
        return mix(hash, 0xffff);
    };
    hash = mix(hash, u64::from(cell.utf8_len));
    hash = mix(hash, u64::from(cell.wide));
    hash = mix(hash, u64::from(cell.flags));
    hash = mix(hash, u64::from(cell.underline));
    for byte in color_bytes(cell) {
        hash = mix(hash, u64::from(byte));
    }
    hash = mix_metadata(hash, frame.buffer.metadata.get(index).copied());
    match text_span(cell, &frame.buffer.utf8) {
        Some(text) => {
            for byte in text {
                hash = mix(hash, u64::from(*byte));
            }
            hash
        }
        None => mix(hash, 0xff),
    }
}

fn mix_metadata(mut hash: u64, metadata: Option<CellMetadata>) -> u64 {
    let Some(metadata) = metadata else {
        return mix(hash, 0);
    };
    hash = mix(hash, u64::from(metadata.foreground_kind));
    hash = mix(hash, u64::from(metadata.foreground_palette_index));
    hash = mix(hash, u64::from(metadata.underline_color_is_default));
    mix(hash, u64::from(metadata.background_color_is_default))
}

fn mix(hash: u64, value: u64) -> u64 {
    hash.wrapping_mul(0x517cc1b727220a95).wrapping_add(value)
}
