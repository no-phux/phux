//! Laser cursor. When the cursor jumps, a beam streaks from where it was to
//! where it landed and burns out behind it. The beam is painted over the
//! cached scene and never enters it, so a jump reshapes nothing.

use std::time::{Duration, Instant};

use gpui::{
    Anchor, Bounds, Hsla, PathBuilder, Pixels, Point, linear_color_stop, linear_gradient, point, px,
};

use super::geometry::Geometry;

/// How long a beam takes to burn out.
pub const SPAN: Duration = Duration::from_millis(170);

/// A painted cursor in cells: row, head column, width.
pub type Spot = (u16, u16, u16);

/// The last cursor seen and the beam in flight, if any.
#[derive(Clone, Copy, Debug, Default)]
pub struct Laser {
    last: Option<Spot>,
    beam: Option<Beam>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Beam {
    from: Spot,
    to: Spot,
    started: Instant,
}

impl Laser {
    /// Record the cursor this frame paints. A jump fires a beam; typing one
    /// character (a single cell to the right) does not. `None` (hidden,
    /// unfocused, or disabled) forgets the cursor so its return fires nothing.
    pub fn observe(&mut self, cursor: Option<Spot>, now: Instant) {
        let Some(cursor) = cursor else {
            *self = Self::default();
            return;
        };
        if let Some(last) = self.last
            && jumped(last, cursor)
        {
            self.beam = Some(Beam {
                from: last,
                to: cursor,
                started: now,
            });
        }
        self.last = Some(cursor);
        if !self.active(now) {
            self.beam = None;
        }
    }

    /// Whether a beam is still burning.
    pub fn active(&self, now: Instant) -> bool {
        self.beam
            .is_some_and(|beam| now.duration_since(beam.started) < SPAN)
    }

    /// Paint the beam in `color` over `geometry`. False once it has burned out.
    pub fn paint(
        &self,
        geometry: Geometry,
        color: Hsla,
        now: Instant,
        window: &mut gpui::Window,
    ) -> bool {
        let Some((hull, fade, angle)) = self.shape(geometry, now) else {
            return false;
        };
        let head = color.opacity(fade);
        let glow = geometry.cell_height.min(geometry.cell_width) * 0.35;
        // The halo first, then the core over it: a soft bloom around a hot line.
        for (outline, strength) in [(grow(&hull, glow), 0.35), (hull, 0.9)] {
            let mut path = PathBuilder::fill();
            path.add_polygon(&outline, true);
            let Ok(path) = path.build() else {
                continue;
            };
            window.paint_path(
                path,
                linear_gradient(
                    angle,
                    linear_color_stop(head.opacity(0.), 0.),
                    linear_color_stop(head.opacity(fade * strength), 1.),
                ),
            );
        }
        true
    }

