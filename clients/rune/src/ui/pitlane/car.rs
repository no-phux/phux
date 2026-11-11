//! A car, as the prototype's `car`: its night light trail and headlight,
//! its mood's glow, speed lines, then the car in its harness's livery with
//! its tier's flourishes (wing in its fin colour, stripes, gold trim, a
//! trophy); an idle one under its cover. Then the pit crew, the chequered
//! flag, confetti, fireworks and the helicopter.

use std::f32::consts::{PI, TAU};

use crate::pitlane::{AMBER, Car, GOLD, MINT, Mode, PitLane};
use crate::reef::{Mood, Pt, Rgb, mix};
use crate::ui::reef::pen::{Pen, Xf, quad};

const WHITE: Rgb = [255.0, 255.0, 255.0];
const BLACK: Rgb = [0.0, 0.0, 0.0];
const TYRE: Rgb = [22.0, 23.0, 28.0];

/// `c` toward its grey by `q`.
fn grey(c: Rgb, q: f32) -> Rgb {
    let g = c[0] * 0.3 + c[1] * 0.59 + c[2] * 0.11;
    mix(c, [g; 3], q)
}

/// A rounded rectangle in a shape's own frame.
fn rrect(xf: &Xf, x: f32, y: f32, w: f32, h: f32, rad: f32) -> Vec<Pt> {
    let rad = rad.min(w / 2.0).min(h / 2.0);
    let corners = [
        (x + w - rad, y + rad, -PI / 2.0),
        (x + w - rad, y + h - rad, 0.0),
        (x + rad, y + h - rad, PI / 2.0),
        (x + rad, y + rad, PI),
    ];
    let mut pts = Vec::with_capacity(16);
    for (cx, cy, a0) in corners {
        for s in 0..4 {
            let a = a0 + s as f32 / 3.0 * PI / 2.0;
            pts.push(xf.p([cx + rad * a.cos(), cy + rad * a.sin()]));
        }
    }
    pts
}

fn box_at(xf: &Xf, x: f32, y: f32, w: f32, h: f32) -> Vec<Pt> {
    xf.all(&[[x, y], [x + w, y], [x + w, y + h], [x, y + h]])
}

fn oval(pen: &Pen, c: Pt, rx: f32, ry: f32, rot: f32) -> Vec<Pt> {
    let mut pts = pen.ellipse(c, rx, ry, rot, 0.0, TAU);
    pts.pop();
    pts
}

