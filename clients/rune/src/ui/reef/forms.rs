//! The exotic forms an agent grows into at tier 3, one per harness, as the
//! prototype's `lionfish`, `manta`, `seahorse`, `cuttlefish` and `octopus`:
//! drawn in the fish's own frame, nose at +x, in its species' colours. A
//! gradient is nested bands, a dashed ray is its dashes, a tapering arm is
//! three strokes.

use std::f32::consts::{FRAC_PI_2, TAU};

use super::fish::{body, inside_convex};
use super::pen::{Pen, Xf, clip_convex, clip_half, quad};
use crate::creature::Form;
use crate::reef::{Fish, Mood, Pt, Rgb, lerp, mix};

const WHITE: Rgb = [255.0, 255.0, 255.0];
const BLACK: Rgb = [0.0, 0.0, 0.0];

/// A creature's colours this frame: shaded for idle, inked toward night.
pub struct Tint {
    pub body: Rgb,
    pub belly: Rgb,
    pub fin: Rgb,
    pub ink: Rgb,
    pub night: f32,
}

/// Everything a form needs: where, how big, and when.
pub struct Ctx<'a> {
    pub f: &'a Fish,
    pub xf: Xf,
    pub l: f32,
    pub wag: f32,
    pub s: f32,
    pub t: f32,
    pub c: &'a Tint,
}

impl Ctx<'_> {
    fn fill(&self, pen: &mut Pen, pts: &[Pt], c: Rgb, a: f32) {
        pen.fill(&self.xf.all(pts), c, a);
    }

    fn line(&self, pen: &mut Pen, pts: &[Pt], w: f32, c: Rgb, a: f32) {
        pen.stroke(&self.xf.all(pts), false, w, c, a);
    }

    fn oval(&self, pen: &mut Pen, at: Pt, rx: f32, ry: f32, rot: f32) -> Vec<Pt> {
        let mut pts = pen.ellipse(at, rx, ry, rot, 0.0, TAU);
        pts.pop();
        pts
    }

    /// The prototype's `bodyFill`: a darker back through the body colour to
    /// the belly, from `y0` down to `y1`, as nested bands. `to` carries a
    /// shape in a frame of its own into the fish's.
    fn shade(&self, pen: &mut Pen, poly: &[Pt], [y0, y1]: [f32; 2], to: &dyn Fn(Pt) -> Pt) {
        let c = self.c;
        let at = |v: f32| {
            let dark = mix(c.body, BLACK, 0.25);
            let v = v.clamp(0.0, 1.0);
            if v < 0.5 {
                mix(dark, c.body, v / 0.5)
            } else {
                mix(c.body, c.belly, (v - 0.5) / 0.5)
            }
        };
        let carry = |pts: &[Pt]| -> Vec<Pt> { pts.iter().map(|&p| self.xf.p(to(p))).collect() };
        pen.fill(&carry(poly), at(1.0), 1.0);
        const N: usize = 6;
        for k in (1..N).rev() {
            let y = y0 + (y1 - y0) * k as f32 / N as f32;
            pen.fill(
                &carry(&clip_half(poly, y)),
                at((k as f32 - 0.5) / N as f32),
                1.0,
            );
        }
    }

    /// The prototype's `eye`: white, a pupil (narrow when idle), a glint.
    pub fn eye(&self, pen: &mut Pen, [x, y]: Pt, r: f32) {
        let pupil = r * if self.f.who.mood == Mood::Idle {
            0.45
        } else {
            0.62
        };
        for (at, rad, c) in [
            ([x, y], r, [246.0, 251.0, 255.0]),
            ([x + r * 0.2, y], pupil, [7.0, 21.0, 33.0]),
            ([x + r * 0.35, y - r * 0.3], r * 0.22, WHITE),
        ] {
            let disc = self.oval(pen, at, rad, rad, 0.0);
            self.fill(pen, &disc, c, 1.0);
        }
    }
}

