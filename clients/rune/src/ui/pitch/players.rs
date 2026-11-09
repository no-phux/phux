//! What moves on the pitch, painted as the prototype: players, balls,
//! pigeons, confetti and the stands' passing light. Rune shapes are flat,
//! so a radial glow is stacked discs and the crowd's wave is strips.

use std::f32::consts::{PI, TAU};

use crate::pitch::{
    AMBER, Ball, Boots, GOLD, HAIRS, MINT, Mood, Pitch, Player, Pt, Rgb, SKINS, grey, kit, lerp,
};

use super::super::reef::pen::Pen;

/// Where a player was drawn, for its label, ring and press target.
pub struct Spot {
    pub at: Pt,
    pub r: f32,
    /// The shirt number's centre and size.
    pub number: (Pt, f32, Rgb),
}

fn hex(v: u32) -> Rgb {
    [(v >> 16) as f32, ((v >> 8) & 255) as f32, (v & 255) as f32]
}

/// A rounded rectangle's outline.
fn rrect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Vec<Pt> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let corner = |c: Pt, a0: f32| {
        (0..=3).map(move |k| {
            let a = a0 + PI / 2.0 * k as f32 / 3.0;
            [c[0] + r * a.cos(), c[1] + r * a.sin()]
        })
    };
    corner([x + w - r, y + r], -PI / 2.0)
        .chain(corner([x + w - r, y + h - r], 0.0))
        .chain(corner([x + r, y + h - r], PI / 2.0))
        .chain(corner([x + r, y + r], PI))
        .collect()
}

fn oval(pen: &Pen, c: Pt, rx: f32, ry: f32, rot: f32) -> Vec<Pt> {
    let mut pts = pen.ellipse(c, rx, ry, rot, 0.0, TAU);
    pts.pop();
    pts
}

/// A player's shadow: the sun's by day, one per floodlight by night.
fn shadow(pen: &mut Pen, pitch: &Pitch, x: f32, y: f32, rx: f32, ry: f32) {
    let n = pitch.nt;
    if n < 0.98 {
        let pts = oval(pen, [x + rx * 0.5, y + ry * 0.4], rx, ry, 0.3);
        pen.fill(&pts, [10.0, 30.0, 10.0], 0.26 * (1.0 - n));
    }
    if n > 0.02 {
        for t in &pitch.towers {
            let (dx, dy) = (x - t[0], y - t[1]);
            let d = dx.hypot(dy).max(1.0);
            let o = rx * 1.1;
            let at = [x + dx / d * o, y + dy / d * o * 0.6];
            let pts = oval(pen, at, rx * 1.2, ry * 0.8, dy.atan2(dx));
            pen.fill(&pts, [0.0, 8.0, 6.0], 0.16 * n);
        }
    }
}

