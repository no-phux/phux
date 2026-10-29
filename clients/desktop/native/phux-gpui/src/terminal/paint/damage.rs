//! Compare painted rows and rebuild only the damaged portions of a scene.

use super::*;

/// Reshape `frame` from `previous` when the geometry and theme still match.
/// `None` means the caller should build the scene from scratch. `previous`
/// is consumed either way.
pub(super) fn patch(
    mut previous: Scene,
    frame: Arc<GridFrame>,
    settings: Settings,
    bounds: Bounds<Pixels>,
    scale: (f32, Option<crate::platform::Raster>),
    window: &gpui::Window,
) -> Option<Scene> {
    if !can_patch(&previous, &frame, bounds, scale, &settings) {
        return None;
    }
    let rebuilt = rows_to_rebuild(&previous.frame, &frame, &settings);
    // Hashing both grids costs a full read. Do it only when the in-place
    // diff would reshape most of the screen, which is what a line of output
    // does. A few dirty rows are cheaper to repaint than to hash.
    if (previous.frame.scrollbar.offset != frame.scrollbar.offset
        || worth_scroll_search(rebuilt.len(), frame.rows))
        && let Some(scroll) = scroll_of(&previous.frame, &frame)
    {
        // Two frames that both carry a fringe scroll it with the viewport: a
        // row leaving the bottom becomes the fringe, and the old fringe can
        // enter, so only the rows new to the scene are shaped.
        let height = shifted_height(&previous.frame, &frame);
        let mut scrolled = rows_after_scroll(
            &previous.frame,
            &frame,
            &settings,
            (scroll, height),
            &previous.geometry,
        );
        if height == frame.rows {
            scrolled = with_fringe_row(scrolled, &frame);
        }
        if scrolled.len() < rebuilt.len() + usize::from(has_fringe(&frame)) {
            if height == frame.rows {
                drop_fringe(&mut previous);
            }
            shift_scene(&mut previous, scroll, height);
            return finish_patch(previous, frame, settings, &scrolled, window);
        }
    }
    // Even a full redraw keeps its font tables. Rebuilding Fonts here would
    // reshape the printable-ASCII alphabet on every animation frame.
    drop_fringe(&mut previous);
    let rebuilt = with_fringe_row(rebuilt, &frame);
    finish_patch(previous, frame, settings, &rebuilt, window)
}

/// Rows a scroll shift moves: the viewport, plus the fringe row when both
/// frames carry one.
pub(super) fn shifted_height(previous: &GridFrame, next: &GridFrame) -> u16 {
    if has_fringe(previous) && has_fringe(next) {
        next.rows + 1
    } else {
        next.rows
    }
}

/// Whether `frame` carries the row below its viewport, which the painter
/// builds as row `frame.rows`.
pub(super) fn has_fringe(frame: &GridFrame) -> bool {
    frame.buffer.fringe.len() == usize::from(frame.cols)
}

/// A viewport cell, or the fringe cell in row `frame.rows`.
pub(super) fn painted_cell(frame: &GridFrame, row: u16, col: u16) -> Option<(&Cell, CellMetadata)> {
    if row == frame.rows && col < frame.cols {
        let col = usize::from(col);
        let cell = frame.buffer.fringe.get(col)?;
        let metadata = frame.buffer.fringe_metadata.get(col).copied();
        return Some((cell, metadata.unwrap_or_default()));
    }
    let cell = frame.cell(row, col)?;
    let index = usize::from(row) * usize::from(frame.cols) + usize::from(col);
    let metadata = frame.buffer.metadata.get(index).copied();
    Some((cell, metadata.unwrap_or_default()))
}

pub(super) fn painted_text(frame: &GridFrame, row: u16, col: u16) -> &[u8] {
    if row == frame.rows {
        frame.buffer.fringe_text(usize::from(col))
    } else {
        frame.cell_text(row, col)
    }
}

/// `rows` plus the fringe row when `frame` has one. Rows stay sorted.
fn with_fringe_row(mut rows: Vec<u16>, frame: &GridFrame) -> Vec<u16> {
    if has_fringe(frame) {
        rows.push(frame.rows);
    }
    rows
}

/// Remove the previous fringe row; a patch rebuilds it from the new frame.
fn drop_fringe(scene: &mut Scene) {
    let rows = scene.frame.rows;
    scene.backgrounds.retain(|quad| quad.row < rows);
    scene.inks.retain(|ink| ink.row < rows);
    scene.sprites.retain(|sprites| sprites.row < rows);
    scene.decorations.retain(|quad| quad.row < rows);
    scene.glyphs.retain(|glyph| glyph.row < rows);
}

/// True when so many rows changed that a scroll shift can beat reshaping them.
pub(super) fn worth_scroll_search(rebuilt: usize, rows: u16) -> bool {
    let height = usize::from(rows);
    let slack = height / 8;
    height >= 2 && rebuilt >= height.saturating_sub(slack).max(2)
}