fn path(start: Pt, rest: impl IntoIterator<Item = Pt>) -> Vec<Pt> {
    std::iter::once(start).chain(rest).collect()
}

pub fn paint(pen: &mut Pen, cx: &Ctx<'_>, form: Form) {
    match form {
        Form::Lionfish => lionfish(pen, cx),
        Form::Manta => manta(pen, cx),
        Form::Seahorse => seahorse(pen, cx),
        Form::Cuttlefish => cuttlefish(pen, cx),
        Form::Octopus => octopus(pen, cx),
    }
}

fn lionfish(pen: &mut Pen, cx: &Ctx<'_>) {
    let (l, wag, s, t, c) = (cx.l, cx.wag, cx.s, cx.t, cx.c);
    let h = l * 0.24;
    let ray = |b: Pt, a: f32, len: f32| [b[0] + a.cos() * len, b[1] + a.sin() * len];
    // Dorsal spines over a translucent web.
    let spines: Vec<(Pt, Pt)> = (0..=10)
        .map(|i| {
            let u = i as f32 / 10.0;
            let b = [l * (0.24 - u * 0.5), -h * (0.75 - u * 0.25)];
            let a = -FRAC_PI_2 - 0.35 - u * 0.6 + (t * 1.8 + i as f32).sin() * 0.05;
            (b, ray(b, a, l * (0.5 - u * 0.22)))
        })
        .collect();
    let web = path(
        spines[0].0,
        spines.iter().map(|sp| sp.1).chain([spines[10].0]),
    );
    cx.fill(pen, &web, c.fin, 0.2);
    for (b, tip) in &spines {
        cx.line(pen, &[*b, *tip], 1.3 * s, c.fin, 0.95);
    }
    // Pectoral fans, slowly opening and closing, their rays dashed.
    let flap = (cx.f.phase * 0.7).sin() * 0.22;
    let o = [l * 0.1, h * 0.35];
    let fan: Vec<Pt> = (0..=9)
        .map(|i| {
            let len = l * (0.55 - (i as f32 - 4.5).abs() * 0.03);
            ray(o, 0.35 + flap + i as f32 * 0.2, len)
        })
        .collect();
    cx.fill(
        pen,
        &path(o, fan.iter().copied()),
        mix(c.fin, c.belly, 0.3),
        0.3,
    );
    let (dash, gap) = (3.0 * s, 2.5 * s);
    for tip in &fan {
        let d = (tip[0] - o[0]).hypot(tip[1] - o[1]);
        let at = |k: f32| {
            [
                o[0] + (tip[0] - o[0]) * k / d,
                o[1] + (tip[1] - o[1]) * k / d,
            ]
        };
        let mut k = 0.0;
        while k < d {
            cx.line(pen, &[at(k), at((k + dash).min(d))], 1.1 * s, c.fin, 0.85);
            k += dash + gap;
        }
    }
    // A rounded tail fan.
    let root = [-l * 0.28, wag * 0.4];
    let tail = path(
        root,
        quad(root, [-l * 0.6, -h * 1.1 + wag], [-l * 0.62, wag], 6).chain(quad(
            [-l * 0.62, wag],
            [-l * 0.6, h * 1.1 + wag],
            root,
            6,
        )),
    );
    cx.fill(pen, &tail, c.fin, 0.55);
    let outline = body(l, h, wag);
    cx.shade(pen, &outline, [-h, h], &|p| p);
    // Bold stripes, cut to the body.
    let stripe = mix([255.0, 246.0, 236.0], c.ink, c.night);
    for i in 0..7 {
        let bx = l * (0.36 - i as f32 * 0.1);
        let a = [bx + h * 0.15, -h * 1.2];
        let line = path(a, quad(a, [bx - h * 0.2, 0.0], [bx + h * 0.1, h * 1.2], 8));
        for run in line
            .split(|p| !inside_convex(*p, &outline))
            .filter(|r| r.len() > 1)
        {
            cx.line(pen, run, l * 0.035, stripe, 0.9);
        }
    }
    cx.eye(pen, [l * 0.34, -h * 0.25], l * 0.055);
}

