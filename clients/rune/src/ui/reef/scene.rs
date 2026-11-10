//! The reef's scenery, painter by painter as the prototype draws it: water,
//! far ridges and their school, jellies, rays, sky, kelp, sand, buoys,
//! coral, bubbles, snow and the vignette.

use std::f32::consts::{PI, TAU};

use super::Label;
use super::pen::{Pen, Xf, clip_convex, quad};
use crate::reef::{Jelly, Kelp, Mood, Pal, Pt, Reef, Rgb, Zone, mix};

const WHITE: Rgb = [255.0, 255.0, 255.0];
const BLACK: Rgb = [0.0, 0.0, 0.0];

fn grad3(a: Rgb, b: Rgb, c: Rgb, t: f32, mid: f32) -> Rgb {
    if t < mid {
        mix(a, b, t / mid)
    } else {
        mix(b, c, (t - mid) / (1.0 - mid))
    }
}

/// x from `a` to `b` by `step`, both ends in.
fn xs(a: f32, b: f32, step: f32) -> impl Iterator<Item = f32> {
    (0..)
        .map(move |k| a + step * k as f32)
        .take_while(move |x| *x <= b)
}

pub fn water(pen: &mut Pen, r: &Reef, pal: &Pal) {
    let (h, sy) = (r.h, r.surface_y);
    let (y0, y1) = (sy - 10.0, h + 50.0);
    pen.vgrad(-20.0, y0, r.w + 40.0, y1, 48, |k| {
        let t = ((y0 + (y1 - y0) * k - sy) / (h - sy)).clamp(0.0, 1.0);
        (grad3(pal.w0, pal.w1, pal.w2, t, 0.45), 1.0)
    });
}

/// The far ridges, the background school between them, and the jellies.
pub fn far(pen: &mut Pen, r: &Reef, pal: &Pal) {
    let (w, h) = (r.w, r.h);
    let ridge = |pts: &[Pt]| {
        let mut poly = vec![[-80.0, h + 40.0]];
        poly.extend_from_slice(pts);
        poly.push([w + 80.0, h + 40.0]);
        poly
    };
    pen.fill(&ridge(&r.far_b), pal.far2, 0.55);
    let c = mix(pal.far2, [210.0, 240.0, 250.0], 0.35 * (1.0 - r.nt));
    for a in &r.amb {
        let s = 5.0 * r.s;
        let dir = if a.vx < 0.0 { -1.0 } else { 1.0 };
        let body = pen.ellipse([a.x, a.y], s, s * 0.38, 0.0, 0.0, TAU);
        pen.fill(&body, c, 0.5);
        let tail = [
            [a.x - dir * s * 0.8, a.y],
            [a.x - dir * s * 1.6, a.y - s * 0.5],
            [a.x - dir * s * 1.6, a.y + s * 0.5],
        ];
        pen.fill(&tail, c, 0.5);
    }
    pen.fill(&ridge(&r.far_a), mix(pal.far, pal.w2, 0.15), 0.9);
    for j in &r.jellies {
        jelly(pen, r, j);
    }
}