fn finish_patch(
    mut previous: Scene,
    frame: Arc<GridFrame>,
    settings: Settings,
    rebuilt: &[u16],
    window: &gpui::Window,
) -> Option<Scene> {
    if rebuilt.is_empty() {
        let cursor_changed =
            cursor_key(&previous.frame, &settings) != cursor_key(&frame, &settings);
        previous.frame = frame;
        previous.settings = settings;
        if cursor_changed {
            prepare_cursor(&mut previous);
        }
        return Some(previous);
    }
    let geometry = previous.geometry;
    let kept_bounds = previous.bounds;
    let kept_scale = previous.scale;
    let fonts = Rc::clone(&previous.fonts);
    let mut builder = Builder::from_kept(
        frame,
        settings,
        geometry,
        (kept_bounds, kept_scale, previous.raster),
        Some(fonts),
    );
    builder.paint_rows(rebuilt, window);
    let fresh = builder.finish();
    Some(assemble(previous, fresh, rebuilt))
}

pub(super) fn can_patch(
    scene: &Scene,
    frame: &GridFrame,
    bounds: Bounds<Pixels>,
    (scale, raster): (f32, Option<crate::platform::Raster>),
    settings: &Settings,
) -> bool {
    scene.bounds == bounds
        && scene.scale == scale
        && scene.raster == raster
        && scene.frame.cols == frame.cols
        && scene.frame.rows == frame.rows
        && scene.frame.colors == frame.colors
        && scene.frame.terminal_id == frame.terminal_id
        && scene.frame.stream_id == frame.stream_id
        && scene.frame.bootstrap_id == frame.bootstrap_id
        && scene.frame.generation <= frame.generation
        && scene.settings.paints_like(settings)
}

pub(super) fn rows_to_rebuild(
    previous: &GridFrame,
    next: &GridFrame,
    settings: &Settings,
) -> Vec<u16> {
    let old = solid_head(previous, settings);
    let new = solid_head(next, settings);
    // Row damage describes one publication only. A painter can skip generations,
    // so compare every row unless this is the adjacent frame of the same replica.
    let exact_rows = next.damage == GridDamage::Rows
        && previous.generation.checked_add(1) == Some(next.generation)
        && previous.stream_id == next.stream_id
        && previous.bootstrap_id == next.bootstrap_id
        && next.buffer.row_dirty.len() == usize::from(next.rows);
    (0..next.rows)
        .filter(|row| {
            solid_row_changed(old, new, *row)
                || (!exact_rows || next.is_row_dirty(*row))
                    && !row_paints_alike(previous, next, *row)
        })
        .collect()
}

fn solid_row_changed(old: Option<(u16, u16)>, new: Option<(u16, u16)>, row: u16) -> bool {
    if old == new {
        return false;
    }
    old.is_some_and(|(at, _)| at == row) || new.is_some_and(|(at, _)| at == row)
}

pub(super) fn solid_head(frame: &GridFrame, settings: &Settings) -> Option<(u16, u16)> {
    solid_cursor(frame, settings).then(|| cursor_head(frame))
}

/// Cursor placement that changes quads without changing cell glyphs: a bar,
/// underline, or hollow cursor, or a width change on an unmoved solid head.
pub(super) fn cursor_key(
    frame: &GridFrame,
    settings: &Settings,
) -> Option<(u16, u16, CursorStyle, bool)> {
    if !cursor_visible(frame, settings) {
        return None;
    }
    let (row, col) = cursor_head(frame);
    let style = if settings.focused {
        frame.cursor.style
    } else {
        CursorStyle::BlockHollow
    };
    Some((row, col, style, frame.cursor.width.is_wide()))
}

struct RowParts<'a> {
    prev_cells: &'a [Cell],
    next_cells: &'a [Cell],
    prev_meta: &'a [CellMetadata],
    next_meta: &'a [CellMetadata],
}

pub(super) fn row_paints_alike(previous: &GridFrame, next: &GridFrame, row: u16) -> bool {
    rows_paint_alike(previous, next, row, row)
}

pub(super) fn rows_paint_alike(
    previous: &GridFrame,
    next: &GridFrame,
    prev_row: u16,
    next_row: u16,
) -> bool {
    let Some(parts) = row_slices(previous, next, prev_row, next_row) else {
        return false;
    };
    parts
        .prev_cells
        .iter()
        .zip(parts.next_cells)
        .all(|(left, right)| {
            cell_paints_alike(left, &previous.buffer.utf8, right, &next.buffer.utf8)
        })
        && parts
            .prev_meta
            .iter()
            .zip(parts.next_meta)
            .all(|(left, right)| metadata_matches(left, right))
}

