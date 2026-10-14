use super::Settings;
use gpui::{self, Bounds, Pixels, Point, point, px, size};

/// One logical-pixel geometry for painting and future IME/mouse hit testing.
#[derive(Clone, Copy, Debug, Default)]
pub struct Geometry {
    pub bounds: Bounds<Pixels>,
    pub cell_width: Pixels,
    pub cell_height: Pixels,
    pub baseline: Pixels,
    pub scale: f32,
    pub cols: u16,
    pub rows: u16,
}

impl Geometry {
    pub(super) fn measure(
        settings: &Settings,
        bounds: Bounds<Pixels>,
        cols: u16,
        rows: u16,
        window: &gpui::Window,
    ) -> Self {
        let sample = window.text_system().shape_line(
            "M".into(),
            px(settings.font_size),
            &[gpui::TextRun {
                len: 1,
                font: settings.font.clone(),
                ..Default::default()
            }],
            None,
        );
        let scale = window.scale_factor();
        // Layout origins can be fractional even when cell dimensions are not.
        // Snap the grid too, otherwise every background row and glyph mask
        // straddles device pixels and exposes horizontal antialiasing seams.
        let origin = point(snap(bounds.left(), scale), snap(bounds.top(), scale));
        let bounds = Bounds::new(
            origin,
            size(
                (bounds.right() - origin.x).max(px(0.)),
                (bounds.bottom() - origin.y).max(px(0.)),
            ),
        );
        let cell_width = snap(
            (sample.width() * settings.cell_width_scale).max(px(1.)),
            scale,
        );
        let cell_height = snap(
            (px(settings.font_size * settings.line_height).max(sample.ascent + sample.descent)
                * settings.cell_height_scale)
                .max(px(1.)),
            scale,
        );
        Self {
            bounds,
            cell_width,
            cell_height,
            baseline: (cell_height - sample.ascent - sample.descent) / 2. + sample.ascent,
            scale,
            cols,
            rows,
        }
    }

    pub fn cell_bounds(self, row: u16, col: u16, width: u16) -> Bounds<Pixels> {
        let left = edge(
            self.bounds.left(),
            self.cell_width,
            u32::from(col),
            self.scale,
        );
        let right = edge(
            self.bounds.left(),
            self.cell_width,
            u32::from(col) + u32::from(width),
            self.scale,
        );
        let top = edge(
            self.bounds.top(),
            self.cell_height,
            u32::from(row),
            self.scale,
        );
        let bottom = edge(
            self.bounds.top(),
            self.cell_height,
            u32::from(row) + 1,
            self.scale,
        );
        Bounds::new(point(left, top), size(right - left, bottom - top))
    }

    /// No clamping: positions in clipped cells, padding, or outside the surface
    /// must not accidentally target the last terminal cell.
    pub fn hit(self, position: Point<Pixels>) -> Option<(u16, u16)> {
        if !self.bounds.contains(&position) {
            return None;
        }
        let col = hit_index(
            self.bounds.left(),
            self.cell_width,
            position.x,
            self.cols,
            self.scale,
        )?;
        let row = hit_index(
            self.bounds.top(),
            self.cell_height,
            position.y,
            self.rows,
            self.scale,
        )?;
        Some((row, col))
    }
}

fn snap(value: Pixels, scale: f32) -> Pixels {
    px((f32::from(value) * scale).ceil() / scale)
}

/// Compute both sides from the same grid edge, rather than accumulating
/// fractional widths. Adjacent translucent quads then neither overlap nor gap.
fn edge(origin: Pixels, step: Pixels, index: u32, scale: f32) -> Pixels {
    px(((f32::from(origin) + f32::from(step) * index as f32) * scale).round() / scale)
}

fn hit_index(origin: Pixels, step: Pixels, value: Pixels, count: u16, scale: f32) -> Option<u16> {
    let mut index = ((value - origin) / step).floor() as u32;
    // Division can land one cell early at an exact snapped edge. Resolve
    // against the same edges that painting uses, not a second ideal grid.
    if index > 0 && value < edge(origin, step, index, scale) {
        index -= 1;
    } else if value >= edge(origin, step, index.saturating_add(1), scale) {
        index = index.saturating_add(1);
    }
    (index < u32::from(count)).then_some(index as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_hits_match_cell_bounds_at_fractional_scale_and_clip() {
        let geometry = Geometry {
            bounds: Bounds::new(point(px(10.), px(20.)), size(px(25.), px(50.))),
            cell_width: snap(px(8.1), 1.5),
            cell_height: px(16.),
            baseline: px(12.),
            scale: 1.5,
            cols: 3,
            rows: 2,
        };
        assert_eq!(geometry.hit(point(px(10.), px(20.))), Some((0, 0)));
        assert_eq!(
            geometry.hit(geometry.cell_bounds(1, 2, 1).origin),
            Some((1, 2))
        );
        assert_eq!(geometry.hit(point(px(9.), px(20.))), None);
        assert_eq!(geometry.hit(point(px(36.), px(20.))), None);
        assert_eq!(geometry.hit(point(px(12.), px(55.))), None);
        assert_eq!(
            geometry.cell_bounds(0, 1, 2).size.width,
            geometry.cell_width * 2.
        );
    }

    #[test]
    fn fractional_scale_edges_are_shared_and_device_aligned() {
        for scale in [1., 1.25, 1.5, 2.] {
            let geometry = Geometry {
                bounds: Bounds::new(
                    point(snap(px(0.3), scale), snap(px(0.7), scale)),
                    size(px(1000.), px(1000.)),
                ),
                cell_width: snap(px(8.1), scale),
                cell_height: snap(px(17.3), scale),
                scale,
                cols: 80,
                rows: 40,
                ..Default::default()
            };
            for row in 0..39 {
                let bounds = geometry.cell_bounds(row, 0, 1);
                assert_eq!(bounds.bottom(), geometry.cell_bounds(row + 1, 0, 1).top());
                assert_eq!(bounds.right(), geometry.cell_bounds(row, 1, 1).left());
                for edge in [bounds.top(), bounds.bottom(), bounds.left(), bounds.right()] {
                    let device = f32::from(edge) * scale;
                    assert!((device - device.round()).abs() < 0.001);
                }
                assert_eq!(
                    geometry.hit(bounds.center()),
                    Some((row, 0)),
                    "painting and input disagree at scale {scale}"
                );
                for col in 0..79 {
                    let cell = geometry.cell_bounds(row, col, 1);
                    assert_eq!(geometry.hit(cell.origin), Some((row, col)));
                    assert_eq!(
                        geometry.hit(point(cell.right(), cell.top())),
                        Some((row, col + 1)),
                        "shared edge must hit the following cell"
                    );
                }
            }
        }
    }
}