fn jelly(pen: &mut Pen, r: &Reef, j: &Jelly) {
    let (t, nt) = (r.t, r.nt);
    let s = j.s * r.s;
    let y = j.by + (t * 0.18 + j.ph).sin() * 50.0 * r.s;
    let c = ((t * 1.7 + j.ph).sin() + 1.0) / 2.0;
    let (bw, bh) = (20.0 * s * (1.0 + c * 0.12), 15.0 * s * (1.0 - c * 0.15));
    let o = [j.x, y];
    if nt > 0.02 {
        pen.glow(o, 60.0 * s, [255.0, 120.0, 220.0], 0.35 * nt);
    }
    let tentacle = [255.0, 190.0 + nt * 40.0, 240.0];
    for k in 0..6 {
        let x0 = (k as f32 / 5.0 - 0.5) * bw * 1.4;
        let pts: Vec<Pt> = (0..=8)
            .map(|i| {
                let fi = i as f32;
                let wob = if i == 0 {
                    0.0
                } else {
                    (t * 2.0 + j.ph + k as f32 + fi * 0.7).sin() * 3.0 * s * fi / 8.0
                };
                [o[0] + x0 + wob, o[1] + fi * 7.0 * s]
            })
            .collect();
        pen.stroke(&pts, false, 1.1 * r.s, tentacle, 0.32 + nt * 0.35);
    }
    let mut bell = pen.ellipse(o, bw, bh, 0.0, PI, TAU);
    bell.extend(quad(
        [o[0] + bw, o[1]],
        [o[0], o[1] + bh * 0.35],
        [o[0] - bw, o[1]],
        8,
    ));
    pen.fill(&bell, [255.0, 140.0, 210.0], 0.12 + nt * 0.2);
    for (k, (rx, ry)) in [(0.7, 0.75), (0.38, 0.45)].into_iter().enumerate() {
        let inner = pen.ellipse([o[0], o[1] - bh * 0.35], bw * rx, bh * ry, 0.0, 0.0, TAU);
        let col = mix(
            [255.0, 140.0, 210.0],
            [255.0, 236.0, 250.0],
            0.5 + 0.5 * k as f32,
        );
        pen.fill(
            &clip_convex(&inner, &bell),
            col,
            (0.25 + nt * 0.15) * (1.0 + k as f32 * 0.4),
        );
    }
}

pub fn rays(pen: &mut Pen, r: &Reef) {
    if r.nt > 0.97 {
        return;
    }
    let (t, s, sy) = (r.t, r.s, r.surface_y);
    let depth = r.floor_y - sy + 40.0 * s;
    const N: usize = 6;
    for ray in &r.rays {
        let a = (0.09 + 0.06 * (t * ray.sp + ray.ph).sin()) * (1.0 - r.nt);
        let x = ray.x + (t * 0.13 + ray.ph).sin() * 20.0 * s;
        let edge = |v: f32| {
            let y = sy + depth * v;
            let half = ray.w / 2.0 + (ray.w * 1.3 - ray.w / 2.0) * v;
            (x + ray.skew * v - half, x + ray.skew * v + half, y)
        };
        for k in 0..N {
            let (l0, r0, y0) = edge(k as f32 / N as f32);
            let (l1, r1, y1) = edge((k + 1) as f32 / N as f32);
            let alpha = a * (1.0 - (k as f32 + 0.5) / N as f32);
            pen.fill(
                &[[l0, y0], [r0, y0], [r1, y1], [l1, y1]],
                [235.0, 255.0, 250.0],
                alpha,
            );
        }
    }
}

