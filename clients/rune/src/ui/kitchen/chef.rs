//! A chef, as the prototype's `chef`: legs, arms acting out its pose,
//! jacket and harness-coloured apron, head, and the toque whose height is
//! its rank (gold, with glints, for the star). Then what's on the stations
//! (`prop`), the pass (`passProps`) and in the air (`drawParts`).

use std::f32::consts::{PI, TAU};

use super::room::{BLACK, WHITE, hex, rr};
use crate::kitchen::{Chef, Goal, HAT, Kitchen, Line, Part, PartKind};
use crate::reef::{Mood, Pt, Rgb, mix};
use crate::ui::reef::pen::Pen;

pub const AMBER: Rgb = [255.0, 181.0, 71.0];
pub const MINT: Rgb = [98.0, 230.0, 180.0];
const SKIN: [Rgb; 5] = [
    [241.0, 200.0, 166.0],
    [224.0, 172.0, 128.0],
    [190.0, 134.0, 92.0],
    [140.0, 94.0, 62.0],
    [98.0, 66.0, 44.0],
];

fn grey(c: Rgb, k: f32) -> Rgb {
    let l = c[0] * 0.3 + c[1] * 0.59 + c[2] * 0.11;
    mix(c, [l; 3], k)
}

#[derive(Clone, Copy, PartialEq)]
enum Pose {
    Walk,
    Work(u8),
    Ring,
    Cheer,
    Sit,
}

/// The chef's own frame: feet at the origin, scaled.
#[derive(Clone, Copy)]
struct F {
    o: Pt,
    k: f32,
}

impl F {
    fn p(self, x: f32, y: f32) -> Pt {
        [self.o[0] + x * self.k, self.o[1] + y * self.k]
    }
    fn all(self, pts: &[Pt]) -> Vec<Pt> {
        pts.iter().map(|q| self.p(q[0], q[1])).collect()
    }
    fn up(self, dy: f32) -> F {
        F {
            o: [self.o[0], self.o[1] + dy * self.k],
            k: self.k,
        }
    }
}

