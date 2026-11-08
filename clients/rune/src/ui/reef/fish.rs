//! A fish, as the prototype's `drawFish`: its mood's aura, a legendary's
//! orbiting motes and a level-up's gold ring, then its tier's form (an
//! exotic one from `forms`, else the reef fish's tail and fins, a body
//! shaded top to belly with its species' pattern, gill, pectoral fin and
//! eye, photophores at night), and the selected one's turning ring.

use std::f32::consts::{PI, TAU};

use super::Label;
use super::forms::{self, Ctx, Tint};
use super::pen::{Pen, Xf, clip_convex, cubic, quad};
use crate::reef::{Fish, Mood, Pal, Pattern, Pt, Reef, mix};

const WHITE: [f32; 3] = [255.0, 255.0, 255.0];
const BLACK: [f32; 3] = [0.0, 0.0, 0.0];
const GOLD: [f32; 3] = [255.0, 226.0, 140.0];

/// The body's outline in the fish's own frame, nose at +x.
pub(super) fn body(l: f32, h: f32, wag: f32) -> Vec<Pt> {
    let nose = [l * 0.5, 0.0];
    let top = [-l * 0.3, -h * 0.22 + wag * 0.4];
    let bottom = [-l * 0.3, h * 0.22 + wag * 0.4];
    let mut pts = vec![nose];
    pts.extend(cubic(
        nose,
        [l * 0.42, -h * 0.98],
        [-l * 0.06, -h * 1.05],
        top,
        12,
    ));
    pts.push(bottom);
    pts.extend(cubic(
        bottom,
        [-l * 0.06, h * 1.05],
        [l * 0.42, h * 0.98],
        nose,
        12,
    ));
    pts.pop();
    pts
}

/// Whether `p` is inside the convex `poly`, in either winding.
pub(super) fn inside_convex(p: Pt, poly: &[Pt]) -> bool {
    let mut sign = 0.0;
    for (i, &a) in poly.iter().enumerate() {
        let b = poly[(i + 1) % poly.len()];
        let c = ((b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])).signum();
        if sign == 0.0 {
            sign = c;
        } else if c != sign {
            return false;
        }
    }
    true
}

fn closed_quad(a: Pt, c: Pt, b: Pt) -> Vec<Pt> {
    std::iter::once(a).chain(quad(a, c, b, 8)).collect()
}

pub fn fish(pen: &mut Pen, r: &Reef, pal: &Pal, f: &Fish, labels: &mut Vec<Label>) {
    let (s, t, nt) = (r.s, r.t, r.nt);
    let k = f.who.species;
    let l = f.len(s);
    let sp = f.vx.hypot(f.vy) / s;
    let wag = f.phase.sin() * l * (0.045 + sp.min(120.0) / 120.0 * 0.06);
    let mood = f.who.mood;
    let mc = mood.color();
    let o = [f.x, f.y];
    match mood {
        Mood::Waiting => {
            pen.glow(o, l * 1.2, mc, 0.35 + 0.2 * (t * 5.0).sin());
            pen.ring(
                o,
                l * (0.6 + f.pulse * 0.9),
                2.0 * s,
                mc,
                (1.0 - f.pulse) * 0.9,
            );
        }
        Mood::Done => pen.glow(o, l * 0.95, mc, 0.22 + 0.12 * (t * 1.6 + f.seed).sin()),
        _ => {}
    }
    if nt > 0.05 {
        pen.glow(o, l * 1.25, k.body, 0.3 * nt);
    }

    if f.tier == 4 {
        for i in 0..6 {
            let a = t * 0.6 + i as f32 * TAU / 6.0;
            let at = [f.x + a.cos() * l * 0.78, f.y + a.sin() * l * 0.42];
            pen.glow(at, 6.0 * s, k.fin, 0.6);
        }
    }
    if f.level_up > 0.0 {
        let lu = f.level_up;
        pen.ring(o, l * (0.5 + (1.0 - lu) * 1.8), 3.0 * s, GOLD, lu);
        pen.glow(o, l * 1.4, GOLD, 0.5 * lu);
    }

    let xf = Xf::new(
        o,
        f.tilt * if f.face_s < 0.0 { -1.0 } else { 1.0 },
        f.face_s,
    );
    let shade = if mood == Mood::Idle { 0.3 } else { 0.0 };
    let night = nt * 0.45;
    let c = Tint {
        body: mix(mix(k.body, [120.0, 130.0, 140.0], shade), pal.ink, night),
        belly: mix(mix(k.belly, [170.0, 180.0, 190.0], shade), pal.ink, night),
        fin: mix(k.fin, pal.ink, night),
        ink: pal.ink,
        night,
    };
    let cx = Ctx {
        f,
        xf,
        l,
        wag,
        s,
        t,
        c: &c,
    };
    if f.tier >= 3 {
        forms::paint(pen, &cx, k.form);
    } else {
        reef_fish(pen, &cx, nt);
    }

    let selected = r.selected.as_ref() == Some(&f.who.id);
    if selected {
        // Dashes that keep their screen size as the camera zooms.
        let z = pen.zoom();
        let rad = l * 0.85;
        let period = 9.0 / z;
        let n = ((TAU * rad / period) as usize).max(6);
        let step = TAU / n as f32;
        let turn = -t * 12.0 / rad;
        for d in 0..n {
            let from = turn + d as f32 * step;
            let dash = pen.ellipse(o, rad, rad, 0.0, from, from + step * 4.0 / 9.0);
            pen.stroke(&dash, false, 1.4 * s / z, [230.0, 250.0, 255.0], 0.85);
        }
    }
    let hovered = r.hovered.as_ref() == Some(&f.who.id);
    let named = selected || hovered;
    if mood == Mood::Waiting {
        let text = if named {
            format!("{} · needs you", f.who.name)
        } else {
            "needs you".to_owned()
        };
        labels.push(Label {
            at: [f.x, f.y - l * 0.9],
            text,
            badge: None,
            amber: true,
        });
    } else if named {
        labels.push(Label {
            at: [f.x, f.y + l * 0.95],
            text: f.who.name.clone(),
            badge: None,
            amber: false,
        });
    }
}

