//! What the reef paints with: Rune's shapes, which are flat fills and
//! strokes, standing in for the prototype's canvas. A gradient is bands, a
//! radial glow is rings of discs, and a clip is a polygon cut to shape.
//!
//! Rune draws each triangle of a fill as its bounding box, and ear-clipping
//! fans a long polygon out from one corner, so a wide one costs the GPU many
//! times its area. Big opaque areas go down as bands and columns that each
//! overlap the next by a screen point (painted over, so no seam shows), and
//! no area is painted twice for a gradient.

use std::f32::consts::TAU;

use rune::color::srgb_decode;
use rune::{Color, LineCap, Shape};

use crate::reef::{Pt, Reef, Rgb};

/// The prototype's `rgba()`: sRGB 0–255 with straight alpha.
pub fn rgba(c: Rgb, a: f32) -> Color {
    let ch = |v: f32| srgb_decode((v / 255.0).clamp(0.0, 1.0));
    Color {
        r: ch(c[0]),
        g: ch(c[1]),
        b: ch(c[2]),
        a: a.clamp(0.0, 1.0),
    }
}

/// Paints world points through the camera, as the prototype's `setT`.
pub struct Pen {
    shape: Shape,
    /// Times every alpha painted: a fry is drawn at 0.8.
    pub fade: f32,
    /// Screen is `world * z + o`.
    z: f32,
    o: Pt,
}

impl Pen {
    pub fn new() -> Pen {
        Pen {
            shape: Shape::new(),
            fade: 1.0,
            z: 1.0,
            o: [0.0, 0.0],
        }
    }

    /// Follow the camera with parallax `p` (1 is the scene itself).
    pub fn set(&mut self, reef: &Reef, p: f32) {
        self.z = 1.0 + (reef.cam.z - 1.0) * p;
        let to = reef.to_screen(p);
        self.o = to([0.0, 0.0]);
    }

    /// Paint in screen points.
    pub fn screen(&mut self) {
        self.z = 1.0;
        self.o = [0.0, 0.0];
    }

    pub fn zoom(&self) -> f32 {
        self.z
    }

    pub fn into_shape(self) -> Shape {
        self.shape
    }

    fn at(&self, q: Pt) -> Pt {
        [q[0] * self.z + self.o[0], q[1] * self.z + self.o[1]]
    }

    pub fn fill(&mut self, pts: &[Pt], c: Rgb, a: f32) {
        let a = a * self.fade;
        if a < 0.004 || pts.len() < 3 {
            return;
        }
        let pts: Vec<Pt> = pts.iter().map(|&q| self.at(q)).collect();
        self.shape = std::mem::take(&mut self.shape).fill(pts, rgba(c, a));
    }

    /// A polyline `w` world points wide.
    pub fn stroke(&mut self, pts: &[Pt], closed: bool, w: f32, c: Rgb, a: f32) {
        let a = a * self.fade;
        if a < 0.004 || pts.len() < 2 {
            return;
        }
        let pts: Vec<Pt> = pts.iter().map(|&q| self.at(q)).collect();
        let w = w * self.z;
        self.shape =
            std::mem::take(&mut self.shape).stroke_with(pts, closed, w, LineCap::Round, rgba(c, a));
    }