pub fn chef(pen: &mut Pen, kt: &Kitchen, c: &Chef) {
    let (t, ph) = (kt.t, c.ph);
    let k = c.who.species;
    let idle = c.who.mood == Mood::Idle;
    let dk = if idle { 0.6 } else { 0.0 };
    let (body, fin) = (grey(k.body, dk), grey(k.fin, dk));
    let jacket = if idle {
        [196.0, 198.0, 200.0]
    } else {
        [250.0, 250.0, 248.0]
    };
    let skin = grey(SKIN[c.seed as usize % SKIN.len()], dk * 0.6);
    let pose = match (c.arrived(), c.who.mood, c.goal) {
        (false, ..) => Pose::Walk,
        (_, Mood::Working, Goal::Station(s)) => Pose::Work(kt.line_of(c).work[s].kind),
        (_, Mood::Working, _) => Pose::Work(0),
        (_, Mood::Waiting, _) => Pose::Ring,
        (_, Mood::Done, _) => Pose::Cheer,
        (_, Mood::Idle, _) => Pose::Sit,
    };
    let (front_face, walk, sit) = (!c.up, c.walk, pose == Pose::Sit);
    let mut f = F {
        o: [c.x, c.y],
        k: kt.chef_scale(c),
    };
    pen.fade = if idle { 0.92 } else { 1.0 };
    let shadow = pen.ellipse(f.o, 11.0 * f.k, 4.0 * f.k, 0.0, 0.0, TAU);
    pen.fill(&shadow, BLACK, 0.25);
    if sit {
        f = f.up(-8.0);
    }
    // Legs.
    let (trouser, shoe) = (hex(0x2b2d35), hex(0x121316));
    let rect = |pen: &mut Pen, f: F, x: f32, y: f32, w: f32, h: f32, c: Rgb| {
        pen.fill(
            &f.all(&[[x, y], [x + w, y], [x + w, y + h], [x, y + h]]),
            c,
            1.0,
        );
    };
    if sit {
        rect(pen, f, -6.0, -9.0, 4.0, 8.0, trouser);
        rect(pen, f, 2.0, -9.0, 4.0, 8.0, trouser);
        rect(pen, f, -7.0, -2.0, 6.0, 3.0, shoe);
        rect(pen, f, 1.0, -2.0, 6.0, 3.0, shoe);
    } else {
        let sw = ph.sin() * 3.0 * walk;
        rect(pen, f, -6.0, -9.0 + sw, 4.0, 9.0 - sw, trouser);
        rect(pen, f, 2.0, -9.0 - sw, 4.0, 9.0 + sw, trouser);
        rect(pen, f, -7.0, -1.0 + sw.max(0.0) * 0.3, 6.0, 2.5, shoe);
        rect(pen, f, 1.0, -1.0 + (-sw).max(0.0) * 0.3, 6.0, 2.5, shoe);
    }
    let bob = if walk > 0.0 {
        ph.sin().abs() * 1.5
    } else {
        0.0
    };
    let f = f.up(-bob);
    // Arms: behind the body when it faces away, in front when it faces you.
    let arms = |pen: &mut Pen, front: bool| {
        let arm = |pen: &mut Pen, s: Pt, h: Pt| {
            if front != front_face {
                return;
            }
            pen.line(f.p(s[0], s[1]), f.p(h[0], h[1]), 4.2 * f.k, jacket, 1.0);
            pen.disc(f.p(h[0], h[1]), 2.3 * f.k, skin, 1.0);
        };
        match pose {
            Pose::Work(kind) => {
                let w1 = if kind == 1 {
                    (t * 11.0 + ph).sin().abs() * -5.0
                } else {
                    (t * 5.0 + ph).cos() * 3.0
                };
                let w2 = if kind == 1 {
                    0.0
                } else {
                    (t * 5.0 + ph).sin() * 2.0
                };
                arm(pen, [-8.0, -20.0], [-11.0, -27.0 + w2]);
                let hx = 11.0 + if kind == 0 { w1 } else { 0.0 };
                arm(
                    pen,
                    [8.0, -20.0],
                    [hx, -27.0 + if kind == 1 { w1 } else { w2 }],
                );
            }
            Pose::Ring => {
                let wave = (t * 7.0).sin() * 3.0;
                let tap = (t * PI / 0.8).sin().max(0.0) * 4.0;
                arm(pen, [-8.0, -21.0], [-13.0 + wave, -40.0]);
                arm(pen, [8.0, -20.0], [9.0, -12.0 + tap]);
            }
            Pose::Cheer => {
                if (t + ph) % 3.0 < 0.7 {
                    arm(pen, [-8.0, -21.0], [-14.0, -37.0]);
                    arm(pen, [8.0, -21.0], [14.0, -37.0]);
                } else {
                    arm(pen, [-8.0, -20.0], [-9.0, -10.0]);
                    arm(pen, [8.0, -20.0], [4.0, -19.0]);
                }
            }
            Pose::Sit => {
                let sip = (t * 0.4 + ph) % 3.0 < 0.8;
                arm(pen, [-8.0, -20.0], [-4.0, -11.0]);
                arm(
                    pen,
                    [8.0, -20.0],
                    if sip { [3.0, -25.0] } else { [5.0, -12.0] },
                );
                if front {
                    let (x, y) = if sip { (1.0, -27.0) } else { (3.0, -14.0) };
                    pen.fill(
                        &f.all(&[[x, y], [x + 5.0, y], [x + 5.0, y + 5.0], [x, y + 5.0]]),
                        hex(0xece6da),
                        1.0,
                    );
                }
            }
            Pose::Walk => {
                let sw = ph.sin() * 4.0 * walk;
                arm(pen, [-8.0, -20.0], [-10.0, -11.0 + sw]);
                arm(pen, [8.0, -20.0], [10.0, -11.0 - sw]);
            }
        }
    };
    arms(pen, false);
    // Jacket and apron.
    let box_ = |pen: &mut Pen, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgb, a: f32| {
        pen.fill(&f.all(&rr(x, y, w, h, r)), c, a);
    };
    box_(pen, -9.0, -25.0, 18.0, 17.0, 5.0, jacket, 1.0);
    box_(pen, 3.0, -25.0, 6.0, 17.0, 4.0, BLACK, 0.08);
    box_(pen, -9.5, -16.0, 19.0, 11.0, 3.0, body, 1.0);
    if front_face {
        box_(pen, -9.5, -16.0, 19.0, 1.6, 0.0, grey(k.belly, dk), 0.55);
        for yy in [-22.0, -19.0] {
            for xx in [-3.0, 3.0] {
                pen.disc(f.p(xx, yy), 0.8 * f.k, [40.0; 3], 0.5);
            }
        }
        pen.fill(
            &f.all(&[[-5.0, -25.0], [5.0, -25.0], [0.0, -20.0]]),
            fin,
            1.0,
        );
    } else {
        pen.line(f.p(-6.0, -16.0), f.p(-2.0, -24.0), 1.6 * f.k, body, 1.0);
        pen.line(f.p(6.0, -16.0), f.p(2.0, -24.0), 1.6 * f.k, body, 1.0);
        box_(pen, -5.0, -26.0, 10.0, 2.5, 0.0, fin, 1.0);
    }
    // Head.
    pen.disc(f.p(0.0, -30.0), 6.5 * f.k, skin, 1.0);
    if front_face {
        let ink = hex(0x1d1a18);
        pen.disc(f.p(-2.3, -30.0), 0.9 * f.k, ink, 1.0);
        pen.disc(f.p(2.3, -30.0), 0.9 * f.k, ink, 1.0);
        let mouth = if pose == Pose::Ring {
            pen.ellipse(f.p(0.0, -26.6), 1.3 * f.k, 1.3 * f.k, 0.0, 0.0, TAU)
        } else {
            pen.ellipse(f.p(0.0, -28.0), 2.0 * f.k, 2.0 * f.k, 0.0, 0.2, PI - 0.2)
        };
        pen.stroke(&mouth, pose == Pose::Ring, 0.9 * f.k, hex(0x6b3a2c), 1.0);
    } else {
        let hair = pen.ellipse(f.p(0.0, -30.0), 6.5 * f.k, 6.5 * f.k, 0.0, 0.1, PI - 0.1);
        pen.fill(&hair, [60.0, 40.0, 30.0], 0.55);
    }
    // The toque: its height is rank, gold for the star.
    let (hh, gold) = (HAT[c.tier], c.tier == 4);
    let hat = if gold {
        [255.0, 206.0, 84.0]
    } else if idle {
        [210.0, 212.0, 214.0]
    } else {
        WHITE
    };
    let band = if gold {
        [214.0, 150.0, 40.0]
    } else {
        [214.0, 218.0, 222.0]
    };
    box_(pen, -6.5, -38.0, 13.0, 4.0, 1.5, band, 1.0);
    pen.fill(
        &f.all(&[
            [-6.0, -37.0],
            [-7.0, -37.0 - hh],
            [7.0, -37.0 - hh],
            [6.0, -37.0],
        ]),
        hat,
        1.0,
    );
    for (px, pr) in [(-4.5, 4.6), (0.0, 5.4), (4.5, 4.6)] {
        pen.disc(f.p(px, -37.0 - hh), pr * f.k, hat, 1.0);
    }
    for px in [-2.5, 2.5] {
        pen.line(
            f.p(px, -37.0),
            f.p(px * 1.15, -37.0 - hh),
            0.8 * f.k,
            band,
            0.6,
        );
    }
    if gold {
        let a = 0.6 + 0.4 * (t * 4.0 + ph).sin();
        for k in 0..3 {
            let an = t * 1.5 + k as f32 * 2.1;
            let (sx, sy) = (an.cos() * 10.0, -42.0 - hh + an.sin() * 4.0);
            let glint = [
                [sx, sy - 3.0],
                [sx + 1.0, sy],
                [sx, sy + 3.0],
                [sx - 1.0, sy],
            ];
            pen.fill(&f.all(&glint), [255.0, 248.0, 210.0], a);
            pen.fill(
                &f.all(&[
                    [sx - 3.0, sy - 0.5],
                    [sx + 3.0, sy - 0.5],
                    [sx + 3.0, sy + 0.5],
                    [sx - 3.0, sy + 0.5],
                ]),
                [255.0, 248.0, 210.0],
                a,
            );
        }
    }
    arms(pen, true);
    pen.fade = 1.0;
}