pub fn sky(pen: &mut Pen, r: &Reef, pal: &Pal) {
    let (w, h, s, t, nt, sy) = (r.w, r.h, r.s, r.t, r.nt, r.surface_y);
    // The bright underside of the surface; the sky covers its top half.
    let top = 0.32 - nt * 0.2;
    for k in 0..5 {
        let y = sy - 4.0 + k as f32 * 8.0 * s;
        pen.rect(
            -20.0,
            y,
            w + 40.0,
            8.0 * s,
            [220.0, 255.0, 255.0],
            top * (1.0 - (k as f32 + 0.5) / 5.0),
        );
    }
    let wave: Vec<Pt> = xs(-20.0, w + 20.0, 10.0)
        .map(|x| [x, r.wave_y(x)])
        .collect();
    let mut poly = wave.clone();
    poly.extend([[w + 20.0, -h], [-20.0, -h]]);
    pen.vgrad_poly(&poly, -h * 0.2, sy, 10, |k| mix(pal.sky0, pal.sky1, k));
    if nt > 0.02 {
        for star in &r.stars {
            let a = nt * (0.45 + 0.45 * (t * 2.0 + star.tw).sin());
            pen.rect(
                star.at[0],
                star.at[1],
                star.r,
                star.r,
                [230.0, 240.0, 255.0],
                a,
            );
        }
    }
    let (o, sr) = ([w * 0.8, sy * 0.42], 22.0 * s);
    let halo = mix([255.0, 248.0, 220.0], [200.0, 220.0, 255.0], nt);
    pen.glow_cut(o, sr * 5.0, halo, 0.55, sy - 5.0 * s);
    pen.disc(
        o,
        sr * (1.0 - nt * 0.35),
        mix([255.0, 252.0, 236.0], [226.0, 234.0, 255.0], nt),
        1.0,
    );
    pen.stroke(&wave, false, 1.4 * s, WHITE, 0.6 - nt * 0.3);
    for rip in &r.ripples {
        let k = rip.a / 1.8;
        let rx = (8.0 + rip.a * 46.0) * s;
        let (c, a) = if rip.amber {
            ([255.0, 190.0, 90.0], (1.0 - k) * 0.85)
        } else {
            (WHITE, (1.0 - k) * 0.5)
        };
        let mut ring = pen.ellipse([rip.x, r.wave_y(rip.x) + 1.0], rx, rx * 0.16, 0.0, 0.0, TAU);
        ring.pop();
        pen.stroke(&ring, true, 1.3 * s, c, a);
    }
}

pub fn kelp(pen: &mut Pen, r: &Reef, pal: &Pal, k: &Kelp, alpha: f32) {
    const N: usize = 16;
    let (t, s) = (r.t, r.s);
    let base = r.sand_y(k.x) + 6.0 * s;
    let pts: Vec<Pt> = (0..=N)
        .map(|i| {
            let f = i as f32 / N as f32;
            let x = k.x
                + (t * 0.85 + k.seed + i as f32 * 0.3).sin() * f.powf(1.3) * k.h * 0.09
                + (t * 0.3 + k.seed).sin() * f * 10.0 * s;
            [x, base - k.h * f]
        })
        .collect();
    let c = if k.back {
        mix(pal.kelp, pal.w1, 0.5)
    } else {
        pal.kelp
    };
    let stem = mix(c, BLACK, 0.25);
    for i in 0..N {
        pen.line(
            pts[i],
            pts[i + 1],
            k.w * (1.0 - i as f32 / N as f32 * 0.6),
            stem,
            alpha,
        );
    }
    for i in 2..N {
        let ([x, y], [px, py]) = (pts[i], pts[i - 1]);
        let side = if i % 2 == 1 { 1.0 } else { -1.0 };
        let a = (y - py).atan2(x - px) + side * (1.0 + (t * 1.2 + i as f32 + k.seed).sin() * 0.15);
        let len = 17.0 * s * (1.0 - i as f32 / N as f32 * 0.4);
        let centre = [x + a.cos() * len * 0.5, y + a.sin() * len * 0.5];
        let leaf = pen.ellipse(centre, len * 0.55, len * 0.2, a, 0.0, TAU);
        pen.fill(&leaf, c, alpha * 0.92);
    }
}

pub fn sand(pen: &mut Pen, r: &Reef, pal: &Pal) {
    let (w, h, s, nt, fy) = (r.w, r.h, r.s, r.nt, r.floor_y);
    let top = fy - 12.0 * s;
    let mut poly: Vec<Pt> = xs(-20.0, w + 20.0, 8.0).map(|x| [x, r.sand_y(x)]).collect();
    poly.extend([[w + 20.0, h + 40.0], [-20.0, h + 40.0]]);
    let c0 = mix(pal.sand0, pal.w1, 0.18);
    pen.vgrad_poly(&poly, top, h, 8, |k| mix(c0, pal.sand1, k));
    for row in 1..6 {
        let yy = fy + row as f32 * (h - fy) / 6.0;
        let line: Vec<Pt> = xs(-20.0, w + 20.0, 12.0)
            .map(|x| {
                [
                    x,
                    (yy + (x * 0.05 + row as f32 * 1.7).sin() * 2.0 * s).max(r.sand_y(x) + 1.0),
                ]
            })
            .collect();
        pen.stroke(&line, false, s, [255.0, 250.0, 230.0], 0.12 - nt * 0.08);
    }
    for p in &r.pebbles {
        let at = [p.x, r.sand_y(p.x) + 6.0 * s + p.dy * (h - fy) * 0.8];
        let pebble = pen.ellipse(at, p.rx, p.rx * 0.6, 0.0, 0.0, TAU);
        pen.fill(&pebble, mix(pal.sand1, [60.0, 50.0, 40.0], p.sh * 0.4), 0.7);
    }
}

