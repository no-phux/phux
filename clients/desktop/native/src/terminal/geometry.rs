use super::Settings;
use gpui::{self, Bounds, Pixels, Point, point, px, size};

/// One logical-pixel geometry for painting, hit testing, and the IME rectangle.
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
        let (x, y) = (self.columns(), self.lines());
        let (row, col) = (u32::from(row), u32::from(col));
        Bounds::from_corners(
            point(x.edge(col), y.edge(row)),
            point(x.edge(col + u32::from(width)), y.edge(row + 1)),
        )
    }

    /// No clamping: positions in clipped cells, padding, or outside the surface
    /// must not accidentally target the last terminal cell.
    pub fn hit(self, position: Point<Pixels>) -> Option<(u16, u16)> {
        if !self.bounds.contains(&position) {
            return None;
        }
        Some((
            self.lines().hit(position.y)?,
            self.columns().hit(position.x)?,
        ))
    }

    /// The cell under `position`, or the nearest cell when the point misses
    /// the grid. `None` when the grid has no cells. Drags that leave the
    /// surface keep a real cell instead of the padding.
    pub fn hit_clamped(self, position: Point<Pixels>) -> Option<(u16, u16)> {
        if self.cols == 0 || self.rows == 0 {
            return None;
        }
        self.hit(position).or_else(|| {
            Some((
                self.lines().clamp(position.y),
                self.columns().clamp(position.x),
            ))
        })
    }

    fn columns(self) -> Track {
        Track {
            origin: self.bounds.left(),
            step: self.cell_width,
            count: self.cols,
            scale: self.scale,
        }
    }

    fn lines(self) -> Track {
        Track {
            origin: self.bounds.top(),
            step: self.cell_height,
            count: self.rows,
            scale: self.scale,
        }
    }
}

/// One axis of a grid snapped to device pixels: `count` cells of `step` from `origin`.
#[derive(Clone, Copy)]
struct Track {
    origin: Pixels,
    step: Pixels,
    count: u16,
    scale: f32,
}

impl Track {
    /// Compute both sides from the same grid edge, rather than accumulating
    /// fractional widths. Adjacent translucent quads then neither overlap nor gap.
    fn edge(self, index: u32) -> Pixels {
        px(
            ((f32::from(self.origin) + f32::from(self.step) * index as f32) * self.scale).round()
                / self.scale,
        )
    }

    fn hit(self, value: Pixels) -> Option<u16> {
        let mut index = ((value - self.origin) / self.step).floor() as u32;
        // Division can land one cell early at an exact snapped edge. Resolve
        // against the same edges that painting uses, not a second ideal grid.
        if index > 0 && value < self.edge(index) {
            index -= 1;
        } else if value >= self.edge(index.saturating_add(1)) {
            index = index.saturating_add(1);
        }
        (index < u32::from(self.count)).then_some(index as u16)
    }

    /// [`Self::hit`], or the nearest cell when `value` is outside the grid.
    fn clamp(self, value: Pixels) -> u16 {
        self.hit(value).unwrap_or(if value < self.origin {
            0
        } else {
            self.count.saturating_sub(1)
        })
    }
}

/// A pointer position in the integer cell grid a resize reports as `cell_px`.
///
/// `x` / `cell_width` and `y` / `cell_height` name `col` and `row`. That is the
/// grid the server divides `INPUT_MOUSE` pixels by, and the grid the selection
/// gesture uses for its intra-cell threshold.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfacePoint {
    pub row: u16,
    pub col: u16,
    pub x: f64,
    pub y: f64,
    pub cell_width: u32,
    pub cell_height: u32,
}