fn manta(pen: &mut Pen, cx: &Ctx<'_>) {
    let (l, wag, s, c) = (cx.l, cx.wag, cx.s, cx.c);
    let flap = (cx.f.phase * 0.5).sin();
    let (wy, wy2) = (
        l * 0.5 * (0.65 + 0.35 * flap),
        l * 0.22 * (0.65 - 0.35 * flap),
    );
    let a = [-l * 0.2, 0.0];
    let tail = path(a, quad(a, [-l * 0.5, wag * 1.4], [-l * 0.9, wag * 2.2], 8));
    cx.line(pen, &tail, 1.6 * s, c.body, 0.9);
    // The far wing, in shadow.
    let a = [l * 0.3, 0.0];
    let far = path(
        a,
        quad(a, [l * 0.05, wy2 * 0.7], [-l * 0.1, wy2], 6).chain(quad(
            [-l * 0.1, wy2],
            [-l * 0.1, wy2 * 0.4],
            [-l * 0.22, 0.0],
            6,
        )),
    );
    cx.fill(pen, &far, mix(c.body, BLACK, 0.35), 1.0);
    let (nose, tip, root) = ([l * 0.36, -l * 0.02], [-l * 0.08, -wy], [-l * 0.24, 0.0]);
    let wing = path(
        nose,
        quad(nose, [l * 0.16, -wy * 0.95], tip, 8)
            .chain(quad(tip, [-l * 0.12, -wy * 0.35], root, 6))
            .chain(quad(root, [0.0, l * 0.08], [l * 0.36, l * 0.03], 6)),
    );
    cx.shade(pen, &wing, [-wy, l * 0.08], &|p| p);
    let a = [l * 0.3, -l * 0.06];
    cx.line(
        pen,
        &path(a, quad(a, [l * 0.14, -wy * 0.9], tip, 8)),
        1.6 * s,
        c.fin,
        0.8,
    );
    let spot = cx.oval(pen, [l * 0.1, -l * 0.07], l * 0.07, l * 0.035, -0.5);
    cx.fill(pen, &spot, mix(c.belly, WHITE, 0.4), 0.75);
    // Cephalic fins.
    let horn = mix(c.body, BLACK, 0.2);
    for (a, ctl, b) in [
        ([0.34, -0.02], [0.44, -0.04], [0.42, 0.03]),
        ([0.34, 0.02], [0.44, 0.06], [0.4, 0.08]),
    ] {
        let p = |q: [f32; 2]| [l * q[0], l * q[1]];
        cx.line(
            pen,
            &path(p(a), quad(p(a), p(ctl), p(b), 5)),
            2.4 * s,
            horn,
            1.0,
        );
    }
    cx.eye(pen, [l * 0.3, -l * 0.035], l * 0.03);
}

