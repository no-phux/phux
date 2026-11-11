//! The hive's still backdrop, the prototype's `paint`: the meadow's sky and
//! grass, the hive box's planks and the comb frames with every cell, by day
//! or by night. Painted once per layout in world points and stretched
//! through the camera, so a frame costs nothing for it.

use std::f32::consts::TAU;

use super::super::reef::pen::{Pen, clip_convex, cubic, quad};
use super::{glow_in, hex, hgrad};
use crate::hive::{Hive, Kind};
use crate::reef::{Pt, Rgb, Rng, mix};

/// The day (`n` false) or night backdrop as a shape spanning the scene.
pub fn paint(h: &Hive, n: bool) -> rune::Shape {
    let mut pen = Pen::new();
    meadow(&mut pen, h, n);
    hive_box(&mut pen, h, n);
    for i in 0..h.frames.len() {
        frame(&mut pen, h, i, n);
    }
    pen.into_shape().view_box(h.w, h.h)
}

fn pick(n: bool, night: Rgb, day: Rgb) -> Rgb {
    if n { night } else { day }
}

fn meadow(pen: &mut Pen, h: &Hive, n: bool) {
    let (w, hh, mw) = (h.mw + 40.0, h.h, h.mw);
    let stops = [
        pick(n, [6.0, 12.0, 34.0], [128.0, 198.0, 236.0]),
        pick(n, [18.0, 30.0, 62.0], [220.0, 238.0, 236.0]),
        pick(n, [12.0, 30.0, 30.0], [168.0, 214.0, 120.0]),
    ];
    pen.vgrad(0.0, 0.0, w, hh, 24, |k| {
        let c = if k < 0.6 {
            mix(stops[0], stops[1], k / 0.6)
        } else {
            mix(stops[1], stops[2], (k - 0.6) / 0.4)
        };
        (c, 1.0)
    });
    let r = &mut Rng::new(7);
    if n {
        for _ in 0..70 {
            let a = 0.3 + r.next() * 0.6;
            let (x, y) = (r.next() * mw, r.next() * hh * 0.7);
            pen.rect(x, y, 1.2, 1.2, [255.0, 255.0, 240.0], a);
        }
        let (m, mr) = ([mw * 0.55, h.top + 36.0], 15.0);
        pen.glow(m, mr * 5.0, [210.0, 225.0, 255.0], 0.35);
        pen.disc(m, mr, [238.0, 242.0, 255.0], 1.0);
        let crater = [160.0, 170.0, 200.0];
        pen.disc([m[0] - 4.0, m[1] - 3.0], 3.5, crater, 0.35);
        pen.disc([m[0] + 5.0, m[1] + 4.0], 2.5, crater, 0.35);
    } else {
        let sun = [mw * 0.3, 40.0];
        pen.glow(sun, 160.0, [255.0, 240.0, 180.0], 0.5);
        pen.glow(sun, 32.0, [255.0, 250.0, 214.0], 0.5);
    }
    let (lo, hi) = pick2(
        n,
        [[18.0, 52.0, 48.0], [30.0, 70.0, 66.0]],
        [[92.0, 160.0, 70.0], [150.0, 200.0, 96.0]],
    );
    for _ in 0..260 {
        let (x, y, bh) = (
            r.next() * (mw + 30.0),
            hh * (0.25 + r.next() * 0.8),
            8.0 + r.next() * 22.0,
        );
        let c = mix(lo, hi, r.next());
        let a = 0.5 + r.next() * 0.4;
        let (bx, cx) = ((r.next() - 0.5) * 8.0, (r.next() - 0.5) * 10.0);
        let base = [x, y];
        let pts: Vec<Pt> = std::iter::once(base)
            .chain(quad(base, [x + bx, y - bh * 0.6], [x + cx, y - bh], 5))
            .collect();
        pen.stroke(&pts, false, 1.2, c, a);
    }
}

fn pick2(n: bool, night: [Rgb; 2], day: [Rgb; 2]) -> (Rgb, Rgb) {
    let [a, b] = if n { night } else { day };
    (a, b)
}

fn hive_box(pen: &mut Pen, h: &Hive, n: bool) {
    let (w, hh, mw) = (h.w, h.h, h.mw);
    let (c0, c1) = pick2(
        n,
        [[34.0, 20.0, 10.0], [22.0, 13.0, 7.0]],
        [[110.0, 66.0, 32.0], [78.0, 46.0, 22.0]],
    );
    hgrad(pen, mw, 0.0, w - mw, hh, 16, |k| (mix(c0, c1, k), 1.0));
    let mut x = mw + 6.0;
    while x < w {
        pen.rect(x, 0.0, 2.0, hh, [0.0; 3], if n { 0.25 } else { 0.12 });
        pen.rect(x + 2.0, 0.0, 1.0, hh, [255.0, 220.0, 170.0], 0.05);
        x += 46.0;
    }
    // The meadow's edge throws a shadow into the box.
    hgrad(pen, mw - 4.0, 0.0, 30.0, hh, 6, |k| {
        let a = if k < 0.15 {
            k / 0.15
        } else {
            1.0 - (k - 0.15) / 0.85
        };
        ([0.0; 3], 0.5 * a)
    });
    pen.rect(
        mw,
        0.0,
        5.0,
        hh,
        pick(n, [70.0, 44.0, 22.0], [168.0, 112.0, 60.0]),
        1.0,
    );
}