/// Map a window point onto [`SurfacePoint`].
///
/// The cell comes from the same device-pixel edges as paint. The fraction
/// inside that painted cell is then placed inside the rounded integer cell, so
/// a non-integer painted size still names the cell under the pointer. `clamped`
/// keeps a drag that leaves the grid on the nearest cell.
pub fn surface_at(
    geometry: Geometry,
    position: Point<Pixels>,
    clamped: bool,
) -> Option<SurfacePoint> {
    let (row, col) = if clamped {
        geometry.hit_clamped(position)?
    } else {
        geometry.hit(position)?
    };
    let (width, height) = cell_px(geometry.cell_width, geometry.cell_height);
    let bounds = geometry.cell_bounds(row, col, 1);
    Some(SurfacePoint {
        row,
        col,
        x: surface_axis(
            col,
            geometry.cols,
            width,
            position.x,
            bounds.left(),
            bounds.size.width,
        ),
        y: surface_axis(
            row,
            geometry.rows,
            height,
            position.y,
            bounds.top(),
            bounds.size.height,
        ),
        cell_width: u32::from(width),
        cell_height: u32::from(height),
    })
}

/// Rounded cell size reported with a resize. The server divides mouse pixels
/// by this pair.
pub(crate) fn cell_px(width: Pixels, height: Pixels) -> (u16, u16) {
    (rounded_px(width), rounded_px(height))
}

fn rounded_px(pixels: Pixels) -> u16 {
    f32::from(pixels).round().clamp(1.0, f32::from(u16::MAX)) as u16
}

/// Whole pixels of the reported cell, from the fraction inside the painted
/// cell. Floored and clamped to the grid, matching the web `surface_pixel`
/// contract: `floor(x / cell)` stays on `index`.
fn surface_axis(
    index: u16,
    count: u16,
    cell: u16,
    value: Pixels,
    origin: Pixels,
    span: Pixels,
) -> f64 {
    let frac = if f32::from(span) <= f32::EPSILON {
        0.0
    } else {
        // Stay strictly inside this cell. The shared far edge belongs to the
        // next cell, and a drag past the grid must not spill into it either.
        ((f32::from(value) - f32::from(origin)) / f32::from(span)).clamp(0.0, 0.999)
    };
    let cell = f64::from(cell);
    let pixel = (f64::from(u32::from(index)) * cell + f64::from(frac) * cell).floor();
    let max = (f64::from(u32::from(count)) * cell - 1.0).max(0.0);
    pixel.clamp(0.0, max)
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
        assert_eq!(geometry.hit_clamped(point(px(9.), px(20.))), Some((0, 0)));
        assert_eq!(geometry.hit_clamped(point(px(36.), px(20.))), Some((0, 2)));
        assert_eq!(geometry.hit_clamped(point(px(12.), px(55.))), Some((1, 0)));
        assert_eq!(geometry.hit_clamped(point(px(0.), px(0.))), Some((0, 0)));
        let empty = Geometry {
            cols: 0,
            rows: 2,
            ..geometry
        };
        assert_eq!(empty.hit_clamped(point(px(12.), px(20.))), None);
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

    #[test]
    fn surface_pixels_name_the_painted_cell_not_the_pane() {
        let geometry = Geometry {
            bounds: Bounds::new(point(px(0.), px(0.)), size(px(2000.), px(800.))),
            cell_width: px(9.5),
            cell_height: px(17.3),
            scale: 2.,
            cols: 80,
            rows: 24,
            ..Default::default()
        };
        let cell = geometry.cell_bounds(3, 40, 1);
        let surface = surface_at(geometry, cell.center(), false).expect("center hits");
        assert_eq!((surface.row, surface.col), (3, 40));
        assert_eq!((surface.cell_width, surface.cell_height), (10, 17));
        assert_eq!(surface.x.fract(), 0.0);
        assert_eq!(surface.y.fract(), 0.0);
        assert_eq!(
            (surface.x / f64::from(surface.cell_width)).floor() as u16,
            40
        );
        assert_eq!(
            (surface.y / f64::from(surface.cell_height)).floor() as u16,
            3
        );
        let grid_bottom = geometry.cell_bounds(23, 0, 1).bottom();
        assert!(geometry.bounds.bottom() > grid_bottom);
        let below = point(cell.center().x, grid_bottom + px(4.));
        assert_eq!(surface_at(geometry, below, false), None);
        let clamped = surface_at(geometry, below, true).expect("drag stays on the grid");
        assert_eq!(clamped.row, 23);
        assert_eq!(
            (clamped.y / f64::from(clamped.cell_height)).floor() as u16,
            23
        );
        assert!(clamped.y < f64::from(u32::from(geometry.rows) * clamped.cell_height));
    }
}
