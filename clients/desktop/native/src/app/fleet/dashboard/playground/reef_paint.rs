//! The reef map: each machine a reef, each agent a fish (the reef prototype,
//! ported). The simulation steps on the panel's clock; the painter draws it in
//! the prototype's order under the reef's own camera; names, buoy labels, the
//! readout and the click targets are elements on top. A click on a fish
//! selects it (the camera swims to it); a second click, or Return, opens that
//! agent as a card does; open water or Escape lets it go. "N need you" selects
//! each waiting fish in turn.

use std::collections::HashMap;
use std::f32::consts::{PI, TAU};
use std::sync::{Arc, Mutex, OnceLock};

use gpui::prelude::*;
use gpui::{
    AnyElement, Background, Bounds, BoxShadow, ContentMask, Context, Corners, FillOptions, Hsla,
    PathBuilder, PathStyle, Pixels, Point, RenderImage, Rgba, Window, canvas, div,
    linear_color_stop, linear_gradient, point, px, size,
};

use super::reef::{Fish, Mood, Pal, Pattern, Rgb, Rng, Sim, Zone, mix};
use super::stage;
use crate::app::desk::Desk;
use crate::app::fleet::Agent;
use crate::app::skin::Look;

mod coral;
mod draw;
mod fish;
mod floor;
mod scene;

pub(crate) use self::scene::reef_key;
pub(super) use self::scene::scene;
use self::scene::view;
use self::{coral::*, draw::*, fish::*, floor::*};

type P = (f32, f32);

const WHITE: Rgb = [255., 255., 255.];
const BLACK: Rgb = [0., 0., 0.];
const AMBER: Rgb = [255., 181., 71.];

#[allow(clippy::too_many_arguments)]
fn rgba(c: Rgb, a: f32) -> Hsla {
    Rgba {
        r: (c[0] / 255.).clamp(0., 1.),
        g: (c[1] / 255.).clamp(0., 1.),
        b: (c[2] / 255.).clamp(0., 1.),
        a: a.clamp(0., 1.),
    }
    .into()
}

/// A gradient from `a` to `b` down the shape (`angle` 180), or turned.
fn grad(angle: f32, a: Hsla, at_a: f32, b: Hsla, at_b: f32) -> Background {
    linear_gradient(
        angle,
        linear_color_stop(a, at_a),
        linear_color_stop(b, at_b),
    )
}

/// An outline built like a canvas path: lines and curves, flattened.
#[derive(Default, Clone)]
struct Shape(Vec<P>);

impl Shape {
    fn at(x: f32, y: f32) -> Shape {
        Shape(vec![(x, y)])
    }

    fn line(mut self, x: f32, y: f32) -> Shape {
        self.0.push((x, y));
        self
    }

    fn quad(mut self, cx: f32, cy: f32, x: f32, y: f32) -> Shape {
        let (x0, y0) = *self.0.last().unwrap_or(&(x, y));
        for i in 1..=10 {
            let t = i as f32 / 10.;
            let u = 1. - t;
            self.0.push((
                u * u * x0 + 2. * u * t * cx + t * t * x,
                u * u * y0 + 2. * u * t * cy + t * t * y,
            ));
        }
        self
    }

    #[allow(clippy::too_many_arguments)]
    fn cubic(mut self, ax: f32, ay: f32, bx: f32, by: f32, x: f32, y: f32) -> Shape {
        let (x0, y0) = *self.0.last().unwrap_or(&(x, y));
        for i in 1..=14 {
            let t = i as f32 / 14.;
            let u = 1. - t;
            let (k0, k1, k2, k3) = (u * u * u, 3. * u * u * t, 3. * u * t * t, t * t * t);
            self.0.push((
                k0 * x0 + k1 * ax + k2 * bx + k3 * x,
                k0 * y0 + k1 * ay + k2 * by + k3 * y,
            ));
        }
        self
    }
}