fn frame(pen: &mut Pen, h: &Hive, i: usize, n: bool) {
    let f = &h.frames[i];
    let rad = h.r;
    let wax = pick(n, [96.0, 62.0, 24.0], [226.0, 176.0, 80.0]);
    let waxd = pick(n, [52.0, 32.0, 12.0], [176.0, 122.0, 44.0]);
    pen.rect(f.x + 5.0, f.y + 7.0, f.w, f.h, [0.0; 3], 0.45);
    let (w0, w1) = pick2(
        n,
        [[92.0, 62.0, 36.0], [64.0, 42.0, 24.0]],
        [[214.0, 166.0, 106.0], [176.0, 128.0, 74.0]],
    );
    pen.vgrad(f.x, f.y, f.w, f.y + f.h, 8, |k| (mix(w0, w1, k), 1.0));
    // Wood grain along the top bar.
    for k in 0..6 {
        let y = f.y + 3.0 + k as f32 * (f.bar - 6.0) / 5.0;
        let a = [f.x, y];
        let pts: Vec<Pt> = std::iter::once(a)
            .chain(cubic(
                a,
                [f.x + f.w * 0.3, y + 2.0],
                [f.x + f.w * 0.6, y - 2.0],
                [f.x + f.w, y + 1.0],
                12,
            ))
            .collect();
        pen.stroke(
            &pts,
            false,
            1.0,
            [80.0, 40.0, 10.0],
            if n { 0.35 } else { 0.22 },
        );
    }
    pen.rect(f.ix, f.iy, f.iw, f.ih, waxd, 1.0);
    let ri = rad * 0.8;
    for c in &f.cells {
        pen.fill(&hex(c.at, rad * 0.98), wax, 1.0);
        match c.kind {
            Kind::Brood => {
                let (inner, outer) = pick2(
                    n,
                    [[150.0, 120.0, 80.0], [104.0, 76.0, 40.0]],
                    [[252.0, 230.0, 178.0], [214.0, 170.0, 100.0]],
                );
                lit_cell(pen, c.at, ri, outer, inner, [-0.3, -0.35]);
            }
            Kind::Honey => honey(pen, c.at, ri, c.lvl, waxd, n),
            Kind::Empty => {
                let (inner, outer) = pick2(
                    n,
                    [[70.0, 44.0, 16.0], [104.0, 70.0, 30.0]],
                    [[196.0, 134.0, 52.0], [238.0, 194.0, 108.0]],
                );
                lit_cell(pen, c.at, ri, outer, inner, [0.0, 0.0]);
            }
        }
    }
    if n {
        // Honey glows from within at night.
        let comb = rect(f.ix, f.iy, f.iw, f.ih);
        for c in f.cells.iter().filter(|c| c.kind == Kind::Honey) {
            glow_in(pen, c.at, rad * 2.2, [255.0, 150.0, 30.0], 0.12, &comb, 3);
        }
    }
    // Inner shadow along the comb's top and left edges.
    let sh = 10.0;
    pen.vgrad(f.ix, f.iy, f.iw, f.iy + sh, 4, |k| {
        ([40.0, 20.0, 0.0], 0.45 * (1.0 - k))
    });
    hgrad(pen, f.ix, f.iy, sh, f.ih, 4, |k| {
        ([40.0, 20.0, 0.0], 0.45 * (1.0 - k))
    });
    let edge = rect(f.x + 0.75, f.y + 0.75, f.w - 1.5, f.h - 1.5);
    pen.stroke(&edge, true, 1.5, [40.0, 20.0, 4.0], 0.6);
}

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> Vec<Pt> {
    vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]]
}

/// A cell lit as a radial gradient from `outer` at its rim to `inner`
/// toward `toward` (a fraction of its radius): three nested hexes.
pub fn lit_cell(pen: &mut Pen, at: Pt, ri: f32, outer: Rgb, inner: Rgb, toward: Pt) {
    pen.fill(&hex(at, ri), outer, 1.0);
    for (k, sc) in [(0.5, 0.66), (1.0, 0.33)] {
        let c = [
            at[0] + toward[0] * ri * (1.0 - sc),
            at[1] + toward[1] * ri * (1.0 - sc),
        ];
        pen.fill(&hex(c, ri * sc), mix(outer, inner, k), 1.0);
    }
}

/// A honey cell: dark wax, honey to its level shaded down, a glint.
fn honey(pen: &mut Pen, at: Pt, ri: f32, lvl: f32, waxd: Rgb, n: bool) {
    let cell = hex(at, ri);
    pen.fill(&cell, waxd, 1.0);
    let (top, bottom) = pick2(
        n,
        [[200.0, 116.0, 20.0], [130.0, 60.0, 8.0]],
        [[255.0, 196.0, 60.0], [222.0, 132.0, 18.0]],
    );
    fill_level(pen, &cell, at, ri, lvl, top, bottom, 1.0);
    let glint = pen.ellipse(
        [at[0] - ri * 0.3, at[1] - ri * 0.25],
        ri * 0.22,
        ri * 0.12,
        -0.5,
        0.0,
        TAU,
    );
    pen.fill(&glint, [255.0, 250.0, 220.0], 0.55);
}

/// Honey in `cell` up to `lvl` of its height, `top` shading to `bottom`.
#[allow(clippy::too_many_arguments)]
pub fn fill_level(
    pen: &mut Pen,
    cell: &[Pt],
    at: Pt,
    ri: f32,
    lvl: f32,
    top: Rgb,
    bottom: Rgb,
    a: f32,
) {
    let (y0, y1) = (at[1] + ri - ri * 2.0 * lvl, at[1] + ri);
    let mid = (y0 + y1) / 2.0;
    let (upper, lower) = (mix(top, bottom, 0.25), mix(top, bottom, 0.75));
    for (from, to, c) in [(y0, mid + 0.5, upper), (mid, y1, lower)] {
        let band = clip_convex(&rect(at[0] - ri, from, ri * 2.0, to - from), cell);
        pen.fill(&band, c, a);
    }
}
