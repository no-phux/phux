use super::{Settings, gpui};
use gpui::{Bounds, Pixels, Point, point, px, size};

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
        let cell_width = snap(sample.width().max(px(1.)), scale);
        let cell_height = snap(
            px(settings.font_size * settings.line_height).max(sample.ascent + sample.descent),
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
        Bounds::new(
            self.bounds.origin
                + point(
                    self.cell_width * f32::from(col),
                    self.cell_height * f32::from(row),
                ),
            size(self.cell_width * f32::from(width), self.cell_height),
        )
    }

    /// No clamping: positions in clipped cells, padding, or outside the surface
    /// must not accidentally target the last terminal cell.
    pub fn hit(self, position: Point<Pixels>) -> Option<(u16, u16)> {
        if !self.bounds.contains(&position) {
            return None;
        }
        let offset = position - self.bounds.origin;
        let col = (offset.x / self.cell_width).floor() as u16;
        let row = (offset.y / self.cell_height).floor() as u16;
        (col < self.cols && row < self.rows).then_some((row, col))
    }
}

fn snap(value: Pixels, scale: f32) -> Pixels {
    px((f32::from(value) * scale).ceil() / scale)
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
}
