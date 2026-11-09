//! The grounds, the prototype's `render`, drawn once per layout by day and
//! by night: grass, the lake, the grandstand and its crowd, the circuit with
//! its run-off, kerbs and rubber, the pit lane and its garages, parc fermé,
//! tyre walls, trees and floodlights. Translucent lines over one colour are
//! mixed into it first, as a translucent stroke blends twice at each joint.

use std::f32::consts::{PI, TAU};

use crate::pitlane::{MINT, PARC, PitLane, SPACING, at};
use crate::reef::{Pt, Rgb, Rng, mix};
use crate::ui::reef::pen::{Pen, Xf};

pub struct Pal {
    grass: Rgb,
    grass2: Rgb,
    run: Rgb,
    asph: Rgb,
    asph2: Rgb,
    line: Rgb,
    stand: Rgb,
    roof: Rgb,
    conc: Rgb,
    floor: Rgb,
    pub wall: Rgb,
    tree: Rgb,
    tree2: Rgb,
    water: Rgb,
    fan: f32,
}

const DAY: Pal = Pal {
    grass: [110.0, 172.0, 92.0],
    grass2: [98.0, 160.0, 82.0],
    run: [214.0, 206.0, 186.0],
    asph: [72.0, 76.0, 84.0],
    asph2: [84.0, 88.0, 96.0],
    line: [246.0, 248.0, 250.0],
    stand: [150.0, 158.0, 172.0],
    roof: [206.0, 212.0, 222.0],
    conc: [196.0, 200.0, 206.0],
    floor: [168.0, 172.0, 180.0],
    wall: [96.0, 102.0, 114.0],
    tree: [64.0, 128.0, 70.0],
    tree2: [84.0, 150.0, 84.0],
    water: [96.0, 170.0, 214.0],
    fan: 1.0,
};
const NIGHT: Pal = Pal {
    grass: [20.0, 44.0, 36.0],
    grass2: [18.0, 40.0, 33.0],
    run: [44.0, 46.0, 52.0],
    asph: [26.0, 28.0, 34.0],
    asph2: [34.0, 36.0, 44.0],
    line: [214.0, 222.0, 230.0],
    stand: [18.0, 22.0, 32.0],
    roof: [12.0, 15.0, 22.0],
    conc: [40.0, 44.0, 52.0],
    floor: [58.0, 62.0, 72.0],
    wall: [22.0, 26.0, 34.0],
    tree: [12.0, 34.0, 28.0],
    tree2: [16.0, 44.0, 34.0],
    water: [14.0, 34.0, 58.0],
    fan: 0.42,
};
/// The crowd's shirts.
const FANS: [Rgb; 8] = [
    [255.0, 118.0, 58.0],
    [44.0, 102.0, 232.0],
    [242.0, 196.0, 50.0],
    [150.0, 98.0, 242.0],
    [112.0, 190.0, 186.0],
    [236.0, 238.0, 242.0],
    [212.0, 62.0, 74.0],
    [38.0, 40.0, 50.0],
];
const BLACK: Rgb = [0.0, 0.0, 0.0];

pub fn pal(night: bool) -> &'static Pal {
    if night { &NIGHT } else { &DAY }
}

fn rect(pen: &mut Pen, r: [f32; 4], c: Rgb, a: f32) {
    pen.rect(r[0], r[1], r[2], r[3], c, a);
}

/// The outline of `r`, `w` wide.
fn outline(pen: &mut Pen, r: [f32; 4], w: f32, c: Rgb) {
    let [x, y, rw, rh] = r;
    pen.stroke(
        &[[x, y], [x + rw, y], [x + rw, y + rh], [x, y + rh]],
        true,
        w,
        c,
        1.0,
    );
}

/// The circuit's point `i` moved `o` out to its left.
fn off(trk: &[Pt], i: usize, o: f32) -> (Pt, f32) {
    let (q, ang, _) = at(trk, i as f32 * SPACING, true);
    ([q[0] - ang.sin() * o, q[1] + ang.cos() * o], ang)
}

/// Dashes `on` long with `gap` between along the polyline `pts`.
fn dashes(pts: &[Pt], on: f32, gap: f32) -> Vec<Vec<Pt>> {
    let mut out = Vec::new();
    let (mut cur, mut gone, mut drawing) = (vec![pts[0]], 0.0, true);
    for w in pts.windows(2) {
        let d = (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]);
        gone += d;
        if drawing {
            cur.push(w[1]);
        }
        if gone >= if drawing { on } else { gap } {
            gone = 0.0;
            if drawing {
                out.push(std::mem::take(&mut cur));
            } else {
                cur = vec![w[1]];
            }
            drawing = !drawing;
        }
    }
    if drawing && cur.len() > 1 {
        out.push(cur);
    }
    out
}

