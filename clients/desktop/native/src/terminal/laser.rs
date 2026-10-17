//! Laser cursor. When the cursor jumps, a beam streaks from where it was to
//! where it landed and burns out behind it. The beam is painted over the
//! cached scene and never enters it, so a jump reshapes nothing.

use std::time::{Duration, Instant};

use gpui::{
    Bounds, Hsla, PathBuilder, Pixels, Point, linear_color_stop, linear_gradient, point, px,
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
        let corners = [tail, head].iter().flat_map(corners).collect::<Vec<_>>();
        let (dx, dy) = (
            f32::from(head.center().x - tail.center().x),
            f32::from(head.center().y - tail.center().y),
        );
        let angle = dx.atan2(-dy).to_degrees().rem_euclid(360.);
        Some((hull(corners), 1. - t * t, angle))
    }
}

/// More than one cell right, any move left, or any change of row.
fn jumped(from: Spot, to: Spot) -> bool {
    from.0 != to.0 || to.1 < from.1 || to.1 > from.1 + from.2
}

fn lerp(from: Bounds<Pixels>, to: Bounds<Pixels>, t: f32) -> Bounds<Pixels> {
    let mix = |a: Pixels, b: Pixels| a + (b - a) * t;
    Bounds::from_corners(
        point(mix(from.left(), to.left()), mix(from.top(), to.top())),
        point(
            mix(from.right(), to.right()),
            mix(from.bottom(), to.bottom()),
        ),
    )
}

fn corners(bounds: &Bounds<Pixels>) -> [Point<Pixels>; 4] {
    [
        bounds.origin,
        bounds.top_right(),
        bounds.bottom_right(),
        bounds.bottom_left(),
    ]
}

/// Convex hull, counter-clockwise (Andrew's monotone chain).
fn hull(mut points: Vec<Point<Pixels>>) -> Vec<Point<Pixels>> {
    points.sort_by(|a, b| {
        f32::from(a.x)
            .total_cmp(&f32::from(b.x))
            .then(f32::from(a.y).total_cmp(&f32::from(b.y)))
    });
    points.dedup();
    if points.len() < 3 {
        return points;
    }
    let cross = |o: Point<Pixels>, a: Point<Pixels>, b: Point<Pixels>| {
        f32::from(a.x - o.x) * f32::from(b.y - o.y) - f32::from(a.y - o.y) * f32::from(b.x - o.x)
    };
    let mut lower: Vec<Point<Pixels>> = Vec::new();
    for &p in &points {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0. {
            lower.pop();
        }
        lower.push(p);
    }
    let mut upper: Vec<Point<Pixels>> = Vec::new();
    for &p in points.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0. {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
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
        let points = [g.cell_bounds(0, 0, 1), g.cell_bounds(3, 5, 1)]
            .iter()
            .flat_map(corners)
            .collect();
        assert_eq!(hull(points).len(), 6);
    }
}