/// What's on a working station, prototype `prop`.
pub fn prop(pen: &mut Pen, kt: &Kitchen, l: &Line, c: &Chef, s: usize) {
    let st = &l.work[s];
    let (t, cs, ph) = (kt.t, kt.cs, c.ph);
    let (x, y, r) = (st.x, st.iy, 10.0 * cs);
    let odd = c.seed % 2 == 1;
    match st.kind {
        0 => {
            for k in 0..8 {
                let an = k as f32 / 8.0 * TAU + t * 0.6;
                let fl = 2.6 + (t * 19.0 + k as f32 * 2.3).sin() * 1.2;
                let (col, a) = if k % 2 == 1 {
                    ([90.0, 160.0, 255.0], 0.85)
                } else {
                    ([255.0, 150.0, 40.0], 0.9)
                };
                pen.disc(
                    [x + an.cos() * r * 1.02, y + an.sin() * r * 1.02],
                    fl * cs * 0.6,
                    col,
                    a,
                );
            }
            pen.disc([x, y], r * 0.95, hex(0x4a5057), 1.0);
            pen.disc([x, y], r * 0.82, hex(0xc2c8cd), 1.0);
            pen.disc(
                [x, y],
                r * 0.66,
                if odd { hex(0xc7672f) } else { hex(0xd9a441) },
                1.0,
            );
            let an = t * 4.0 + ph;
            pen.line(
                [x + an.cos() * r * 0.4, y + an.sin() * r * 0.4],
                [x + r * 0.6 + 6.0, y + r * 0.9 + 6.0],
                1.6,
                hex(0xe8ecef),
                1.0,
            );
        }
        1 | 3 => {
            let chop = (t * 11.0 + ph).sin().abs();
            if st.kind == 1 {
                const VEG: [u32; 5] = [0x5fae4a, 0xe2533f, 0xf2c14e, 0x7cc35a, 0xe8e2c8];
                for k in 0..5 {
                    let c = hex(VEG[(k + c.seed as usize) % 5]);
                    pen.rect(
                        x - r * 0.9 + k as f32 * 3.3,
                        y - 2.0 + (k % 2) as f32 * 3.0,
                        3.0,
                        3.0,
                        c,
                        1.0,
                    );
                }
            } else {
                pen.disc([x, y], r * 0.75, WHITE, 1.0);
                pen.disc([x - 2.0, y - 1.0], 3.0, hex(0x7cc35a), 1.0);
                pen.disc([x + 2.0, y + 1.0], 2.5, hex(0xe2533f), 1.0);
            }
            pen.line(
                [x + r * 0.2, y - chop * 4.0],
                [x + r * 1.1, y - 2.0 - chop * 6.0],
                2.0,
                hex(0xeef2f5),
                1.0,
            );
        }
        _ => {
            let toss = ((t + ph) % 2.4) / 2.4;
            let up = if toss < 0.25 {
                (toss / 0.25 * PI).sin()
            } else {
                0.0
            };
            for k in 0..6 {
                let a = k as f32 + t * 9.0;
                pen.disc(
                    [x + a.cos() * r * 0.9, y + a.sin() * r * 0.9],
                    2.0 * cs,
                    [255.0, 140.0, 40.0],
                    0.85,
                );
            }
            pen.line(
                [x, y],
                [x + r * 0.9 + 6.0, y + r + 6.0],
                3.0,
                hex(0x26292d),
                1.0,
            );
            pen.disc([x, y], r * 0.9, hex(0x26292d), 1.0);
            pen.disc([x, y], r * 0.7, hex(0x3a3e43), 1.0);
            const FOOD: [u32; 4] = [0xe9b44c, 0xd4582e, 0x8fc35a, 0xf1d79a];
            for (k, food) in FOOD.iter().enumerate() {
                let lift = up * 14.0 + (k % 2) as f32 * up * 4.0;
                pen.disc(
                    [x - 3.0 + k as f32 * 2.0, y - 1.0 - lift],
                    2.0,
                    hex(*food),
                    1.0,
                );
            }
        }
    }
}