pub fn paint(r: &PitLane, night: bool) -> rune::Shape {
    let pal = pal(night);
    let (k, w, h) = (r.k, r.w, r.h);
    let mut pen = Pen::new();
    pen.rect(0.0, 0.0, w, h, pal.grass, 1.0);
    for i in 0..30 {
        let u = i as f32 / 30.0;
        rect(
            &mut pen,
            r.rect_s(u, -0.5, u + 0.5 / 30.0, 1.5),
            pal.grass2,
            1.0,
        );
    }
    // The lake.
    let lake = r.to_s(0.79, 0.7);
    let lr = 0.1 * r.lp.min(r.sp);
    let mut shore = pen.ellipse(lake, lr * 1.2, lr * 0.85, 0.4, 0.0, TAU);
    shore.pop();
    pen.fill(&shore, pal.water, 1.0);
    pen.stroke(&shore, true, 2.0, mix(pal.water, pal.line, 0.25), 1.0);
    stand(&mut pen, r, pal, night);
    // Run-off, kerbs where the circuit bends, asphalt.
    let tw = 30.0 * k;
    pen.stroke(&r.trk, true, tw + 26.0 * k, pal.run, 1.0);
    for side in [-1.0, 1.0] {
        for i in 0..r.trk.len() {
            if r.slow[i] > 0.86 {
                continue;
            }
            let (c, ang) = off(&r.trk, i, side * (tw / 2.0 + 1.5 * k));
            let (d, n) = (
                [ang.cos() * SPACING * 0.6, ang.sin() * SPACING * 0.6],
                [-ang.sin() * 2.0 * k, ang.cos() * 2.0 * k],
            );
            let col = match ((i / 3) % 2 == 1, night) {
                (true, true) => [255.0, 90.0, 106.0],
                (true, false) => [226.0, 59.0, 59.0],
                (false, true) => [255.0, 246.0, 240.0],
                (false, false) => [255.0, 255.0, 255.0],
            };
            let quad = [
                [c[0] - d[0] - n[0], c[1] - d[1] - n[1]],
                [c[0] + d[0] - n[0], c[1] + d[1] - n[1]],
                [c[0] + d[0] + n[0], c[1] + d[1] + n[1]],
                [c[0] - d[0] + n[0], c[1] - d[1] + n[1]],
            ];
            pen.fill(&quad, col, 1.0);
        }
    }
    pen.stroke(&r.trk, true, tw, pal.asph, 1.0);
    pen.stroke(&r.trk, true, tw * 0.35, pal.asph2, 1.0);
    for side in [-1.0, 1.0] {
        let edge: Vec<Pt> = (0..r.trk.len())
            .map(|i| off(&r.trk, i, side * (tw / 2.0 - 2.0 * k)).0)
            .collect();
        pen.stroke(&edge, true, 1.0, mix(pal.asph, pal.line, 0.8), 1.0);
    }
    // Rubber and skid marks through the corners.
    for i in (0..r.trk.len()).step_by(2) {
        if r.slow[i] > 0.7 {
            continue;
        }
        for o in [-4.0, 4.0] {
            let (a, ang) = off(&r.trk, i, o * k);
            let b = [
                a[0] + ang.cos() * SPACING * 2.0,
                a[1] + ang.sin() * SPACING * 2.0,
            ];
            pen.line(a, b, 1.4 * k, [10.0, 10.0, 14.0], 0.18);
        }
    }
    // Start/finish chequers.
    let xf = Xf::new(
        r.to_s(0.5, 0.1),
        if r.portrait { PI / 2.0 } else { 0.0 },
        1.0,
    );
    let cell = tw / 6.0;
    for i in 0..6 {
        for j in 0..2 {
            let (x, y) = (j as f32 * cell - cell, -tw / 2.0 + i as f32 * cell);
            let sq = xf.all(&[[x, y], [x + cell, y], [x + cell, y + cell], [x, y + cell]]);
            let c = if (i + j) % 2 == 1 {
                [17.0; 3]
            } else {
                [255.0; 3]
            };
            pen.fill(&sq, c, 1.0);
        }
    }
    // Pit lane, pit wall, apron.
    rect(&mut pen, r.rect_s(0.12, 0.185, 0.88, 0.25), pal.conc, 1.0);
    pen.stroke(&r.pit, false, 20.0 * k, pal.asph, 1.0);
    for dash in dashes(&r.pit, 6.0, 6.0) {
        pen.stroke(&dash, false, 1.0, mix(pal.asph, pal.line, 0.7), 1.0);
    }
    rect(&mut pen, r.rect_s(0.13, 0.145, 0.87, 0.152), pal.wall, 1.0);
    garages(&mut pen, r, pal, night);
    // Parc fermé.
    let [u0, u1, v0, v1] = PARC;
    let pf = r.rect_s(u0, v0, u1, v1);
    rect(&mut pen, pf, pal.floor, 1.0);
    let cell = 6.0;
    let mut x = 0.0;
    while x < pf[2] {
        let mut y = 0.0;
        while y < pf[3] {
            if ((x + y) / cell) as i64 % 2 == 0 {
                pen.rect(
                    pf[0] + x,
                    pf[1] + y,
                    cell.min(pf[2] - x),
                    cell.min(pf[3] - y),
                    BLACK,
                    0.08,
                );
            }
            y += cell;
        }
        x += cell;
    }
    let [x, y, rw, rh] = pf;
    let border = [[x, y], [x + rw, y], [x + rw, y + rh], [x, y + rh], [x, y]];
    let mint = mix(pal.floor, MINT, if night { 0.8 } else { 0.9 });
    for dash in dashes(&resample_open(&border), 4.0, 3.0) {
        pen.stroke(&dash, false, 2.0, mint, 1.0);
    }
    // Tyre walls on the outside of slow corners.
    let tyre = if night {
        [27.0, 29.0, 36.0]
    } else {
        [44.0, 47.0, 56.0]
    };
    for i in (0..r.trk.len()).step_by(3) {
        if r.slow[i] > 0.6 {
            continue;
        }
        for side in [-1.0, 1.0] {
            let (at, _) = off(&r.trk, i, side * (tw / 2.0 + 16.0 * k));
            pen.disc(at, 2.2 * k, tyre, 1.0);
        }
    }
    for t in &r.trees {
        let [x, y] = t.at;
        pen.disc([x + t.r * 0.3, y + t.r * 0.3], t.r, BLACK, 0.2);
        pen.disc(t.at, t.r, pal.tree, 1.0);
        pen.disc([x - t.r * 0.25, y - t.r * 0.25], t.r * 0.55, pal.tree2, 1.0);
    }
    let lamp = if night {
        [232.0, 238.0, 248.0]
    } else {
        [89.0, 96.0, 108.0]
    };
    for p in &r.poles {
        pen.rect(p[0] - 2.5, p[1] - 2.5, 5.0, 5.0, lamp, 1.0);
    }
    if night {
        for p in &r.poles {
            light(&mut pen, *p, 70.0 * k + 30.0);
        }
        // Garages lit from the back.
        for g in &r.garages {
            const BANDS: usize = 8;
            let band = 0.17 / BANDS as f32;
            for b in 0..BANDS {
                let (va, vb) = (0.25 + band * b as f32, 0.25 + band * (b + 1) as f32);
                let a = 0.05 + 0.17 * (b as f32 + 0.5) / BANDS as f32;
                rect(
                    &mut pen,
                    r.rect_s(g.u0, va, g.u1, vb),
                    [255.0, 240.0, 210.0],
                    a,
                );
            }
        }
    }
    pen.into_shape()
}