fn seahorse(pen: &mut Pen, cx: &Ctx<'_>) {
    const PTS: [[f32; 2]; 11] = [
        [0.06, -0.36],
        [0.0, -0.25],
        [0.07, -0.12],
        [0.1, 0.02],
        [0.05, 0.16],
        [-0.03, 0.26],
        [-0.09, 0.33],
        [-0.11, 0.41],
        [-0.05, 0.46],
        [0.0, 0.41],
        [-0.03, 0.37],
    ];
    const WD: [f32; 11] = [
        0.08, 0.08, 0.11, 0.12, 0.1, 0.075, 0.055, 0.04, 0.03, 0.022, 0.018,
    ];
    let (l, s, t, c) = (cx.l, cx.s, cx.t, cx.c);
    let sway = (t * 1.4 + cx.f.seed).sin() * 0.02;
    let p = |i: usize| [l * (PTS[i][0] + sway * (i as f32 / 11.0)), l * PTS[i][1]];
    // The dorsal fin's flutter.
    let [dx, dy] = p(3);
    let fin = path(
        [dx - l * 0.08, dy - l * 0.06],
        (0..=6)
            .map(|i| {
                let x = dx - l * (0.13 + 0.03 * (t * 22.0 + i as f32).sin());
                [x, dy - l * 0.06 + i as f32 * l * 0.025]
            })
            .chain([[dx - l * 0.08, dy + l * 0.09]]),
    );
    cx.fill(pen, &fin, c.fin, 0.55);
    // A body of overlapping discs, shaded head to tail.
    let (y0, y1) = (-l * 0.4, l * 0.45);
    let dark = mix(c.body, BLACK, 0.25);
    for i in 0..PTS.len() - 1 {
        let (a, b) = (p(i), p(i + 1));
        for k in 0..4 {
            let u = k as f32 / 4.0;
            let at = [lerp(a[0], b[0], u), lerp(a[1], b[1], u)];
            let v = (at[1] - y0) / (y1 - y0);
            let col = if v < 0.5 {
                mix(dark, c.body, v / 0.5)
            } else {
                mix(c.body, c.belly, (v - 0.5) / 0.5)
            };
            let r = l * lerp(WD[i], WD[i + 1], u) * 0.5;
            let disc = cx.oval(pen, at, r, r, 0.0);
            cx.fill(pen, &disc, col, 1.0);
        }
    }
    let ridge = mix(c.body, BLACK, 0.35);
    for (i, wd) in WD.iter().enumerate().take(8).skip(1) {
        let [x, y] = p(i);
        let w = l * wd * 0.5;
        cx.line(pen, &[[x - w, y], [x + w, y]], s, ridge, 0.5);
    }
    let [hx, hy] = p(0);
    let head = cx.oval(
        pen,
        [hx + l * 0.03, hy - l * 0.02],
        l * 0.075,
        l * 0.06,
        0.3,
    );
    cx.fill(pen, &head, c.body, 1.0);
    let snout = [
        [hx + l * 0.06, hy - l * 0.01],
        [hx + l * 0.2, hy + l * 0.02],
    ];
    cx.line(pen, &snout, l * 0.04, c.body, 1.0);
    for k in [-0.03, 0.0, 0.03] {
        let crown = [
            [hx + l * (k - 0.012), hy - l * 0.07],
            [hx + l * k, hy - l * 0.12],
            [hx + l * (k + 0.012), hy - l * 0.07],
        ];
        cx.fill(pen, &crown, c.fin, 1.0);
    }
    cx.eye(pen, [hx + l * 0.045, hy - l * 0.025], l * 0.025);
}