/// A host's buoy and rope; the lamp blinks amber while an agent there
/// needs you. Returns its label.
pub fn buoy(pen: &mut Pen, r: &Reef, z: &Zone, zi: usize) -> Label {
    let (s, t, nt) = (r.s, r.t, r.nt);
    let (bx, by) = (z.cx, r.wave_y(z.cx));
    let tilt = (r.wave_y(bx + 8.0) - r.wave_y(bx - 8.0)).atan2(16.0);
    let waiting = r.waiting_in(zi);
    let from = [bx, by + 8.0 * s];
    let rope: Vec<Pt> = std::iter::once(from)
        .chain(quad(
            from,
            [
                bx + (t * 0.6 + zi as f32).sin() * 14.0 * s,
                (by + z.home_y) / 2.0,
            ],
            [bx + 4.0 * s, z.home_y],
            12,
        ))
        .collect();
    for seg in rope.windows(2) {
        let k = ((seg[0][1] - by) / (z.home_y - by)).clamp(0.0, 1.0);
        pen.line(
            seg[0],
            seg[1],
            1.2 * s,
            [230.0, 240.0, 240.0],
            0.35 * (1.0 - k),
        );
    }
    let xf = Xf::new([bx, by], tilt, 1.0);
    let half = |pen: &Pen, ry: f32, from: f32| {
        xf.all(&pen.ellipse([0.0, 0.0], 11.0 * s, ry * s, 0.0, from, from + PI))
    };
    let top = half(pen, 9.0, PI);
    pen.fill(&top, mix([200.0, 52.0, 48.0], [90.0, 24.0, 30.0], nt), 1.0);
    let band = xf.all(&[
        [-11.0 * s, -3.5 * s],
        [11.0 * s, -3.5 * s],
        [11.0 * s, 0.0],
        [-11.0 * s, 0.0],
    ]);
    pen.fill(
        &band,
        mix([238.0, 238.0, 230.0], [110.0, 116.0, 124.0], nt),
        1.0,
    );
    let bottom = half(pen, 6.0, 0.0);
    pen.fill(
        &bottom,
        mix([150.0, 40.0, 40.0], [40.0, 14.0, 20.0], nt),
        0.85,
    );
    pen.line(
        xf.p([0.0, -9.0 * s]),
        xf.p([0.0, -17.0 * s]),
        1.5 * s,
        mix([60.0; 3], [20.0; 3], nt),
        1.0,
    );
    let lit = if waiting > 0 {
        0.5 + 0.5 * (t * 6.0).sin()
    } else {
        0.85
    };
    let lc = if waiting > 0 {
        Mood::Waiting.color()
    } else {
        Mood::Done.color()
    };
    let lamp = xf.p([0.0, -19.0 * s]);
    pen.disc(lamp, 3.0 * s, lc, 0.4 + lit * 0.6);
    pen.glow(lamp, (10.0 + 14.0 * nt) * s, lc, lit * (0.35 + 0.5 * nt));
    // A machine this window doesn't list the agents of shows no count.
    let text = if z.known {
        format!("{}  {}", z.host, r.fish_in(zi))
    } else {
        z.host.clone()
    };
    Label {
        at: [bx, by - 36.0 * s],
        text,
        badge: (waiting > 0).then_some(waiting),
        amber: false,
        host: Some(z.host.clone()),
    }
}