/// A floodlight's pool of light, fading out: bands that each cover only
/// their own ring with the alpha of the stacked rings inside, each ring two
/// half-annuli, so twenty pools paint their area once and no big fan
/// triangle costs the GPU its box (and only two seams show per ring).
fn light(pen: &mut Pen, c: Pt, r: f32) {
    const BANDS: usize = 8;
    const STEPS: usize = 16;
    let one = 0.32 * 0.9 / BANDS as f32;
    let arc = |rr: f32, from: f32| -> Vec<Pt> {
        (0..=STEPS)
            .map(|s| {
                let a = from + s as f32 / STEPS as f32 * PI;
                [c[0] + rr * a.cos(), c[1] + rr * a.sin()]
            })
            .collect()
    };
    for b in 0..BANDS {
        let (r0, r1) = (
            r * b as f32 / BANDS as f32,
            r * (b + 1) as f32 / BANDS as f32,
        );
        let alpha = 1.0 - (1.0 - one).powi((BANDS - b) as i32);
        for from in [0.0, PI] {
            let mut half = arc(r1, from);
            if b == 0 {
                half.push(c);
            } else {
                half.extend(arc(r0, from).into_iter().rev());
            }
            pen.fill(&half, [200.0, 215.0, 255.0], alpha);
        }
    }
}