/// Points round an ellipse turned by `rot`, from angle `a0` to `a1`.
#[allow(clippy::too_many_arguments)]
fn arc(cx: f32, cy: f32, rx: f32, ry: f32, rot: f32, a0: f32, a1: f32, n: usize) -> Vec<P> {
    let (sin, cos) = rot.sin_cos();
    (0..=n)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / n as f32;
            let (x, y) = (a.cos() * rx, a.sin() * ry);
            (cx + x * cos - y * sin, cy + x * sin + y * cos)
        })
        .collect()
}

fn circle(cx: f32, cy: f32, r: f32) -> Vec<P> {
    arc(cx, cy, r, r, 0., 0., TAU, 16)
}

/// A round-capped segment as one polygon.
fn capsule(a: P, b: P, w: f32) -> Vec<P> {
    let ang = (b.1 - a.1).atan2(b.0 - a.0);
    let r = w / 2.;
    let mut pts = arc(b.0, b.1, r, r, 0., ang - PI / 2., ang + PI / 2., 5);
    pts.extend(arc(a.0, a.1, r, r, 0., ang + PI / 2., ang + PI * 1.5, 5));
    pts
}

/// Which side of edge `a`→`b` the point is on, positive inside a polygon
/// wound the way `sign` says.
fn side(p: P, a: P, b: P) -> f32 {
    (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
}

fn winding(poly: &[P]) -> f32 {
    poly.iter()
        .zip(poly.iter().cycle().skip(1))
        .map(|(a, b)| a.0 * b.1 - b.0 * a.1)
        .sum::<f32>()
        .signum()
}

/// `subject` clipped to the convex polygon `clip` (Sutherland–Hodgman).
fn clip_convex(subject: &[P], clip: &[P]) -> Vec<P> {
    let sign = winding(clip);
    let cross = |p: P, q: P, a: P, b: P| {
        let (r, s) = ((q.0 - p.0, q.1 - p.1), (b.0 - a.0, b.1 - a.1));
        let den = r.0 * s.1 - r.1 * s.0;
        let t = if den.abs() < 1e-9 {
            0.
        } else {
            ((a.0 - p.0) * s.1 - (a.1 - p.1) * s.0) / den
        };
        (p.0 + r.0 * t, p.1 + r.1 * t)
    };
    let mut out = subject.to_vec();
    for i in 0..clip.len() {
        let (a, b) = (clip[i], clip[(i + 1) % clip.len()]);
        let input = std::mem::take(&mut out);
        for j in 0..input.len() {
            let (p, q) = (input[j], input[(j + 1) % input.len()]);
            match (side(p, a, b) * sign >= 0., side(q, a, b) * sign >= 0.) {
                (true, true) => out.push(q),
                (true, false) => out.push(cross(p, q, a, b)),
                (false, true) => {
                    out.push(cross(p, q, a, b));
                    out.push(q);
                }
                (false, false) => {}
            }
        }
    }
    out
}

/// The runs of `line` that fall inside the convex `clip`.
fn runs_inside(line: &[P], clip: &[P]) -> Vec<Vec<P>> {
    let sign = winding(clip);
    let contains =
        |p: P| (0..clip.len()).all(|i| side(p, clip[i], clip[(i + 1) % clip.len()]) * sign >= 0.);
    let mut runs = vec![Vec::new()];
    for &p in line {
        if contains(p) {
            if let Some(run) = runs.last_mut() {
                run.push(p);
            }
        } else if runs.last().is_some_and(|run| !run.is_empty()) {
            runs.push(Vec::new());
        }
    }
    runs.retain(|run| run.len() > 1);
    runs
}

/// Paints scene points through the current camera and local transform.
struct Pen<'w> {
    win: &'w mut Window,
    m: [f32; 6],
}