/// The reef fish of tiers 0 to 2: a fry is faint and plain with a big eye,
/// an elder has the longer tail, taller fins and a sheen.
fn reef_fish(pen: &mut Pen, cx: &Ctx<'_>, nt: f32) {
    let (f, xf, l, wag, s, t) = (cx.f, cx.xf, cx.l, cx.wag, cx.s, cx.t);
    let (k, fry, elder) = (f.who.species, f.tier == 0, f.tier == 2);
    let h = l * 0.27 * k.fat;
    let (body_c, belly, fin, night) = (cx.c.body, cx.c.belly, cx.c.fin, cx.c.night);
    if fry {
        pen.fade = 0.8;
    }
    // Tail: an elder's reaches further, forked deeper.
    let (tip, fork) = if elder { (0.72, 1.25) } else { (0.57, 0.95) };
    let root = [-l * 0.27, wag * 0.4];
    let tip_up = [-l * tip, -h * fork + wag];
    let tip_down = [-l * tip, h * fork + wag];
    let mut tail = vec![root];
    tail.extend(quad(root, [-l * 0.42, -h * 0.2 + wag * 0.7], tip_up, 6));
    tail.extend(quad(tip_up, [-l * (tip - 0.12), wag * 0.95], tip_down, 6));
    tail.extend(quad(tip_down, [-l * 0.42, h * 0.2 + wag * 0.7], root, 6));
    tail.pop();
    pen.fill(&xf.all(&tail), fin, 0.95);
    // Dorsal, and a ventral on the tall ones; an elder's stand taller.
    let tall = k.tall + if elder { 0.7 } else { 0.0 };
    let (back, under) = if elder { (0.4, 0.36) } else { (0.24, 0.24) };
    let dorsal = closed_quad(
        [l * 0.22, -h * 0.82],
        [0.0, -h * (1.15 + 0.6 * tall)],
        [-l * back, -h * 0.42 + wag * 0.25],
    );
    pen.fill(&xf.all(&dorsal), fin, 0.95);
    if tall > 1.0 {
        let ventral = closed_quad(
            [l * 0.12, h * 0.85],
            [-l * 0.04, h * (1.1 + 0.5 * tall)],
            [-l * under, h * 0.42 + wag * 0.25],
        );
        pen.fill(&xf.all(&ventral), fin, 0.95);
    }
    // Body, shaded from a darker back to the belly.
    let outline = body(l, h, wag);
    let dark = mix(body_c, BLACK, 0.22);
    let shaded = |y: f32| {
        let v = (y + h) / (2.0 * h);
        if v < 0.45 {
            mix(dark, body_c, v / 0.45)
        } else {
            mix(body_c, belly, (v - 0.45) / 0.55)
        }
    };
    pen.fill(&xf.all(&outline), shaded(h), 1.0);
    for band in (1..8).rev() {
        let y = -h + 2.0 * h * band as f32 / 8.0;
        let cut = super::pen::clip_half(&outline, y);
        pen.fill(&xf.all(&cut), shaded(y - h / 8.0), 1.0);
    }
    let inside = |poly: &[Pt]| xf.all(&clip_convex(poly, &outline));
    let pattern = if fry { Pattern::Plain } else { k.pattern };
    match pattern {
        Pattern::Bands => {
            for bx in [l * 0.22, -l * 0.1] {
                let band = pen.ellipse([bx, 0.0], l * 0.05, h * 1.2, 0.0, 0.0, TAU);
                pen.fill(&inside(&band), mix(WHITE, cx.c.ink, night), 1.0);
                // Its two edges where they cross the body: from the bottom,
                // which is outside, so each edge is one unbroken run.
                let ring = pen.ellipse([bx, 0.0], l * 0.05, h * 1.2, 0.0, PI / 2.0, PI / 2.0 + TAU);
                for run in ring
                    .split(|p| !inside_convex(*p, &outline))
                    .filter(|run| run.len() > 1)
                {
                    pen.stroke(&xf.all(run), false, 1.1 * s, [20.0, 10.0, 10.0], 0.55);
                }
            }
        }
        Pattern::Mask => {
            let mask = pen.ellipse([-l * 0.04, -h * 0.28], l * 0.3, h * 0.38, -0.08, 0.0, TAU);
            pen.fill(&inside(&mask), [8.0, 16.0, 48.0], 0.7);
        }
        Pattern::Lines => {
            for bx in [l * 0.2, l * 0.04, -l * 0.12] {
                let (a, b) = ([bx + h * 0.2, -h * 1.1], [bx - h * 0.2, h * 1.1]);
                let wd = 0.6 * s;
                let strip = [
                    [a[0] - wd, a[1]],
                    [a[0] + wd, a[1]],
                    [b[0] + wd, b[1]],
                    [b[0] - wd, b[1]],
                ];
                pen.fill(&inside(&strip), [70.0, 40.0, 0.0], 0.45);
            }
        }
        Pattern::Spots => {
            for [px, py] in [
                [0.18, -0.3],
                [0.04, -0.45],
                [-0.1, -0.25],
                [0.08, 0.05],
                [-0.18, -0.05],
                [0.24, 0.1],
            ] {
                pen.disc(xf.p([l * px, h * py]), 1.6 * s, WHITE, 0.5);
            }
        }
        Pattern::Plain => {}
    }
    if elder {
        // A slow band of light across the scales.
        let sx = ((t * 0.4 + f.seed) % 1.6 - 0.6) * l;
        for i in 0..5 {
            let u = (i as f32 - 2.0) / 2.5;
            let x = sx + u * l * 0.12;
            let strip = [
                [x - l * 0.025, -h * 1.2],
                [x + l * 0.025, -h * 1.2],
                [x + l * 0.025, h * 1.2],
                [x - l * 0.025, h * 1.2],
            ];
            pen.fill(&inside(&strip), WHITE, 0.35 * (1.0 - u.abs()));
        }
    }
    pen.stroke(&xf.all(&outline), true, 1.0, [0.0, 10.0, 20.0], 0.25);
    // Gill, pectoral fin, eye.
    let gill = pen.ellipse(
        [l * 0.36, 0.0],
        h * 0.62,
        h * 0.62,
        0.0,
        PI * 0.62,
        PI * 1.38,
    );
    pen.stroke(&xf.all(&gill), false, s, BLACK, 0.22);
    let pec = Xf::new([l * 0.14, h * 0.28], 0.5 + (f.phase * 1.3).sin() * 0.4, 1.0);
    let pec_fin = pec.all(&pen.ellipse([-l * 0.06, 0.0], l * 0.1, l * 0.035, 0.0, 0.0, TAU));
    pen.fill(&xf.all(&pec_fin), fin, 0.8);
    let eye = l * if fry { 0.095 } else { 0.065 };
    cx.eye(pen, [l * 0.33, -h * 0.2], eye.max(2.0));
    if nt > 0.05 {
        for i in 0..6 {
            let at = xf.p([
                l * (0.2 - i as f32 * 0.085),
                h * 0.12 + wag * 0.06 * i as f32,
            ]);
            pen.glow(
                at,
                3.0 * s,
                k.fin,
                nt * (0.6 + 0.4 * (t * 3.0 + i as f32 + f.seed).sin()),
            );
        }
    }
    pen.fade = 1.0;
}