pub fn car(pen: &mut Pen, r: &PitLane, c: &Car) {
    let (t, n, k) = (r.t, r.nt, r.k);
    let u = c.u(k);
    let kit = c.who.species;
    let idle = c.who.mood == Mood::Idle;
    let q = if idle { 0.65 } else { 0.0 };
    let (body, fin, belly) = (grey(kit.body, q), grey(kit.fin, q), grey(kit.belly, q));
    let (lc, wc) = (24.0 * u, 10.0 * u);
    let o = [c.x, c.y];
    let dir = [c.h.cos(), c.h.sin()];
    // Night light trails and the headlight's beam.
    if n > 0.05 && c.mode == Mode::Track && c.trail.len() > 2 {
        let len = c.trail.len() as f32;
        for (i, seg) in c.trail.windows(2).enumerate() {
            let a = n * 0.5 * (i + 1) as f32 / len;
            pen.line(seg[0], seg[1], 2.4 * u, [255.0, 40.0, 60.0], a);
        }
        let head = [c.x + dir[0] * lc * 0.5, c.y + dir[1] * lc * 0.5];
        let beam = 46.0 * u;
        for s in 0..4 {
            let mut fan = vec![head];
            fan.extend(pen.ellipse(
                head,
                beam * (1.0 - s as f32 / 4.0),
                beam * (1.0 - s as f32 / 4.0),
                0.0,
                c.h - 0.45,
                c.h + 0.45,
            ));
            pen.fill(&fan, [255.0, 250.0, 225.0], 0.38 * n * 0.9 / 4.0);
        }
    }
    match c.who.mood {
        Mood::Waiting => {
            let pulse = 0.5 + 0.5 * (t * 5.0).sin();
            pen.glow(o, 30.0 * u, AMBER, 0.42 + 0.2 * pulse);
            if c.landed {
                let p = (t * 0.8 + c.seed) % 1.0;
                pen.ring(o, 12.0 * u + p * 22.0 * u, 2.0, AMBER, 1.0 - p);
            }
        }
        Mood::Done if c.landed => pen.glow(o, 22.0 * u, MINT, 0.5),
        _ => {}
    }
    if c.mode == Mode::Track && c.speed > 170.0 * k && n < 0.6 {
        for off in [-0.35f32, 0.0, 0.35] {
            let b = [
                c.x - dir[0] * lc * 0.6 - dir[1] * off * wc * 1.4,
                c.y - dir[1] * lc * 0.6 + dir[0] * off * wc * 1.4,
            ];
            let len = c.speed / (250.0 * k) * 14.0 * u * (1.0 + off.abs());
            pen.line(
                b,
                [b[0] - dir[0] * len, b[1] - dir[1] * len],
                1.0,
                WHITE,
                0.45 * (1.0 - n),
            );
        }
    }
    let xf = Xf::new(o, c.h, 1.0);
    let shadow = rrect(&xf, -lc / 2.0 + 2.0, -wc / 2.0 + 2.0, lc, wc, 3.0 * u);
    pen.fill(&shadow, BLACK, 0.28 - 0.1 * n);
    if idle {
        let cover =
            mix([170.0, 176.0, 186.0], grey(kit.body, 0.5), 0.3).map(|v| v * (1.0 - 0.35 * n));
        pen.fill(
            &rrect(
                &xf,
                -lc / 2.0 - 1.0,
                -wc / 2.0 - 1.0,
                lc + 2.0,
                wc + 2.0,
                4.0 * u,
            ),
            cover,
            1.0,
        );
        for x in [-0.25, 0.05, 0.3] {
            let a = [x * lc, -wc / 2.0];
            let fold: Vec<Pt> = std::iter::once(a)
                .chain(quad(a, [x * lc + 2.0 * u, 0.0], [x * lc - u, wc / 2.0], 6))
                .collect();
            pen.stroke(&xf.all(&fold), false, 1.0, mix(cover, BLACK, 0.18), 1.0);
        }
        pen.fill(
            &rrect(&xf, -4.0 * u, -2.0 * u, 8.0 * u, 4.0 * u, 1.5 * u),
            body,
            1.0,
        );
        return;
    }
    for [x, y] in [[0.3, 1.0], [0.3, -1.0], [-0.32, 1.0], [-0.32, -1.0]] {
        pen.fill(
            &box_at(
                &xf,
                x * lc - 2.6 * u,
                y * wc / 2.0 - 1.8 * u,
                5.2 * u,
                3.6 * u,
            ),
            TYRE,
            1.0,
        );
    }
    let (wing, ww) = if c.tier >= 2 {
        (fin, 3.4)
    } else {
        ([42.0, 44.0, 51.0], 2.2)
    };
    pen.fill(
        &box_at(
            &xf,
            -lc / 2.0 - u,
            -wc / 2.0 - 0.4 * u,
            ww * u,
            wc + 0.8 * u,
        ),
        wing,
        1.0,
    );
    let hull = xf.all(&[
        [lc / 2.0, -1.2 * u],
        [lc * 0.15, -wc * 0.24],
        [lc * 0.02, -wc * 0.44],
        [-lc * 0.3, -wc * 0.42],
        [-lc / 2.0 + 1.5 * u, -wc * 0.22],
        [-lc / 2.0 + 1.5 * u, wc * 0.22],
        [-lc * 0.3, wc * 0.42],
        [lc * 0.02, wc * 0.44],
        [lc * 0.15, wc * 0.24],
        [lc / 2.0, 1.2 * u],
    ]);
    pen.fill(&hull, body, 1.0);
    if c.tier >= 3 {
        pen.stroke(&hull, true, 0.9 * u, GOLD, 1.0);
    }
    if c.tier >= 1 {
        for y in [-1.3, 1.3] {
            pen.line(
                xf.p([lc / 2.0 - u, y * 0.55 * u]),
                xf.p([-lc / 2.0 + 2.0 * u, y * u]),
                1.1 * u,
                fin,
                1.0,
            );
        }
    }
    pen.fill(
        &box_at(
            &xf,
            lc / 2.0 - 2.4 * u,
            -wc / 2.0 - 0.4 * u,
            2.2 * u,
            wc + 0.8 * u,
        ),
        fin,
        1.0,
    );
    let cockpit = oval(pen, xf.p([-lc * 0.02, 0.0]), 3.4 * u, 2.0 * u, c.h);
    pen.fill(&cockpit, [18.0, 19.0, 24.0], 1.0);
    pen.disc(xf.p([-lc * 0.03, 0.0]), 1.5 * u, belly, 1.0);
    // The number roundel; Rune can't turn text, so it goes without its number.
    let roundel = xf.p([-lc * 0.28, 0.0]);
    if c.tier >= 3 {
        pen.glow(roundel, 6.0 * u, WHITE, 0.8);
    }
    pen.disc(roundel, 2.6 * u, WHITE, 1.0);
    if n > 0.05 {
        pen.fill(
            &box_at(&xf, -lc / 2.0 - 1.4 * u, -u, 1.4 * u, 2.0 * u),
            [255.0, 60.0, 70.0],
            n,
        );
    }
    if c.tier >= 4 {
        trophy(pen, [c.x, c.y - 16.0 * u + (t * 3.0).sin() * 1.2], u);
    }
}