/// A polyline resampled every point, for dashes along a few long sides.
fn resample_open(pts: &[Pt]) -> Vec<Pt> {
    let mut out = vec![pts[0]];
    for w in pts.windows(2) {
        let n = (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]).ceil().max(1.0) as usize;
        out.extend((1..=n).map(|k| {
            let t = k as f32 / n as f32;
            [
                w[0][0] + (w[1][0] - w[0][0]) * t,
                w[0][1] + (w[1][1] - w[0][1]) * t,
            ]
        }));
    }
    out
}

/// The grandstand along the outside of the main straight: the crowd, then
/// the roof and its shadow.
fn stand(pen: &mut Pen, r: &PitLane, pal: &Pal, night: bool) {
    let gs = r.rect_s(0.04, -0.17, 0.96, 0.045);
    let rng = &mut Rng::new(5 + night as u32);
    rect(pen, gs, pal.stand, 1.0);
    let (along, across) = if r.portrait {
        (gs[3], gs[2])
    } else {
        (gs[2], gs[3])
    };
    for i in 0..(across / 6.0).floor() as usize {
        let mut j = 0;
        while (j as f32) < along / 6.0 {
            if j % 24 != 12 {
                let (fi, ji) = (i as f32, j as f32);
                let (x, y) = if r.portrait {
                    (gs[0] + gs[2] - 4.0 - fi * 6.0, gs[1] + ji * 6.0 + 3.0)
                } else {
                    (gs[0] + ji * 6.0 + 3.0, gs[1] + gs[3] - 4.0 - fi * 6.0)
                };
                let f = FANS[((rng.next() * 8.0) as usize).min(7)];
                if night && rng.next() < 0.03 {
                    pen.rect(x - 1.5, y - 1.5, 3.0, 3.0, [220.0, 235.0, 255.0], 0.9);
                } else {
                    let c = mix(f, pal.stand, 0.4).map(|v| v * pal.fan);
                    pen.rect(x - 1.5, y - 1.5, 3.0, 3.0, c, 1.0);
                }
            }
            j += 1;
        }
    }
    let rf = r.rect_s(0.04, -0.17, 0.96, -0.1);
    rect(pen, [rf[0] + 3.0, rf[1] + 4.0, rf[2], rf[3]], BLACK, 0.22);
    rect(pen, rf, pal.roof, 1.0);
    let rib = mix(pal.roof, pal.stand, 0.6);
    let mut u = 0.04;
    while u < 0.96 {
        pen.line(r.to_s(u, -0.17), r.to_s(u, -0.1), 1.0, rib, 1.0);
        u += 0.03;
    }
}

/// Each machine's garage: floor, back wall in its team's stripe, a box per
/// agent with its marked spot on the apron.
fn garages(pen: &mut Pen, r: &PitLane, pal: &Pal, night: bool) {
    for g in &r.garages {
        let f = r.rect_s(g.u0, 0.25, g.u1, 0.42);
        rect(pen, f, pal.floor, 1.0);
        rect(pen, r.rect_s(g.u0, 0.395, g.u1, 0.42), pal.wall, 1.0);
        let strip = mix(pal.wall, g.c, if night { 0.95 } else { 0.85 });
        rect(pen, r.rect_s(g.u0, 0.385, g.u1, 0.395), strip, 1.0);
        let half = (g.u1 - g.u0) / g.bays.len().max(1) as f32 / 2.0;
        let mark = mix(pal.conc, g.c, 0.75);
        for (j, b) in g.bays.iter().enumerate() {
            if j > 0 {
                pen.line(
                    r.to_s(b.u - half, 0.25),
                    r.to_s(b.u - half, 0.42),
                    2.0,
                    pal.wall,
                    1.0,
                );
            }
            outline(
                pen,
                r.rect_s(b.u - half * 0.8, 0.205, b.u + half * 0.8, 0.248),
                1.2,
                mark,
            );
        }
        outline(pen, f, 3.0, pal.wall);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dashes_alternate_along_a_line() {
        let line: Vec<[f32; 2]> = (0..=30).map(|x| [x as f32, 0.0]).collect();
        let d = super::dashes(&line, 6.0, 6.0);
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].first(), Some(&[0.0, 0.0]));
        assert_eq!(d[0].last(), Some(&[6.0, 0.0]));
        assert_eq!(d[1].first(), Some(&[12.0, 0.0]));
    }
}