/// A line's pass: the heat lamps' glow, the ticket rail, plates for the
/// finished and a ringing bell and flapping amber ticket for each chef
/// there that needs you, prototype `passProps`.
pub fn pass(pen: &mut Pen, kt: &Kitchen, l: &Line, li: usize) {
    let (t, cs) = (kt.t, kt.cs);
    let (ty, ly) = (l.py0 + 3.0, l.py0 + l.pd * 0.42);
    let (x0, w) = (l.x0 + l.brk + 4.0, l.x1 - l.x0 - l.brk - 8.0);
    let hot = 0.28 + 0.05 * (t * 3.0).sin();
    pen.vgrad(x0, ly - 6.0, w, ly + 10.0, 6, |k| {
        ([255.0, 110.0, 40.0], hot * (1.0 - (k - 0.5).abs() * 2.0))
    });
    let at_pass = |m: Mood| {
        kt.chefs
            .iter()
            .filter(move |c| c.who.host == li && c.who.mood == m && c.arrived())
            .filter_map(|c| match c.goal {
                Goal::Pass(k) => Some((k, c)),
                _ => None,
            })
    };
    let ringing: Vec<usize> = at_pass(Mood::Waiting).map(|(k, _)| k).collect();
    for (k, p) in l.pass.iter().enumerate() {
        if !l.tickets[k] || ringing.contains(&k) {
            continue;
        }
        pen.rect(p[0] - 5.0, ty, 10.0, 9.0, hex(0xfbfaf4), 1.0);
        for n in 0..3 {
            pen.rect(
                p[0] - 3.5,
                ty + 2.5 + n as f32 * 2.0,
                6.0 - n as f32,
                0.8,
                BLACK,
                0.25,
            );
        }
    }
    for (k, c) in at_pass(Mood::Done) {
        let (px, py) = (l.pass[k][0], l.py0 + l.pd * 0.55);
        pen.glow([px, py], 16.0 * cs, MINT, 0.55);
        pen.disc([px, py], 7.5 * cs, WHITE, 1.0);
        pen.ring([px, py], 7.5 * cs, 1.2, MINT, 0.9);
        let sp = c.who.species;
        pen.disc([px - 1.5 * cs, py], 3.0 * cs, sp.body, 1.0);
        pen.disc([px + 2.5 * cs, py - 1.5 * cs], 1.8 * cs, hex(0x7cc35a), 1.0);
        pen.disc([px + 2.0 * cs, py + 2.0 * cs], 1.4 * cs, sp.fin, 1.0);
        let wisp: Vec<Pt> = (0..14)
            .map(|n| {
                let n = n as f32;
                [
                    px + (n * 0.6 - t * 3.0 + (c.seed % 7) as f32).sin() * 2.5,
                    py - 6.0 - n * 1.6 * cs,
                ]
            })
            .collect();
        pen.stroke(&wisp, false, 1.4, WHITE, 0.55);
    }
    for (k, c) in at_pass(Mood::Waiting) {
        let p = l.pass[k];
        let (bx, by) = (p[0] + 9.0 * cs, l.py0 + l.pd * 0.6);
        let ph = ((t - c.ring0).rem_euclid(1.6)) / 1.6;
        pen.ring(
            [bx, by],
            4.0 + ph * 26.0 * cs,
            2.2,
            AMBER,
            (1.0 - ph) * 0.95,
        );
        pen.ring([bx, by], 4.0 + ph * 14.0 * cs, 1.4, AMBER, (1.0 - ph) * 0.5);
        let base = pen.ellipse([bx, by + 2.0], 6.0 * cs, 2.5 * cs, 0.0, 0.0, TAU);
        pen.fill(&base, hex(0xc8a24a), 1.0);
        let dome = pen.ellipse([bx, by + 1.0], 5.0 * cs, 5.0 * cs, 0.0, PI, TAU);
        pen.fill(&dome, hex(0xf3cf6a), 1.0);
        pen.disc([bx, by - 4.0 * cs], 1.4 * cs, hex(0xfff3c4), 1.0);
        // The amber ticket flapping on the rail, glowing.
        let flap = (t * 9.0 + p[0]).sin() * 0.5 + 0.5;
        let pulse = 0.75 + 0.25 * (t * 5.0).sin();
        let (skew, sy) = ((flap - 0.5) * 0.5, 0.75 + flap * 0.25);
        let o = [p[0] - 2.0, ty];
        let tk = |x: f32, y: f32| [o[0] + x + skew * y * sy, o[1] + y * sy];
        pen.glow([o[0] + skew * 6.0, ty + 6.5 * sy], 14.0 * pulse, AMBER, 0.6);
        pen.fill(
            &[tk(-6.0, 0.0), tk(6.0, 0.0), tk(6.0, 13.0), tk(-6.0, 13.0)],
            AMBER,
            1.0,
        );
        for (y, len) in [(3.0, 8.0), (6.0, 6.0), (9.0, 7.0)] {
            pen.fill(
                &[
                    tk(-4.0, y),
                    tk(-4.0 + len, y),
                    tk(-4.0 + len, y + 1.2),
                    tk(-4.0, y + 1.2),
                ],
                [60.0, 30.0, 0.0],
                0.6,
            );
        }
    }
    pen.rect(x0, ty - 1.0, w, 2.0, hex(0x9aa3ab), 1.0);
}