impl Pen<'_> {
    fn xy(&self, (x, y): P) -> P {
        let m = &self.m;
        (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
    }

    fn pt(&self, p: P) -> Point<Pixels> {
        let (x, y) = self.xy(p);
        point(px(x), px(y))
    }

    /// The transform's uniform scale, for line widths and glows.
    fn k(&self) -> f32 {
        (self.m[0] * self.m[3] - self.m[1] * self.m[2]).abs().sqrt()
    }

    fn translate(&mut self, x: f32, y: f32) {
        let m = &mut self.m;
        m[4] += m[0] * x + m[2] * y;
        m[5] += m[1] * x + m[3] * y;
    }

    fn rotate(&mut self, a: f32) {
        let (s, c) = a.sin_cos();
        let m = self.m;
        self.m[0] = m[0] * c + m[2] * s;
        self.m[1] = m[1] * c + m[3] * s;
        self.m[2] = -m[0] * s + m[2] * c;
        self.m[3] = -m[1] * s + m[3] * c;
    }

    fn scale(&mut self, sx: f32, sy: f32) {
        self.m[0] *= sx;
        self.m[1] *= sx;
        self.m[2] *= sy;
        self.m[3] *= sy;
    }

    fn fill(&mut self, pts: &[P], bg: impl Into<Background>) {
        self.fill_many(std::slice::from_ref(&pts.to_vec()), bg);
    }

    /// Several outlines as one path; overlaps stay filled.
    fn fill_many(&mut self, polys: &[Vec<P>], bg: impl Into<Background>) {
        let mut path = PathBuilder::fill().with_style(PathStyle::Fill(FillOptions::non_zero()));
        let mut any = false;
        for poly in polys.iter().filter(|poly| poly.len() > 2) {
            let pts = poly.iter().map(|&p| self.pt(p)).collect::<Vec<_>>();
            path.add_polygon(&pts, true);
            any = true;
        }
        if any && let Ok(path) = path.build() {
            self.win.paint_path(path, bg);
        }
    }

    fn stroke(&mut self, pts: &[P], closed: bool, width: f32, bg: impl Into<Background>) {
        self.stroke_many(std::slice::from_ref(&pts.to_vec()), closed, width, bg);
    }

    fn stroke_many(
        &mut self,
        lines: &[Vec<P>],
        closed: bool,
        width: f32,
        bg: impl Into<Background>,
    ) {
        let mut path = PathBuilder::stroke(px((width * self.k()).max(0.5)));
        let mut any = false;
        for line in lines.iter().filter(|line| line.len() > 1) {
            let pts = line.iter().map(|&p| self.pt(p)).collect::<Vec<_>>();
            path.add_polygon(&pts, closed);
            any = true;
        }
        if any && let Ok(path) = path.build() {
            self.win.paint_path(path, bg);
        }
    }

    /// A soft round glow of radius `r`: the prototype's radial gradient dots.
    fn glow(&mut self, p: P, r: f32, c: Rgb, a: f32) {
        let (x, y) = self.xy(p);
        let r = r * self.k();
        if r < 0.5 || a < 0.004 {
            return;
        }
        let side = r * 0.5;
        let bounds = Bounds {
            origin: point(px(x - side / 2.), px(y - side / 2.)),
            size: size(px(side), px(side)),
        };
        self.win.paint_drop_shadows(
            bounds,
            Corners::all(px(side / 2.)),
            &[BoxShadow {
                color: rgba(c, (a * 1.7).min(1.)),
                offset: point(px(0.), px(0.)),
                blur_radius: px(r * 0.33),
                spread_radius: px(0.),
                inset: false,
            }],
        );
    }

    /// Paint inside `rect` (panel coordinates) only.
    fn clipped(&mut self, rect: Bounds<Pixels>, f: impl FnOnce(&mut Pen)) {
        let m = self.m;
        self.win
            .with_content_mask(Some(ContentMask { bounds: rect }), |win| {
                f(&mut Pen { win, m });
            });
    }
}

fn rect(pen: &Pen, a: P, b: P) -> Bounds<Pixels> {
    let (a, b) = (pen.xy(a), pen.xy(b));
    Bounds {
        origin: point(px(a.0.min(b.0)), px(a.1.min(b.1))),
        size: size(px((b.0 - a.0).abs()), px((b.1 - a.1).abs())),
    }
}