/// A legend's trophy, floating over it.
fn trophy(pen: &mut Pen, [x, y]: Pt, u: f32) {
    pen.glow([x, y], 10.0 * u, GOLD, 0.55);
    let mut cup = vec![[x - 3.4 * u, y - 3.0 * u], [x + 3.4 * u, y - 3.0 * u]];
    cup.extend(quad(
        cup[1],
        [x + 3.0 * u, y + 1.5 * u],
        [x + 0.8 * u, y + 1.8 * u],
        5,
    ));
    cup.extend([
        [x + 0.8 * u, y + 3.0 * u],
        [x + 2.0 * u, y + 3.8 * u],
        [x - 2.0 * u, y + 3.8 * u],
        [x - 0.8 * u, y + 3.0 * u],
        [x - 0.8 * u, y + 1.8 * u],
    ]);
    cup.extend(quad(
        [x - 0.8 * u, y + 1.8 * u],
        [x - 3.0 * u, y + 1.5 * u],
        [x - 3.4 * u, y - 3.0 * u],
        5,
    ));
    cup.pop();
    pen.fill(&cup, GOLD, 1.0);
}

/// The crew round a boxed car that needs you, the lollipop up ahead of its nose.
pub fn crew(pen: &mut Pen, r: &PitLane, c: &Car, team: Rgb) {
    let (t, u) = (r.t, r.k);
    let (lc, wc) = (24.0 * u, 10.0 * u);
    let xf = Xf::new([c.x, c.y], c.h, 1.0);
    for (i, [x, y]) in [[0.3, 1.0], [0.3, -1.0], [-0.32, 1.0], [-0.32, -1.0]]
        .into_iter()
        .enumerate()
    {
        let p = xf.p([
            x * lc + (t * 9.0 + i as f32).sin() * 0.8 * u,
            y * (wc / 2.0 + 4.0 * u),
        ]);
        pen.disc(p, 2.6 * u, team, 1.0);
        pen.disc(p, 1.4 * u, [244.0, 244.0, 246.0], 1.0);
    }
    let (lp, bp) = (
        xf.p([lc / 2.0 + 7.0 * u, 0.0]),
        xf.p([lc / 2.0 + 3.0 * u, 0.0]),
    );
    let pulse = 0.5 + 0.5 * (t * 6.0).sin();
    pen.disc(lp, 2.6 * u, team, 1.0);
    pen.line(lp, bp, 1.0, [221.0; 3], 1.0);
    let board = (3.6 + pulse) * u;
    pen.disc(bp, board, AMBER, 0.8 + 0.2 * pulse);
    pen.ring(bp, board, 0.8, [42.0, 22.0, 0.0], 1.0);
}