/// Steam, bits, sparks, flames and a finish's mint burst.
pub fn parts(pen: &mut Pen, parts: &[Part]) {
    for p in parts {
        let k = 1.0 - p.life / p.max;
        match p.kind {
            PartKind::Steam => pen.disc([p.x, p.y], p.r.max(0.1), WHITE, 0.32 * k),
            PartKind::Bit => pen.rect(p.x - 1.5, p.y - 1.5, 3.0, 3.0, p.c, k),
            _ => {}
        }
    }
    // The prototype adds these as light; here they lay over.
    for p in parts {
        let k = 1.0 - p.life / p.max;
        match p.kind {
            PartKind::Spark => pen.disc([p.x, p.y], 1.3, [255.0, 150.0 + 80.0 * k, 60.0], k),
            PartKind::Flame => {
                pen.glow([p.x, p.y], p.r, [255.0, 120.0, 30.0], 0.6 * k);
                pen.disc([p.x, p.y], p.r * 0.35, [255.0, 230.0, 140.0], 0.7 * k);
            }
            PartKind::Mint => pen.disc([p.x, p.y], p.r * k + 0.4, MINT, k * 0.9),
            _ => {}
        }
    }
}

/// A soft ellipse of light on the floor: `a` at the centre, none at the rim.
pub fn pool(pen: &mut Pen, c: Pt, rx: f32, ry: f32, col: Rgb, a: f32) {
    const N: usize = 6;
    for k in 0..N {
        let f = 1.0 - k as f32 / N as f32;
        let e = pen.ellipse(c, rx * f, ry * f, 0.0, 0.0, TAU);
        pen.fill(&e, col, a * 0.9 / N as f32);
    }
}
