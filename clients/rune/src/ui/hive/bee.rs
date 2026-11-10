//! A bee, as the prototype's `bee()`: shadow, beating wings, a striped
//! abdomen shaded from belly to body, the fuzzy thorax in the harness's fin
//! colour, head, eyes and antennae, with its tier's flourishes (antenna
//! tips, big eyes, the queen's longer abdomen and crown).

use std::f32::consts::TAU;

use super::super::reef::pen::{Pen, Xf, clip_convex, quad};
use crate::hive::{DARK, flourish};
use crate::reef::{Pt, Rgb, Species, mix};

const HEAD: Rgb = [38.0, 26.0, 18.0];
const STRIPE: Rgb = [28.0, 18.0, 10.0];
const WHITE: Rgb = [255.0, 255.0, 255.0];

/// How a bee is drawn this frame.
pub struct Look {
    pub flap: f32,
    /// Resting wings, no flutter: asleep.
    pub still: bool,
    /// Greyed toward stone: asleep.
    pub desat: f32,
    pub pollen: bool,
    pub tier: usize,
}

pub fn bee(pen: &mut Pen, at: Pt, ang: f32, l: f32, k: &Species, o: &Look) {
    let grey = [150.0, 146.0, 140.0];
    let body = mix(k.body, grey, o.desat);
    let fin = mix(k.fin, grey, o.desat);
    let belly = mix(k.belly, grey, o.desat);
    let fl = flourish(o.tier);
    let xf = Xf::new(at, ang, 1.0);
    let ell = |pen: &Pen, c: Pt, rx: f32, ry: f32, rot: f32| {
        let mut pts = pen.ellipse(c, rx, ry, rot, 0.0, TAU);
        pts.pop();
        pts
    };
    let shadow = ell(pen, [-l * 0.05, l * 0.18], l * 0.5, l * 0.2, 0.0);
    pen.fill(&xf.all(&shadow), [20.0, 10.0, 0.0], 0.25);
    // Wings, turned about their roots.
    let beat = if o.still { 0.0 } else { o.flap.sin() };
    for sd in [-1.0f32, 1.0] {
        let a = -sd * if o.still { 0.25 } else { 0.75 + 0.5 * beat };
        let wl = l * 0.6;
        let ww = l * if o.still {
            0.2
        } else {
            0.14 + 0.1 * beat.abs()
        };
        let c = [
            l * 0.08 - wl * 0.5 * a.cos(),
            sd * l * 0.1 - wl * 0.5 * a.sin(),
        ];
        let wing = xf.all(&ell(pen, c, wl * 0.5, ww, a));
        let tint = mix(WHITE, mix([200.0, 230.0, 255.0], fin, 0.35), 0.5);
        pen.fill(&wing, tint, if o.still { 0.45 } else { 0.36 });
        pen.stroke(&wing, true, 0.6, [60.0, 40.0, 30.0], 0.35);
    }
    // Abdomen: edge colour, then two steps toward the lit side.
    let (ax, arx, ary) = (-l * 0.28 * fl.abdomen, l * 0.34 * fl.abdomen, l * 0.22);
    let abdomen = ell(pen, [ax, 0.0], arx, ary, 0.0);
    let (lit, edge) = (mix(body, belly, 0.45), mix(body, DARK, 0.25));
    pen.fill(&xf.all(&abdomen), edge, 1.0);
    for (kk, sc) in [(0.5, 0.72), (0.9, 0.42)] {
        let c = [ax + arx * 0.2 * (1.0 - sc), -ary * 0.4 * (1.0 - sc)];
        let band = ell(pen, c, arx * sc, ary * sc, 0.0);
        pen.fill(&xf.all(&band), mix(edge, lit, kk), 1.0);
    }
    let cut = |x: f32, w: f32| {
        let rect = [[x, -ary], [x + w, -ary], [x + w, ary], [x, ary]];
        clip_convex(&rect, &abdomen)
    };
    for s in [0.12, -0.28, -0.66] {
        let band = cut(ax + arx * s - l * 0.04, l * 0.08);
        pen.fill(&xf.all(&band), STRIPE, 0.85);
    }
    pen.fill(&xf.all(&cut(ax - arx * 0.86, l * 0.07)), fin, 0.9);
    let sting = [
        [ax - arx * 0.95, -l * 0.04],
        [ax - arx - l * 0.12, 0.0],
        [ax - arx * 0.95, l * 0.04],
    ];
    pen.fill(&xf.all(&sting), STRIPE, 0.9);
    // Thorax with its fuzz, pollen baskets, head and eyes.
    let thorax = ell(pen, [l * 0.1, 0.0], l * 0.17, l * 0.17, 0.0);
    pen.fill(&xf.all(&thorax), mix(fin, DARK, 0.3), 1.0);
    let fuzz = mix(fin, WHITE, 0.3);
    for i in 0..10 {
        let (s, c) = (i as f32 * TAU / 10.0).sin_cos();
        let a = xf.p([l * 0.1 + c * l * 0.13, s * l * 0.13]);
        let b = xf.p([l * 0.1 + c * l * 0.2, s * l * 0.2]);
        pen.line(a, b, 0.8, fuzz, 0.6);
    }
    if o.pollen {
        for sd in [-1.0, 1.0] {
            pen.disc(
                xf.p([0.0, sd * l * 0.22]),
                l * 0.085,
                [255.0, 178.0, 30.0],
                1.0,
            );
        }
    }
    pen.disc(xf.p([l * 0.32, 0.0]), l * 0.12, HEAD, 1.0);
    let eye = mix(belly, [20.0, 20.0, 30.0], 0.6);
    for sd in [-1.0, 1.0] {
        let e = ell(
            pen,
            [l * 0.34, sd * l * 0.08],
            l * fl.eye,
            l * fl.eye * 0.7,
            0.0,
        );
        pen.fill(&xf.all(&e), eye, 1.0);
    }
    let tw = if o.still {
        0.0
    } else {
        (o.flap * 0.13).sin() * 0.15
    };
    for sd in [-1.0, 1.0] {
        let (a, c) = ([l * 0.4, sd * l * 0.05], [l * 0.55, sd * l * (0.12 + tw)]);
        let b = [l * 0.6, sd * l * (0.24 + tw)];
        let pts: Vec<Pt> = std::iter::once(a).chain(quad(a, c, b, 6)).collect();
        pen.stroke(&xf.all(&pts), false, (l * 0.035).max(0.8), HEAD, 1.0);
        if fl.tips {
            pen.disc(xf.p(b), l * 0.045, fin, 1.0);
        }
    }
    if fl.crown {
        // The queen's crown, upright on screen.
        let (cx, cy, w) = (at[0], at[1] - l * 0.48, l * 0.3);
        let crown = [
            [cx - w, cy + w * 0.5],
            [cx - w, cy - w * 0.2],
            [cx - w * 0.5, cy + w * 0.15],
            [cx, cy - w * 0.5],
            [cx + w * 0.5, cy + w * 0.15],
            [cx + w, cy - w * 0.2],
            [cx + w, cy + w * 0.5],
        ];
        pen.fill(&crown, [255.0, 211.0, 77.0], 1.0);
        pen.stroke(&crown, true, 0.8, [120.0, 70.0, 0.0], 0.8);
    }
}