/// The chequered flag over a parked finished car, waving.
pub fn flag(pen: &mut Pen, r: &PitLane, c: &Car) {
    let (t, u) = (r.t, r.k);
    let (x, y, cell) = (c.x + 10.0 * u, c.y - 8.0 * u, 2.4 * u);
    pen.line(
        [x, y + 8.0 * u],
        [x, y - 8.0 * u],
        1.2,
        [232.0, 232.0, 234.0],
        1.0,
    );
    for i in 0..4 {
        let wave = (t * 6.0 - i as f32 * 0.9).sin() * 1.4 * u;
        for j in 0..3 {
            let col = if (i + j) % 2 == 1 { [17.0; 3] } else { WHITE };
            let (cx, cy) = (x + i as f32 * cell, y - 8.0 * u + j as f32 * cell + wave);
            pen.rect(cx, cy, cell + 0.3, cell + 0.3, col, 1.0);
        }
    }
}

/// The dashed ring that turns round the selected car.
pub fn ring(pen: &mut Pen, r: &PitLane, c: &Car) {
    let rad = c.r(r.k) + 7.0;
    let n = ((TAU * rad / 9.0) as usize).max(6);
    let step = TAU / n as f32;
    let turn = r.t * 20.0 / rad;
    for d in 0..n {
        let from = turn + d as f32 * step;
        let dash = pen.ellipse([c.x, c.y], rad, rad, 0.0, from, from + step * 5.0 / 9.0);
        pen.stroke(&dash, false, 2.0, WHITE, 1.0);
    }
}

/// Confetti, fireworks, and the helicopter circling over everything.
pub fn sky(pen: &mut Pen, r: &PitLane) {
    for b in &r.bits {
        let xf = Xf::new([b.x, b.y], b.rot, 1.0);
        pen.fill(&box_at(&xf, -2.0, -1.0, 4.0, 2.0), b.c, b.life.min(1.0));
    }
    for e in &r.embers {
        pen.rect(
            e.x - 1.6,
            e.y - 1.6,
            3.2,
            3.2,
            e.c,
            (e.life / 1.5).max(0.0) * r.nt,
        );
    }
    let (t, n) = (r.t, r.nt);
    let rr = r.w.min(r.h) * 0.28;
    let an = t * 0.12;
    let (x, y) = (
        r.w / 2.0 + an.cos() * rr * 1.4,
        r.h / 2.0 + an.sin() * rr * 0.8,
    );
    let h = an + PI / 2.0;
    let shade = Xf::new([x + 40.0, y + 50.0], h, 1.0);
    let sa = 0.18 * (1.0 - n);
    pen.fill(&oval(pen, shade.p([0.0, 0.0]), 14.0, 5.0, h), BLACK, sa);
    pen.fill(&box_at(&shade, -26.0, -1.5, 16.0, 3.0), BLACK, sa);
    let xf = Xf::new([x, y], h, 1.0);
    let dark = n > 0.5;
    let hull = if dark {
        [42.0, 49.0, 64.0]
    } else {
        [233.0, 237.0, 243.0]
    };
    pen.fill(&oval(pen, [x, y], 12.0, 5.0, h), hull, 1.0);
    pen.fill(&box_at(&xf, -24.0, -1.2, 14.0, 2.4), hull, 1.0);
    pen.fill(
        &oval(pen, xf.p([6.0, 0.0]), 4.0, 3.4, h),
        [90.0, 184.0, 255.0],
        1.0,
    );
    let blade = if dark {
        [200.0, 210.0, 230.0]
    } else {
        [30.0, 34.0, 44.0]
    };
    pen.ring([x, y], 17.0, 1.4, blade, 0.35);
    let spin = Xf::new([x, y], h + t * 40.0, 1.0);
    let ba = if dark { 0.5 } else { 0.55 };
    pen.line(spin.p([-17.0, 0.0]), spin.p([17.0, 0.0]), 1.6, blade, ba);
    pen.line(spin.p([0.0, -17.0]), spin.p([0.0, 17.0]), 1.6, blade, ba);
    if n > 0.1 && t % 1.0 < 0.15 {
        pen.disc([x, y], 2.5, [255.0, 60.0, 60.0], n);
    }
}