/// The prototype's `drawPlayer`: a footballer in its harness's kit, facing
/// its way, acting out its mood.
pub fn player(pen: &mut Pen, pitch: &Pitch, p: &Player, x: f32, y: f32) -> Spot {
    let (t, mood) = (pitch.t, p.who.mood);
    let u = pitch.k * (1.0 + 0.07 * p.tier as f32);
    let sp = p.who.species;
    let idle = mood == Mood::Idle;
    let q = if idle { 0.62 } else { 0.0 };
    let (shirt, sleeve, shorts) = (grey(sp.body, q), grey(sp.fin, q), grey(sp.belly, q));
    let skin = grey(SKINS[p.look % SKINS.len()], q * 0.6);
    let hair = HAIRS[(p.look * 7) % HAIRS.len()];
    let jump = if p.cel > 0.0 {
        (p.cel * 6.5).sin().abs() * 8.0 * u
    } else {
        0.0
    };
    let sit = p.sit;
    let kit = kit(p.tier);
    shadow(pen, pitch, x, y, 5.0 * u, 2.0 * u);
    if mood == Mood::Waiting {
        let pulse = 0.5 + 0.5 * (t * 5.0).sin();
        pen.glow(
            [x, y - 10.0 * u],
            28.0 * u,
            AMBER,
            0.34 + 0.18 * pulse + 0.12 * pitch.nt,
        );
        let k = (t * 0.8 + p.who.number as f32 * 0.13).fract();
        let r = 6.0 * u + k * 20.0 * u;
        let pts = oval(pen, [x, y], r, r * 0.42, 0.0);
        pen.stroke(&pts, true, 2.0, AMBER, 1.0 - k);
    } else if mood == Mood::Done {
        let n = 6;
        for i in 0..n {
            let f = 1.0 - i as f32 / n as f32;
            let pts = oval(pen, [x, y], 14.0 * u * f, 6.0 * u * f, 0.0);
            pen.fill(&pts, MINT, 0.55 * 0.9 / n as f32);
        }
    }
    if idle {
        pen.fade = 0.88;
    }
    let fs = if p.face < 0.0 { -1.0 } else { 1.0 } * (0.55 + 0.45 * p.face.abs());
    let o = [x, y - jump];
    let at = |q: Pt| [o[0] + q[0] * fs, o[1] + q[1]];
    let all = |pts: Vec<Pt>| pts.into_iter().map(at).collect::<Vec<Pt>>();
    let hy = lerp(-7.5 * u, -3.8 * u, sit);
    let top = hy - 10.5 * u;
    let sy = top + 1.6 * u;
    let hx = top - 3.3 * u;
    let boots = match kit.boots {
        Boots::Gold => GOLD,
        Boots::Sleeve => sleeve,
        Boots::Black => hex(0x24262c),
    };
    let arm = |pen: &mut Pen, side: f32, ang: f32, front: bool| {
        let (s, c) = ang.sin_cos();
        let sx = side * 3.3 * u;
        let along = |d: f32| at([sx + s * d * u, sy + c * d * u]);
        pen.line(at([sx, sy]), along(2.6), 2.2 * u, sleeve, 1.0);
        pen.line(along(2.4), along(6.0), 1.8 * u, skin, 1.0);
        if front && kit.armband {
            pen.line(along(1.2), along(2.3), 2.6 * u, hex(0xffd84a), 1.0);
        }
    };
    let sw = p.phase.sin() * 0.9 * p.run.min(1.0);
    let (mut back, mut front) = (-sw, sw);
    if p.cel > 0.0 {
        back = PI - 0.5 + (t * 14.0).sin() * 0.25;
        front = PI + 0.5 - (t * 14.0).sin() * 0.25;
    } else if mood == Mood::Waiting && p.arrived {
        front = PI - 0.2 + (t * 7.0).sin() * 0.4;
        back = 0.15;
    } else if sit > 0.5 {
        (back, front) = (0.5, 0.7);
    }
    arm(pen, -1.0, back, false);
    for side in [-1.0, 1.0] {
        let hip = side * 1.6 * u;
        if sit > 0.5 {
            let knee = [
                at([hip, hy]),
                at([hip + 4.5 * u, hy + 0.5 * u]),
                at([hip + 5.0 * u, 0.0]),
            ];
            pen.stroke(&knee, false, 2.2 * u, skin, 1.0);
            pen.line(
                at([hip + 5.0 * u, -0.4 * u]),
                at([hip + 6.4 * u, 0.0]),
                2.6 * u,
                boots,
                1.0,
            );
            continue;
        }
        let ang = side * sw * 0.8 + if p.cel > 0.0 { side * 0.35 } else { 0.0 };
        let (fx, fy) = (hip + ang.sin() * 7.0 * u, hy + ang.cos() * 7.4 * u);
        pen.line(at([hip, hy]), at([fx, fy]), 2.2 * u, skin, 1.0);
        pen.line(at([fx, fy]), at([fx + 1.4 * u, fy]), 2.8 * u, boots, 1.0);
    }
    pen.fill(
        &all(rrect(-3.6 * u, hy - 2.6 * u, 7.2 * u, 3.4 * u, u)),
        shorts,
        1.0,
    );
    let body = all(rrect(-4.0 * u, top, 8.0 * u, 8.6 * u, 2.2 * u));
    pen.fill(&body, shirt, 1.0);
    let band = [
        [-4.0 * u, top + 6.6 * u],
        [4.0 * u, top + 6.6 * u],
        [4.0 * u, top + 7.7 * u],
        [-4.0 * u, top + 7.7 * u],
    ];
    pen.fill(&band.map(at), sleeve, 1.0);
    if kit.trim {
        pen.stroke(&body, true, 0.8 * u, GOLD, 1.0);
    }
    arm(pen, 1.0, front, true);
    let head = pen.ellipse([0.0, hx], 3.3 * u, 3.3 * u, 0.0, 0.0, TAU);
    pen.fill(&all(head), skin, 1.0);
    let cap = pen.ellipse(
        [-0.4 * u, hx - 0.4 * u],
        3.4 * u,
        3.4 * u,
        0.0,
        PI * 0.95,
        PI * 2.05,
    );
    pen.fill(&all(cap), hair, 1.0);
    pen.fade = 1.0;
    let number = (
        [x, y - jump + top + 4.0 * u],
        (5.2 * u).max(5.0),
        grey(sp.belly, q),
    );
    if kit.star {
        let yy = y - jump + hx - 8.0 * u + (t * 3.0).sin() * 1.2 * u;
        let r = 3.4 * u;
        pen.glow([x, yy], r * 3.0, GOLD, 0.6);
        let star: Vec<Pt> = (0..10)
            .map(|i| {
                let rr = if i % 2 == 1 { r * 0.45 } else { r };
                let an = t * 1.5 + i as f32 * PI / 5.0;
                [x + an.cos() * rr, yy + an.sin() * rr]
            })
            .collect();
        pen.fill(&star, GOLD, 1.0);
    }
    Spot {
        at: [x, y - jump + (top + hy) / 2.0],
        r: 11.0 * u,
        number,
    }
}