    pub fn line(&mut self, a: Pt, b: Pt, w: f32, c: Rgb, al: f32) {
        self.stroke(&[a, b], false, w, c, al);
    }

    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: Rgb, a: f32) {
        self.fill(&[[x, y], [x + w, y], [x + w, y + h], [x, y + h]], c, a);
    }

    /// Points round an ellipse at `c`, turned `rot`, from angle `from` to `to`.
    pub fn ellipse(&self, c: Pt, rx: f32, ry: f32, rot: f32, from: f32, to: f32) -> Vec<Pt> {
        let span = ((to - from).abs() / TAU).min(1.0);
        // Round at any size: a small circle still takes 16 sides.
        let n = ((rx.max(ry) * self.z * 0.7).max(16.0) * span) as usize;
        ellipse_n(c, rx, ry, rot, from, to, n.clamp(4, 64))
    }

    pub fn disc(&mut self, c: Pt, r: f32, col: Rgb, a: f32) {
        let mut pts = self.ellipse(c, r, r, 0.0, 0.0, TAU);
        pts.pop();
        self.fill(&pts, col, a);
    }

    pub fn ring(&mut self, c: Pt, r: f32, w: f32, col: Rgb, a: f32) {
        let mut pts = self.ellipse(c, r, r, 0.0, 0.0, TAU);
        pts.pop();
        self.stroke(&pts, true, w, col, a);
    }

    /// The prototype's `glowDot`: `a` at the centre fading out by `r`.
    pub fn glow(&mut self, c: Pt, r: f32, col: Rgb, a: f32) {
        self.glow_cut(c, r, col, a, f32::MAX);
    }

    /// A glow kept above `y_max`.
    pub fn glow_cut(&mut self, c: Pt, r: f32, col: Rgb, a: f32, y_max: f32) {
        if a < 0.01 || r * self.z < 0.5 {
            return;
        }
        let n = ((r * self.z / 4.0) as usize).clamp(3, 16);
        // Soft and faint, a ring needs few sides; each side is a fan
        // triangle the GPU fills the box of.
        let sides = ((r * self.z * 0.5) as usize).clamp(12, 64);
        for k in 0..n {
            let rk = r * (1.0 - k as f32 / n as f32);
            let mut pts = ellipse_n(c, rk, rk, 0.0, 0.0, TAU, sides);
            pts.pop();
            if y_max < f32::MAX {
                pts = clip_half(&pts, y_max);
            }
            // Stacked, the rings fall off linearly as the prototype's gradient.
            self.fill(&pts, col, a * 0.9 / n as f32);
        }
    }

    /// A vertical gradient over a rectangle: `n` bands, each reaching a
    /// screen point into the next, which paints over it. `at(k)` is the
    /// colour `k` of the way down.
    pub fn vgrad(
        &mut self,
        x: f32,
        y0: f32,
        w: f32,
        y1: f32,
        n: usize,
        at: impl Fn(f32) -> (Rgb, f32),
    ) {
        let band = (y1 - y0) / n as f32;
        let ov = 1.0 / self.z;
        for k in 0..n {
            let (c, a) = at((k as f32 + 0.5) / n as f32);
            let h = if k + 1 == n { band } else { band + ov };
            self.rect(x, y0 + band * k as f32, w, h, c, a);
        }
    }

    /// The opaque `poly` with a vertical gradient from `y0` to `y1` in `n`
    /// bands, the first and last running on past them to its edge.
    pub fn vgrad_poly(&mut self, poly: &[Pt], y0: f32, y1: f32, n: usize, at: impl Fn(f32) -> Rgb) {
        let ov = 1.0 / self.z;
        let y = |k: usize| y0 + (y1 - y0) * k as f32 / n as f32;
        for k in 0..n {
            let mut band = poly.to_vec();
            if k > 0 {
                band = clip_plane(&band, |p| p[1] - y(k));
            }
            if k + 1 < n {
                band = clip_plane(&band, |p| y(k + 1) + ov - p[1]);
            }
            self.fill_opaque(&band, at((k as f32 + 0.5) / n as f32));
        }
    }

    /// An opaque fill, a long outline cut into overlapping columns so its
    /// triangles stay narrow.
    pub fn fill_opaque(&mut self, poly: &[Pt], c: Rgb) {
        for column in columns(poly, 1.0 / self.z) {
            self.fill(&column, c, 1.0);
        }
    }
}

/// `poly` in columns at most `WIDE` across, each reaching `ov` into the next;
/// a short outline stays whole.
fn columns(poly: &[Pt], ov: f32) -> Vec<Vec<Pt>> {
    const WIDE: f32 = 48.0;
    if poly.len() <= 8 {
        return vec![poly.to_vec()];
    }
    let (lo, hi) = poly.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
        (lo.min(p[0]), hi.max(p[0]))
    });
    let mut out = Vec::new();
    let mut x = lo;
    while x < hi {
        let right = x + WIDE;
        let left = clip_plane(poly, |p| p[0] - x);
        out.push(if right >= hi {
            left
        } else {
            clip_plane(&left, |p| right + ov - p[0])
        });
        x = right;
    }
    out
}

/// `n` steps round an ellipse at `c`, turned `rot`, from angle `from` to `to`.
fn ellipse_n(c: Pt, rx: f32, ry: f32, rot: f32, from: f32, to: f32, n: usize) -> Vec<Pt> {
    let (s, co) = rot.sin_cos();
    (0..=n)
        .map(|k| {
            let a = from + (to - from) * k as f32 / n as f32;
            let (x, y) = (rx * a.cos(), ry * a.sin());
            [c[0] + x * co - y * s, c[1] + x * s + y * co]
        })
        .collect()
}

/// The part of `poly` above `y` (smaller y).
pub fn clip_half(poly: &[Pt], y: f32) -> Vec<Pt> {
    clip_plane(poly, |p| y - p[1])
}

/// Sutherland–Hodgman against one half-plane: keeps where `side` is ≥ 0.
fn clip_plane(poly: &[Pt], side: impl Fn(Pt) -> f32) -> Vec<Pt> {
    let mut out = Vec::with_capacity(poly.len() + 2);
    for (i, &b) in poly.iter().enumerate() {
        let a = poly[(i + poly.len() - 1) % poly.len()];
        let (da, db) = (side(a), side(b));
        if (da >= 0.0) != (db >= 0.0) {
            let k = da / (da - db);
            out.push([a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k]);
        }
        if db >= 0.0 {
            out.push(b);
        }
    }
    out
}