fn row_slices<'a>(
    previous: &'a GridFrame,
    next: &'a GridFrame,
    prev_row: u16,
    next_row: u16,
) -> Option<RowParts<'a>> {
    if previous.cols != next.cols {
        return None;
    }
    let (prev_cells, prev_meta) = painted_row(previous, prev_row)?;
    let (next_cells, next_meta) = painted_row(next, next_row)?;
    Some(RowParts {
        prev_cells,
        next_cells,
        prev_meta,
        next_meta,
    })
}

/// A viewport row's cells and metadata, or the fringe's for row `frame.rows`.
fn painted_row(frame: &GridFrame, row: u16) -> Option<(&[Cell], &[CellMetadata])> {
    if row == frame.rows {
        return has_fringe(frame).then_some((
            frame.buffer.fringe.as_slice(),
            frame.buffer.fringe_metadata.as_slice(),
        ));
    }
    let span = row_range(row, usize::from(frame.cols))?;
    Some((
        frame.buffer.cells.get(span.clone())?,
        frame.buffer.metadata.get(span)?,
    ))
}

fn row_range(row: u16, cols: usize) -> Option<std::ops::Range<usize>> {
    let start = usize::from(row).checked_mul(cols)?;
    let end = start.checked_add(cols)?;
    Some(start..end)
}

fn cell_paints_alike(left: &Cell, left_utf8: &[u8], right: &Cell, right_utf8: &[u8]) -> bool {
    paint_fields_match(left, right) && same_text(left, left_utf8, right, right_utf8)
}

fn paint_fields_match(left: &Cell, right: &Cell) -> bool {
    left.utf8_len == right.utf8_len
        && left.wide == right.wide
        && left.flags == right.flags
        && left.underline == right.underline
        && colors_match(left, right)
}

fn colors_match(left: &Cell, right: &Cell) -> bool {
    color_bytes(left) == color_bytes(right)
}

pub(super) fn color_bytes(cell: &Cell) -> [u8; 9] {
    [
        cell.foreground_r,
        cell.foreground_g,
        cell.foreground_b,
        cell.background_r,
        cell.background_g,
        cell.background_b,
        cell.underline_r,
        cell.underline_g,
        cell.underline_b,
    ]
}

fn metadata_matches(left: &CellMetadata, right: &CellMetadata) -> bool {
    left.foreground_kind == right.foreground_kind
        && left.foreground_palette_index == right.foreground_palette_index
        && left.underline_color_is_default == right.underline_color_is_default
        && left.background_color_is_default == right.background_color_is_default
}

fn same_text(left: &Cell, left_utf8: &[u8], right: &Cell, right_utf8: &[u8]) -> bool {
    text_span(left, left_utf8) == text_span(right, right_utf8)
}

pub(super) fn text_span<'a>(cell: &Cell, utf8: &'a [u8]) -> Option<&'a [u8]> {
    let start = usize::try_from(cell.utf8_offset).ok()?;
    let end = start.checked_add(usize::from(cell.utf8_len))?;
    utf8.get(start..end)
}

fn assemble(mut previous: Scene, fresh: Scene, rebuilt: &[u16]) -> Scene {
    previous.frame = fresh.frame;
    previous.settings = fresh.settings;
    previous.background = fresh.background;
    previous.backgrounds = merge_rows(previous.backgrounds, fresh.backgrounds, rebuilt, |quad| {
        quad.row
    });
    previous.inks = merge_rows(previous.inks, fresh.inks, rebuilt, |ink| ink.row);
    previous.sprites = merge_rows(previous.sprites, fresh.sprites, rebuilt, |sprites| {
        sprites.row
    });
    previous.decorations = merge_rows(previous.decorations, fresh.decorations, rebuilt, |quad| {
        quad.row
    });
    previous.cursor_blocks = fresh.cursor_blocks;
    previous.cursor_overlays = fresh.cursor_overlays;
    previous.glyphs = merge_rows(previous.glyphs, fresh.glyphs, rebuilt, |glyph| glyph.row);
    previous
}

pub(super) fn merge_rows<T>(
    previous: Vec<T>,
    fresh: Vec<T>,
    rebuilt: &[u16],
    row_of: impl Fn(&T) -> u16,
) -> Vec<T> {
    // Both inputs are already in row order, including after a constant scroll
    // translation. Merge once instead of sorting every retained primitive.
    let mut merged = Vec::with_capacity(previous.len() + fresh.len());
    let mut fresh = fresh.into_iter().peekable();
    for item in previous {
        let row = row_of(&item);
        if rebuilt.binary_search(&row).is_ok() {
            continue;
        }
        while fresh.peek().is_some_and(|item| row_of(item) < row) {
            merged.push(fresh.next().expect("peeked row"));
        }
        merged.push(item);
    }
    merged.extend(fresh);
    merged
}