pub fn ball(pen: &mut Pen, pitch: &Pitch, b: &Ball) {
    let p = pitch.to_s(b.l, b.s);
    let r = 2.5 * pitch.k;
    shadow(pen, pitch, p[0], p[1], r * (1.0 - b.h / 200.0), r * 0.5);
    let y = p[1] - r - b.h;
    pen.disc([p[0], y], r, hex(0xfbfbf6), 1.0);
    pen.disc([p[0] + r * 0.2, y - r * 0.1], r * 0.38, hex(0x2a2c33), 1.0);
}

/// The day's three drifting cloud shadows: centre and radius.
pub fn clouds(pitch: &Pitch) -> impl Iterator<Item = (Pt, f32)> + '_ {
    let (w, h, t) = (pitch.w, pitch.h, pitch.t);
    (0..3).map(move |i| {
        let i = i as f32;
        let r = w.max(h) * 0.32;
        let x = (t * (9.0 + i * 4.0) + i * 520.0) % (w + r * 2.0) - r;
        ([x, h * (0.25 + 0.3 * i)], r)
    })
}

/// The crowd's wave and the night's camera flashes.
pub fn ambient(pen: &mut Pen, pitch: &Pitch) {
    let (w, h, t, n, g) = (pitch.w, pitch.h, pitch.t, pitch.nt, pitch.g);
    // The wave: strips of a band brightest at its middle, in the stands only.
    let (gx0, gy0, gx1, gy1) = (g.x - 8.0, g.y - 8.0, g.x + g.w + 8.0, g.y + g.h + 8.0);
    let wx = (t * 170.0) % (w + 400.0) - 200.0;
    let peak = 0.16 - 0.08 * n;
    let strips = 8;
    for i in 0..strips {
        let x0 = wx - 80.0 + 160.0 * i as f32 / strips as f32;
        let x1 = x0 + 160.0 / strips as f32;
        let a = peak * (1.0 - ((x0 + x1) / 2.0 - wx).abs() / 80.0);
        let dx = x1 - x0;
        pen.rect(x0, 0.0, dx, gy0.max(0.0), [255.0; 3], a);
        pen.rect(x0, gy1, dx, (h - gy1).max(0.0), [255.0; 3], a);
        if x0 < gx0 {
            pen.rect(x0, gy0, x1.min(gx0) - x0, gy1 - gy0, [255.0; 3], a);
        }
        if x1 > gx1 {
            let l = x0.max(gx1);
            pen.rect(l, gy0, x1 - l, gy1 - gy0, [255.0; 3], a);
        }
    }
    for f in &pitch.flashes {
        let a = (f.life / 0.16).max(0.0);
        pen.glow([f.x, f.y], 9.0 * a + 3.0, [255.0; 3], a);
    }
}

pub fn pigeons(pen: &mut Pen, pitch: &Pitch) {
    let (k, t, alpha) = (pitch.k, pitch.t, 1.0 - pitch.nt);
    for b in &pitch.pigeons {
        if alpha * b.a < 0.02 {
            continue;
        }
        let [x, y] = pitch.to_s(b.l, b.s);
        let r = 2.2 * k;
        pen.fade = alpha * b.a;
        if b.fly > 0.0 {
            let w = (t * 30.0).sin() * 4.0 * k;
            let wing = [
                [x - 4.0 * k, y - 6.0 * k - w],
                [x, y - 6.0 * k],
                [x + 4.0 * k, y - 6.0 * k - w],
            ];
            pen.stroke(&wing, false, 1.4, hex(0x6c7280), 1.0);
        } else {
            let bob = if (b.peck * 5.0).sin() > 0.6 {
                1.2 * k
            } else {
                0.0
            };
            let under = oval(pen, [x + 1.0, y + 1.0], r * 1.3, r * 0.6, 0.0);
            pen.fill(&under, [0.0; 3], 0.18);
            let body = oval(pen, [x, y - r * 0.6], r * 1.4, r * 0.9, 0.0);
            pen.fill(&body, hex(0x8a909c), 1.0);
            pen.disc(
                [x + r * 1.2, y - r * 1.1 + bob],
                r * 0.6,
                hex(0x5d6370),
                1.0,
            );
        }
        pen.fade = 1.0;
    }
}

/// Confetti, then the moths round the floodlights at night.
pub fn confetti(pen: &mut Pen, pitch: &Pitch) {
    for p in &pitch.parts {
        pen.fade = p.life.min(1.0);
        let (s, c) = p.rot.sin_cos();
        let pts = [[-2.0, -1.0], [2.0, -1.0], [2.0, 1.0], [-2.0, 1.0]]
            .map(|[a, b]: Pt| [p.x + a * c - b * s, p.y + a * s + b * c]);
        pen.fill(&pts, p.c, 1.0);
    }
    pen.fade = 1.0;
    let n = pitch.nt;
    if n > 0.05 {
        for tw in &pitch.towers {
            for i in 0..5 {
                let i = i as f32;
                let an = pitch.t * (0.8 + i * 0.3) + i * 2.0;
                let r = 10.0 + i * 4.0;
                let (x, y) = (tw[0] + an.cos() * r, tw[1] + (an * 1.3).sin() * r * 0.7);
                pen.rect(x, y, 1.4, 1.4, [255.0, 250.0, 220.0], 0.7 * n);
            }
        }
    }
}
