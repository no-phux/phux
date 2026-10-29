//! Resolve runtime cell styles and cursor primitives for the native painter.

use super::*;

pub(super) fn prepare_cursor(scene: &mut Scene) {
    scene.cursor_blocks.clear();
    scene.cursor_overlays.clear();
    let settings = &scene.settings;
    let Some(cursor) = draw::cursor(&scene.frame, &settings.theme(), settings.visibility()) else {
        return;
    };
    let geometry = scene.geometry;
    let bounds = geometry.cell_bounds(cursor.row, cursor.col, cursor.width);
    let color = cursor.color;
    let thickness = px(1. / geometry.scale);
    match cursor.style {
        CursorStyle::Block => scene.cursor_blocks.push((bounds, color)),
        CursorStyle::Bar => scene.cursor_overlays.push((
            Bounds::new(bounds.origin, size(thickness * 2., bounds.size.height)),
            color,
        )),
        CursorStyle::Underline => scene.cursor_overlays.push((
            Bounds::new(
                point(bounds.left(), bounds.bottom() - thickness * 2.),
                size(bounds.size.width, thickness * 2.),
            ),
            color,
        )),
        CursorStyle::BlockHollow => scene
            .cursor_overlays
            .extend(outline(bounds, thickness).map(|bounds| (bounds, color))),
    }
}

/// One cell's colours under the runtime's rules; `defaults` is
/// [`draw::default_colors`] for this frame and theme. Faint text paints at half alpha.
pub(super) fn cell_colors(
    cell: &Cell,
    metadata: CellMetadata,
    frame: &GridFrame,
    theme: &Theme<Hsla>,
    defaults: (Hsla, Hsla),
) -> CellColors {
    let colors = draw::cell_colors(cell, metadata, frame, theme, defaults);
    CellColors {
        foreground: colors.foreground,
        background: colors.background,
        underline: colors.underline,
        opacity: if colors.faint { 0.5 } else { 1. },
    }
}

/// The cell's face as an index into [`Fonts`]: bit 0 bold, bit 1 italic.
pub(super) fn style(cell: &Cell) -> usize {
    draw::Face::of(cell).index()
}

pub(super) fn hidden(cell: &Cell, settings: &Settings) -> bool {
    draw::hidden(cell, settings.visibility())
}

pub(super) fn cursor_visible(frame: &GridFrame, settings: &Settings) -> bool {
    draw::cursor_visible(frame, settings.visibility())
}

pub(super) fn cursor_color(frame: &GridFrame, settings: &Settings) -> Hsla {
    draw::cursor_color(frame, &settings.theme())
}

pub(super) fn solid_cursor(frame: &GridFrame, settings: &Settings) -> bool {
    draw::solid_cursor(frame, settings.visibility())
}

fn outline(bounds: Bounds<Pixels>, thickness: Pixels) -> [Bounds<Pixels>; 4] {
    [
        Bounds::new(bounds.origin, size(bounds.size.width, thickness)),
        Bounds::new(
            point(bounds.left(), bounds.bottom() - thickness),
            size(bounds.size.width, thickness),
        ),
        Bounds::new(bounds.origin, size(thickness, bounds.size.height)),
        Bounds::new(
            point(bounds.right() - thickness, bounds.top()),
            size(thickness, bounds.size.height),
        ),
    ]
}