fn tint(r: &Reef, pal: &Pal, c: Rgb) -> Rgb {
    mix(c, pal.w1, 0.14 + r.nt * 0.55)
}

/// A dome lit from its upper left, the prototype's radial gradient as
/// lighter ellipses stepping toward the light.
fn dome(pen: &mut Pen, at: Pt, rx: f32, ry: f32, light: Pt, outer: Rgb, inner: Rgb) {
    let shape = pen.ellipse(at, rx, ry, 0.0, PI, TAU);
    pen.fill(&shape, outer, 1.0);
    for k in 1..4 {
        let f = k as f32 / 4.0;
        let c = [
            at[0] + (light[0] - at[0]) * f,
            at[1] + (light[1] - at[1]) * f,
        ];
        let e = pen.ellipse(
            c,
            rx * (1.0 - f * 0.6),
            ry * (1.0 - f * 0.55),
            0.0,
            0.0,
            TAU,
        );
        pen.fill(&clip_convex(&e, &shape), mix(outer, inner, f), 1.0);
    }
}

pub fn reef_back(pen: &mut Pen, r: &Reef, pal: &Pal, z: &Zone) {
    let (s, t, nt) = (r.s, r.t, r.nt);
    for k in &z.rocks {
        let by = r.sand_y(k.x) + k.ry * 0.35;
        let inner = mix([96.0, 112.0, 120.0], pal.w1, 0.3 + nt * 0.5);
        let outer = mix([30.0, 44.0, 54.0], pal.w2, 0.3 + nt * 0.5);
        dome(
            pen,
            [k.x, by],
            k.rx,
            k.ry,
            [k.x - k.rx * 0.3, by - k.ry * 0.6],
            outer,
            inner,
        );
    }
    for f in &z.fans {
        let (bx, by) = (f.x, r.sand_y(f.x) + 2.0 * s);
        let sway = (t * 0.9 + f.seed).sin() * 5.0 * s;
        let c = tint(r, pal, f.col);
        let ends: Vec<Pt> = (0..9)
            .map(|k| {
                let a = -PI * 0.86 + k as f32 / 8.0 * PI * 0.72;
                let len = f.h * (0.78 + 0.22 * (k as f32 * 1.7 + f.seed).sin());
                [a.cos() * len * 0.8, a.sin() * len]
            })
            .collect();
        let at = |e: Pt, fr: f32| [bx + e[0] * fr + sway * fr * fr, by + e[1] * fr];
        let mut web = vec![[bx, by]];
        web.extend(ends.iter().map(|&e| at(e, 1.0)));
        pen.fill(&web, c, 0.28);
        for &e in &ends {
            let mid = at(e, 0.55);
            let rib: Vec<Pt> = std::iter::once([bx, by])
                .chain(quad(
                    [bx, by],
                    [mid[0] + e[0] * 0.04, mid[1]],
                    at(e, 1.0),
                    8,
                ))
                .collect();
            pen.stroke(&rib, false, 1.6 * s, c, 0.85);
        }
        for fr in [0.38, 0.62, 0.84] {
            let cross: Vec<Pt> = ends.iter().map(|&e| at(e, fr)).collect();
            pen.stroke(&cross, false, 1.6 * s, c, 0.85);
        }
    }
    for c in &z.corals {
        let (bx, by) = (c.x, r.sand_y(c.x) + 3.0 * s);
        let sway = (t * 1.05 + c.seed).sin() * 4.0 * s;
        let sw = |y: f32| sway * (-y / c.hmax).max(0.0).powf(1.5);
        let max = (c.by_depth.len() - 1) as f32;
        for (d, segs) in c.by_depth.iter().enumerate() {
            let col = tint(
                r,
                pal,
                mix(mix(c.col, [40.0, 20.0, 40.0], 0.35), c.col, d as f32 / max),
            );
            for g in segs {
                pen.line(
                    [bx + g.a[0] + sw(g.a[1]), by + g.a[1]],
                    [bx + g.b[0] + sw(g.b[1]), by + g.b[1]],
                    g.w,
                    col,
                    1.0,
                );
            }
        }
        let tip = tint(r, pal, mix(c.col, WHITE, 0.35));
        for g in &c.tips {
            pen.disc([bx + g.b[0] + sw(g.b[1]), by + g.b[1]], g.w * 0.7, tip, 1.0);
        }
        if nt > 0.05 {
            for g in &c.tips {
                // Over, not the prototype's additive light: packed tips would
                // pile into blobs at full strength.
                let a = 0.33 * nt * (0.7 + 0.3 * (t * 2.0 + g.b[0]).sin());
                pen.glow([bx + g.b[0] + sw(g.b[1]), by + g.b[1]], 7.0 * s, c.col, a);
            }
        }
    }
}