fn cuttlefish(pen: &mut Pen, cx: &Ctx<'_>) {
    let (l, wag, t, c) = (cx.l, cx.wag, cx.t, cx.c);
    let (bl, bh) = (l * 0.42, l * 0.16);
    let edge = |x: f32| bh * (1.0 - (x / bl).powi(2)).max(0.0).sqrt();
    let x_at = |i: usize| lerp(-bl * 0.95, bl * 0.75, i as f32 / 24.0);
    // The rippling fin skirt, each side.
    for side in [-1.0, 1.0] {
        let wave = (0..=24).map(|i| {
            let (u, x) = (i as f32 / 24.0, x_at(i));
            let ripple = side * l * 0.05 * (0.6 + 0.4 * (u * 16.0 - t * 9.0).sin());
            [x, side * edge(x) + ripple]
        });
        let back = (0..=24)
            .rev()
            .map(|i| [x_at(i), side * edge(x_at(i)) * 0.9]);
        cx.fill(pen, &wave.chain(back).collect::<Vec<_>>(), c.fin, 0.45);
    }
    let arm = mix(c.body, c.belly, 0.3);
    for k in 0..8 {
        let y0 = (k as f32 / 7.0 - 0.5) * bh * 0.9;
        let a = [bl * 0.95, y0];
        let ctl = [bl * 1.2, y0 * 1.3 + (t * 3.0 + k as f32).sin() * l * 0.015];
        let tip = [bl * 1.38, y0 * 1.1 + wag * 0.3];
        cx.line(pen, &path(a, quad(a, ctl, tip, 5)), l * 0.022, arm, 1.0);
    }
    let mantle = cx.oval(pen, [0.0, 0.0], bl, bh, 0.0);
    cx.shade(pen, &mantle, [-bh, bh], &|p| p);
    // Bands of colour passing down the mantle.
    let band = mix(c.body, [10.0, 0.0, 30.0], 0.55);
    for k in 0..5 {
        let x = ((k as f32 / 5.0) + t * 0.3 + cx.f.seed).rem_euclid(1.0) * 2.4 * bl - 1.2 * bl;
        let stripe = cx.oval(pen, [x, 0.0], bl * 0.07, bh * 1.2, 0.25);
        cx.fill(pen, &clip_convex(&stripe, &mantle), band, 0.4);
    }
    let head = cx.oval(pen, [bl * 0.92, 0.0], bh * 0.75, bh * 0.7, 0.0);
    cx.fill(pen, &head, c.body, 1.0);
    // Its W-shaped pupil.
    let (ex, ey, er) = (bl * 0.9, -bh * 0.3, l * 0.04);
    let white = cx.oval(pen, [ex, ey], er, er, 0.0);
    cx.fill(pen, &white, [246.0, 251.0, 255.0], 1.0);
    let w = [
        (-0.6, -0.2),
        (-0.3, 0.35),
        (0.0, 0.0),
        (0.3, 0.35),
        (0.6, -0.2),
    ]
    .map(|(x, y)| [ex + er * x, ey + er * y]);
    cx.line(pen, &w, er * 0.4, [7.0, 21.0, 33.0], 1.0);
}

fn octopus(pen: &mut Pen, cx: &Ctx<'_>) {
    let (l, s, t, c) = (cx.l, cx.s, cx.t, cx.c);
    let pulse = ((cx.f.phase * 0.6).sin() + 1.0) / 2.0;
    for k in 0..8 {
        let b = (k as f32 / 7.0 - 0.5) * l * 0.16;
        let col = mix(c.body, BLACK, 0.15 + 0.1 * (k % 2) as f32);
        let at = |j: usize| {
            let j = j as f32;
            let x = l * 0.02 - j * l * 0.065 * (1.0 + pulse * 0.25);
            let curl = (j * 0.7 - t * 4.0 + k as f32).sin() * l * 0.03 * (j / 10.0);
            [x, b * (1.0 + j * 0.12) + curl]
        };
        // ponytail: three strokes stand in for the prototype's ten
        // tapering segments per arm; more if the taper ever looks stepped.
        for (from, to) in [(0, 4), (3, 7), (6, 10)] {
            let arm: Vec<Pt> = (from..=to).map(at).collect();
            let mid = (from + to) as f32 / 2.0;
            cx.line(pen, &arm, l * (0.045 - mid * 0.0038), col, 1.0);
        }
    }
    let frame = Xf::new([l * 0.18, 0.0], -0.15, 1.0);
    let (rx, ry) = (
        l * 0.22 * (1.0 - pulse * 0.08),
        l * 0.16 * (1.0 + pulse * 0.1),
    );
    let mantle = cx.oval(pen, [0.0, 0.0], rx, ry, 0.0);
    cx.shade(pen, &mantle, [-l * 0.16, l * 0.16], &|p| frame.p(p));
    let spot = mix(c.belly, WHITE, 0.3);
    for [sx, sy] in [[0.05, -0.08], [-0.06, -0.04], [0.1, 0.02], [-0.02, 0.06]] {
        let disc = cx.oval(pen, [l * sx, l * sy], 2.0 * s, 2.0 * s, 0.0);
        cx.fill(pen, &frame.all(&disc), spot, 0.5);
    }
    cx.eye(pen, [l * 0.02, -l * 0.07], l * 0.04);
}
