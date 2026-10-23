//! Merge cell backgrounds and prepare rules, patterned underlines, and blocks.

use super::*;

impl Builder {
    /// Extend the row's open run, or start one. The whole surface is already
    /// filled with the default background, so those cells add no quad; a
    /// screen of text is then a handful of quads instead of one per cell.
    pub(super) fn background_cell(&mut self, row: u16, col: u16, color: Hsla) {
        if let Some(run) = &mut self.run
            && run.row == row
            && run.end == col
            && run.color == color
        {
            run.end += 1;
            return;
        }
        self.flush_background();
        if color != self.scene.background {
            self.run = Some(BackgroundRun {
                row,
                start: col,
                end: col + 1,
                color,
            });
        }
    }

    pub(super) fn flush_background(&mut self) {
        if let Some(run) = self.run.take() {
            let bounds = self
                .scene
                .geometry
                .cell_bounds(run.row, run.start, run.end - run.start);
            self.scene.backgrounds.push(RowQuad {
                row: run.row,
                bounds,
                color: run.color,
            });
        }
    }

    pub(super) fn prepare_decorations(
        &mut self,
        row: u16,
        cell: &Cell,
        bounds: Bounds<Pixels>,
        mut colors: CellColors,
    ) {
        colors.foreground = colors.foreground.opacity(colors.opacity);
        colors.underline = colors.underline.opacity(colors.opacity);
        let thickness = px(1. / self.scene.geometry.scale);
        let bottom = bounds.bottom() - thickness * 2.;
        match cell.underline {
            1 => self.rule(row, bounds, bottom, thickness, colors.underline),
            2 => {
                self.rule(row, bounds, bottom, thickness, colors.underline);
                self.rule(
                    row,
                    bounds,
                    bottom - thickness * 2.,
                    thickness,
                    colors.underline,
                );
            }
            3..=5 => self.patterned_underline(
                row,
                cell.underline,
                bounds,
                bottom,
                thickness,
                colors.underline,
            ),
            _ => (),
        }
        if cell.flags & CELL_STRIKETHROUGH != 0 {
            self.rule(
                row,
                bounds,
                bounds.top() + bounds.size.height * 0.55,
                thickness,
                colors.foreground,
            );
        }
        if cell.flags & CELL_OVERLINE != 0 {
            self.rule(row, bounds, bounds.top(), thickness, colors.foreground);
        }
    }

    fn rule(
        &mut self,
        row: u16,
        bounds: Bounds<Pixels>,
        y: Pixels,
        thickness: Pixels,
        color: Hsla,
    ) {
        self.scene.decorations.push(RowQuad {
            row,
            bounds: Bounds::new(point(bounds.left(), y), size(bounds.size.width, thickness)),
            color,
        });
    }

    fn patterned_underline(
        &mut self,
        row: u16,
        kind: u8,
        bounds: Bounds<Pixels>,
        y: Pixels,
        thickness: Pixels,
        color: Hsla,
    ) {
        let step = match kind {
            3 => 1.,
            4 => 2.,
            _ => 4.,
        };
        let mut x = px(0.);
        let mut index = 0;
        while x < bounds.size.width {
            let offset = if kind == 3 {
                [0., 1., 2., 1.][index % 4]
            } else {
                0.
            };
            let width = if kind == 5 { thickness * 2. } else { thickness };
            self.scene.decorations.push(RowQuad {
                row,
                bounds: Bounds::new(
                    point(bounds.left() + x, y - thickness * offset),
                    size(width.min(bounds.size.width - x), thickness),
                ),
                color,
            });
            x += thickness * step;
            index += 1;
        }
    }
}

pub(super) fn block_bounds(
    bounds: Bounds<Pixels>,
    region: BlockRegion,
    scale: f32,
) -> Bounds<Pixels> {
    let edge = |origin: Pixels, length: Pixels, fraction: u8| {
        px(
            ((f32::from(origin) + f32::from(length) * f32::from(fraction) / 8.) * scale).round()
                / scale,
        )
    };
    let left = edge(bounds.left(), bounds.size.width, region.x);
    let top = edge(bounds.top(), bounds.size.height, region.y);
    let right = edge(bounds.left(), bounds.size.width, region.x + region.width);
    let bottom = edge(bounds.top(), bounds.size.height, region.y + region.height);
    Bounds::new(point(left, top), size(right - left, bottom - top))
}