pub fn reef_front(pen: &mut Pen, r: &Reef, pal: &Pal, z: &Zone) {
    let (s, t, nt) = (r.s, r.t, r.nt);
    for b in &z.brains {
        let by = r.sand_y(b.x) + 4.0 * s;
        let c = tint(r, pal, b.col);
        let (inner, outer) = (mix(c, WHITE, 0.25), mix(c, [20.0, 10.0, 30.0], 0.45));
        dome(
            pen,
            [b.x, by],
            b.r,
            b.r * 0.78,
            [b.x - b.r * 0.3, by - b.r * 0.6],
            outer,
            inner,
        );
        let groove = mix(c, [30.0, 10.0, 30.0], 0.5);
        for j in 0..5 {
            let yy = by - b.r * (0.12 + j as f32 * 0.15);
            let mut run: Vec<Pt> = Vec::new();
            for x in xs(b.x - b.r, b.x + b.r, 3.0) {
                let y = yy + (x * 0.35 + j as f32 * 2.0).sin() * 2.2 * s;
                let (u, v) = ((x - b.x) / b.r, (y - by) / (b.r * 0.78));
                if u * u + v * v <= 1.0 && y <= by {
                    run.push([x, y]);
                } else if !run.is_empty() {
                    pen.stroke(&std::mem::take(&mut run), false, 1.2 * s, groove, 0.55);
                }
            }
            pen.stroke(&run, false, 1.2 * s, groove, 0.55);
        }
    }
    for a in &z.anemones {
        let by = r.sand_y(a.x) + 3.0 * s;
        let top = by - a.r * 0.9;
        let c = tint(r, pal, a.col);
        let body = pen.ellipse(
            [a.x, top + a.r * 0.45],
            a.r * 0.6,
            a.r * 0.55,
            0.0,
            0.0,
            TAU,
        );
        pen.fill(&body, mix(c, [40.0, 20.0, 30.0], 0.4), 1.0);
        let mut tips = Vec::with_capacity(13);
        for k in 0..13 {
            let u = k as f32 / 12.0 - 0.5;
            let x0 = a.x + u * a.r * 1.1;
            let ang = -PI / 2.0 + u * 2.3;
            let sw = (t * 1.7 + k as f32 * 0.55 + a.seed).sin() * 0.35;
            let tip = [
                x0 + (ang + sw).cos() * a.len,
                top + (ang + sw).sin() * a.len,
            ];
            let ctrl = [
                x0 + ang.cos() * a.len * 0.55,
                top + ang.sin() * a.len * 0.55,
            ];
            let arm: Vec<Pt> = std::iter::once([x0, top])
                .chain(quad([x0, top], ctrl, tip, 6))
                .collect();
            pen.stroke(&arm, false, 3.2 * s, c, 1.0);
            tips.push(tip);
        }
        for &tip in &tips {
            pen.disc(tip, 2.0 * s, mix(c, WHITE, 0.45), 1.0);
        }
        if nt > 0.05 {
            for &tip in &tips {
                pen.glow(tip, 6.0 * s, a.col, 0.3 * nt);
            }
        }
    }
}