/// `poly` cut to the convex polygon `by`, in either winding.
pub fn clip_convex(poly: &[Pt], by: &[Pt]) -> Vec<Pt> {
    let area: f32 = (0..by.len())
        .map(|i| {
            let (a, b) = (by[i], by[(i + 1) % by.len()]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum();
    let sign = area.signum();
    let mut out = poly.to_vec();
    for i in 0..by.len() {
        if out.len() < 3 {
            break;
        }
        let (a, b) = (by[i], by[(i + 1) % by.len()]);
        if a == b {
            continue;
        }
        out = clip_plane(&out, |p| {
            sign * ((b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]))
        });
    }
    out
}

/// A shape's own frame in the world: scaled by `sx` on x, turned, moved.
#[derive(Clone, Copy)]
pub struct Xf {
    pub at: Pt,
    pub cos: f32,
    pub sin: f32,
    pub sx: f32,
}

impl Xf {
    pub fn new(at: Pt, rot: f32, sx: f32) -> Xf {
        let (sin, cos) = rot.sin_cos();
        Xf { at, cos, sin, sx }
    }

    pub fn p(&self, q: Pt) -> Pt {
        let x = q[0] * self.sx;
        [
            self.at[0] + x * self.cos - q[1] * self.sin,
            self.at[1] + x * self.sin + q[1] * self.cos,
        ]
    }

    pub fn all(&self, pts: &[Pt]) -> Vec<Pt> {
        pts.iter().map(|&q| self.p(q)).collect()
    }
}

/// A quadratic from `a` through `c` to `b`, `n` steps, without `a`.
pub fn quad(a: Pt, c: Pt, b: Pt, n: usize) -> impl Iterator<Item = Pt> {
    (1..=n).map(move |k| {
        let t = k as f32 / n as f32;
        let u = 1.0 - t;
        [
            u * u * a[0] + 2.0 * u * t * c[0] + t * t * b[0],
            u * u * a[1] + 2.0 * u * t * c[1] + t * t * b[1],
        ]
    })
}

/// A cubic from `a` to `b`, `n` steps, without `a`.
pub fn cubic(a: Pt, c1: Pt, c2: Pt, b: Pt, n: usize) -> impl Iterator<Item = Pt> {
    (1..=n).map(move |k| {
        let t = k as f32 / n as f32;
        let u = 1.0 - t;
        let (w0, w1, w2, w3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
        [
            w0 * a[0] + w1 * c1[0] + w2 * c2[0] + w3 * b[0],
            w0 * a[1] + w1 * c1[1] + w2 * c2[1] + w3 * b[1],
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(poly: &[Pt]) -> f32 {
        (0..poly.len())
            .map(|i| {
                let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
                a[0] * b[1] - b[0] * a[1]
            })
            .sum::<f32>()
            .abs()
            / 2.0
    }

    #[test]
    fn a_long_outline_goes_down_in_narrow_overlapping_columns() {
        let mut sand: Vec<Pt> = (0..=100)
            .map(|k| [k as f32 * 8.0, 100.0 + (k as f32 * 0.4).sin() * 6.0])
            .collect();
        sand.extend([[800.0, 300.0], [0.0, 300.0]]);
        let cols = columns(&sand, 1.0);
        assert_eq!(cols.len(), 17);
        for col in &cols {
            let (lo, hi) = col.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
                (lo.min(p[0]), hi.max(p[0]))
            });
            assert!(hi - lo <= 49.0 + 1e-3);
        }
        // Covered whole, with only the 16 one-point seams painted twice.
        let painted: f32 = cols.iter().map(|c| area(c)).sum();
        let extra = painted - area(&sand);
        assert!(extra > 0.0 && extra < 16.0 * 200.0 + 1.0, "{extra}");
        assert_eq!(columns(&sand[..5], 1.0).len(), 1);
    }

    #[test]
    fn polygons_cut_to_a_line_and_to_a_convex_shape() {
        let square = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let top = clip_half(&square, 4.0);
        assert!(top.iter().all(|p| p[1] <= 4.0) && top.len() == 4);
        let mut wound = square;
        wound.reverse();
        for by in [square, wound] {
            let cut = clip_convex(&[[5.0, 5.0], [20.0, 5.0], [20.0, 20.0], [5.0, 20.0]], &by);
            assert!(
                cut.iter()
                    .all(|p| (5.0..=10.0).contains(&p[0]) && (5.0..=10.0).contains(&p[1]))
            );
            assert_eq!(cut.len(), 4);
        }
    }
}