    /// The beam's outline in window coordinates, its remaining brightness,
    /// and the CSS angle from tail to head.
    fn shape(&self, geometry: Geometry, now: Instant) -> Option<(Vec<Point<Pixels>>, f32, f32)> {
        let beam = self.beam?;
        let t = now.duration_since(beam.started).as_secs_f32() / SPAN.as_secs_f32();
        if t >= 1. {
            return None;
        }
        let cell = |(row, col, width): Spot| geometry.cell_bounds(row, col, width);
        let head = cell(beam.to);
        // The tail chases the head, fast at first, then lands with it.
        let tail = lerp(cell(beam.from), head, 1. - (1. - t).powi(3));
        let d = head.center() - tail.center();
        let angle = f32::from(d.x)
            .atan2(-f32::from(d.y))
            .to_degrees()
            .rem_euclid(360.);
        Some((hull(tail, head), 1. - t * t, angle))
    }
}

/// More than one cell right, any move left, or any change of row.
fn jumped(from: Spot, to: Spot) -> bool {
    from.0 != to.0 || to.1 < from.1 || to.1 > from.1 + from.2
}

fn lerp(from: Bounds<Pixels>, to: Bounds<Pixels>, t: f32) -> Bounds<Pixels> {
    let mix = |a: Point<Pixels>, b: Point<Pixels>| a + (b - a) * t;
    Bounds::from_corners(
        mix(from.origin, to.origin),
        mix(from.bottom_right(), to.bottom_right()),
    )
}

/// The convex hull of two rectangles, clockwise. At each corner it keeps the
/// outer of the two rectangles' corners, or both when neither covers the other.
fn hull(a: Bounds<Pixels>, b: Bounds<Pixels>) -> Vec<Point<Pixels>> {
    let mut outline = Vec::with_capacity(6);
    // Each corner's outward direction, and whether the corner further out
    // along x comes first clockwise.
    for (corner, sx, sy, x_first) in [
        (Anchor::TopLeft, -1., -1., true),
        (Anchor::TopRight, 1., -1., false),
        (Anchor::BottomRight, 1., 1., true),
        (Anchor::BottomLeft, -1., 1., false),
    ] {
        let (p, q) = (a.corner(corner), b.corner(corner));
        let out = |p: Point<Pixels>| (sx * f32::from(p.x), sy * f32::from(p.y));
        let ((ax, ay), (bx, by)) = (out(p), out(q));
        if ax >= bx && ay >= by {
            outline.push(p);
        } else if bx >= ax && by >= ay {
            outline.push(q);
        } else if x_first == (ax > bx) {
            outline.extend([p, q]);
        } else {
            outline.extend([q, p]);
        }
    }
    outline
}

/// Push every vertex `by` away from the hull's centroid.
fn grow(hull: &[Point<Pixels>], by: Pixels) -> Vec<Point<Pixels>> {
    let count = hull.len().max(1) as f32;
    let center = hull.iter().fold(point(px(0.), px(0.)), |sum, p| {
        point(sum.x + p.x, sum.y + p.y)
    });
    let center = point(center.x / count, center.y / count);
    hull.iter()
        .map(|p| {
            let (dx, dy) = (f32::from(p.x - center.x), f32::from(p.y - center.y));
            let length = dx.hypot(dy).max(f32::EPSILON);
            point(p.x + by * (dx / length), p.y + by * (dy / length))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    fn geometry() -> Geometry {
        Geometry {
            bounds: Bounds::new(point(px(0.), px(0.)), size(px(800.), px(480.))),
            cell_width: px(10.),
            cell_height: px(20.),
            baseline: px(15.),
            scale: 1.,
            cols: 80,
            rows: 24,
        }
    }

    #[test]
    fn typing_one_cell_right_fires_nothing_but_a_jump_does() {
        let now = Instant::now();
        let mut laser = Laser::default();
        laser.observe(Some((0, 0, 1)), now);
        laser.observe(Some((0, 1, 1)), now);
        assert!(!laser.active(now));
        laser.observe(Some((0, 2, 2)), now);
        laser.observe(Some((0, 4, 1)), now);
        assert!(!laser.active(now), "a wide character advances two cells");
        laser.observe(Some((5, 4, 1)), now);
        assert!(laser.active(now));
        assert!(!laser.active(now + SPAN));
    }

    #[test]
    fn backspace_and_a_long_jump_right_fire() {
        let now = Instant::now();
        for to in [(0, 9, 1), (0, 20, 1)] {
            let mut laser = Laser::default();
            laser.observe(Some((0, 10, 1)), now);
            laser.observe(Some(to), now);
            assert!(laser.active(now), "{to:?}");
        }
    }

    #[test]
    fn a_hidden_cursor_forgets_where_it_was() {
        let now = Instant::now();
        let mut laser = Laser::default();
        laser.observe(Some((0, 0, 1)), now);
        laser.observe(None, now);
        laser.observe(Some((10, 40, 1)), now);
        assert!(!laser.active(now));
    }

    #[test]
    fn a_settled_cursor_lets_the_beam_burn_out() {
        let now = Instant::now();
        let mut laser = Laser::default();
        laser.observe(Some((0, 0, 1)), now);
        laser.observe(Some((3, 0, 1)), now);
        laser.observe(Some((3, 0, 1)), now + SPAN / 2);
        assert!(laser.active(now + SPAN / 2));
        laser.observe(Some((3, 0, 1)), now + SPAN);
        assert!(laser.beam.is_none());
    }

    #[test]
    fn the_beam_spans_both_cells_then_shrinks_onto_the_head() {
        let now = Instant::now();
        let mut laser = Laser::default();
        laser.observe(Some((0, 0, 1)), now);
        laser.observe(Some((0, 10, 1)), now);
        let (outline, fade, angle) = laser.shape(geometry(), now).expect("beam");
        let left = outline
            .iter()
            .map(|p| f32::from(p.x))
            .fold(f32::MAX, f32::min);
        let right = outline
            .iter()
            .map(|p| f32::from(p.x))
            .fold(f32::MIN, f32::max);
        assert_eq!((left, right, fade), (0., 110., 1.));
        assert!((angle - 90.).abs() < 1e-3, "tail to head points right");
        let (late, fade, _) = laser
            .shape(geometry(), now + SPAN.mul_f32(0.9))
            .expect("still burning");
        let left = late.iter().map(|p| f32::from(p.x)).fold(f32::MAX, f32::min);
        assert!(left > 99., "tail has nearly caught the head: {left}");
        assert!(fade < 0.25);
        assert!(laser.shape(geometry(), now + SPAN).is_none());
    }

    #[test]
    fn hull_of_two_offset_cells_is_a_hexagon() {
        let g = geometry();
        let (a, b) = (g.cell_bounds(0, 0, 1), g.cell_bounds(3, 5, 1));
        assert_eq!(hull(a, b).len(), 6);
        assert_eq!(hull(a, g.cell_bounds(0, 5, 1)).len(), 4, "one row is a box");
    }
}
