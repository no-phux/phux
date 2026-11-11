//! What moves over the hive's backdrop, in the prototype's draw order:
//! swaying flowers, the bees' own cells filling and capping, light through
//! the honey, dance and light trails, the bees with their auras, sleepy z's
//! over each cluster, drifting pollen and fireflies.

use std::f32::consts::TAU;

use super::super::reef::pen::Pen;
use super::bee::{Look, bee};
use super::comb::{fill_level, lit_cell, rect};
use super::{glow_in, hex};
use crate::hive::{AMBER, Hive, MINT};
use crate::reef::{Mood, Pt, Rgb, lerp, mix};

pub fn flowers(pen: &mut Pen, h: &Hive) {
    let (nt, hh) = (h.nt, h.h);
    for fl in &h.flowers {
        let sw = fl.x - fl.bx;
        let stem = mix([70.0, 130.0, 56.0], [24.0, 60.0, 56.0], nt);
        let a = [fl.bx - sw * 0.4, hh + 4.0];
        let pts: Vec<Pt> = std::iter::once(a)
            .chain(super::super::reef::pen::quad(
                a,
                [fl.bx + sw * 0.2, (fl.y + hh) / 2.0],
                [fl.x, fl.y],
                10,
            ))
            .collect();
        pen.stroke(&pts, false, 2.0, stem, 1.0);
        let leaf = [fl.bx + sw * 0.05 + 6.0, lerp(fl.y, hh, 0.35)];
        let leaf = pen.ellipse(leaf, 7.0, 2.5, -0.5, 0.0, TAU);
        pen.fill(&leaf, mix([88.0, 150.0, 64.0], [26.0, 70.0, 60.0], nt), 1.0);
        let c = mix(fl.c, [130.0, 150.0, 205.0], nt * 0.55);
        if nt > 0.05 {
            pen.glow([fl.x, fl.y], fl.pr * 2.4, [170.0, 190.0, 255.0], 0.18 * nt);
        }
        for k in 0..fl.n {
            let a = k as f32 * TAU / fl.n as f32 + fl.ph;
            let at = [fl.x + a.cos() * fl.pr * 0.55, fl.y + a.sin() * fl.pr * 0.55];
            let petal = pen.ellipse(at, fl.pr * 0.55, fl.pr * 0.28, a, 0.0, TAU);
            let tint = mix(c, [255.0; 3], (k % 2) as f32 * 0.12);
            pen.fill(&petal, tint, 1.0);
        }
        let heart = mix([255.0, 190.0, 40.0], [190.0, 150.0, 60.0], nt);
        pen.disc([fl.x, fl.y], fl.pr * 0.3, heart, 1.0);
    }
}

/// Each bee's own cell: its honey level, its wax cap, their glows.
pub fn home_cells(pen: &mut Pen, h: &Hive) {
    let (nt, t, rad) = (h.nt, h.t, h.r);
    let ri = rad * 0.8;
    for b in &h.bees {
        let c = h.home_of(b);
        let cell = hex(c, ri);
        pen.fill(
            &cell,
            mix([196.0, 134.0, 52.0], [70.0, 44.0, 16.0], nt),
            1.0,
        );
        let top = mix([255.0, 200.0, 64.0], [214.0, 124.0, 22.0], nt);
        let bottom = mix([222.0, 132.0, 18.0], [140.0, 64.0, 8.0], nt);
        fill_level(pen, &cell, c, ri, b.honey, top, bottom, 1.0);
        if b.cap > 0.0 {
            let inner = mix([255.0, 246.0, 210.0], [190.0, 170.0, 120.0], nt);
            let outer = mix([236.0, 204.0, 140.0], [130.0, 104.0, 60.0], nt);
            let f = pen.fade;
            pen.fade = b.cap;
            lit_cell(pen, c, ri, outer, inner, [-0.3, -0.3]);
            pen.fade = f;
        }
        if nt > 0.05 {
            pen.glow(c, rad * 2.0, [255.0, 150.0, 30.0], 0.12 * nt * b.honey);
        }
        if b.cap > 0.05 {
            // Additive light on canvas; stacked discs pile up, so softer.
            let a = 0.25 * b.cap * (0.75 + 0.25 * (t * 3.0 + b.ph).sin());
            pen.glow(c, rad * 2.4, MINT, a);
            pen.stroke(&hex(c, rad * 0.92), true, 1.6, MINT, 0.85 * b.cap);
        }
    }
}

/// Light drifting through the honey, and the warm glow of the comb at night.
pub fn light(pen: &mut Pen, h: &Hive) {
    let (t, nt) = (h.t, h.nt);
    for f in &h.frames {
        let comb = rect(f.ix, f.iy, f.iw, f.ih);
        for k in 0..3 {
            let k = k as f32;
            let bx = f.ix + f.iw * (0.5 + 0.45 * (t * 0.17 + k * 2.1 + f.x).sin());
            let by = f.iy + f.ih * (0.18 + 0.14 * (t * 0.23 + k * 1.7).cos());
            let a = 0.08 * (1.0 - nt * 0.5);
            glow_in(
                pen,
                [bx, by],
                f.iw * 0.32,
                [255.0, 214.0, 120.0],
                a,
                &comb,
                5,
            );
        }
        if nt > 0.02 {
            let c = [f.ix + f.iw / 2.0, f.iy + f.ih * 0.45];
            let a = 0.14 * nt * (0.9 + 0.1 * (t * 1.7 + f.x).sin());
            glow_in(
                pen,
                c,
                f.iw.max(f.ih) * 0.7,
                [255.0, 140.0, 30.0],
                a,
                &comb,
                5,
            );
        }
    }
}

/// A trail in eight pieces, each fading in toward the bee and widening.
fn trail(pen: &mut Pen, tr: &[Pt], w: f32, c: Rgb, a: f32) {
    let n = tr.len();
    let pieces = 8.min(n - 1);
    for p in 0..pieces {
        let (from, to) = (p * (n - 1) / pieces, (p + 1) * (n - 1) / pieces);
        let k = (from + to) as f32 / 2.0 / (n - 1) as f32;
        pen.stroke(&tr[from..=to], false, w * (0.4 + 0.6 * k), c, a * k);
    }
}

/// Amber dance figures always, harness-coloured light trails at night.
pub fn trails(pen: &mut Pen, h: &Hive) {
    let nt = h.nt;
    for b in &h.bees {
        let amber = b.who.mood == Mood::Waiting;
        let a0 = match b.who.mood {
            Mood::Waiting => 0.75,
            Mood::Working => 0.55 * nt,
            _ => 0.0,
        };
        if a0 < 0.02 || b.trail.len() < 2 {
            continue;
        }
        if amber && nt < 0.98 {
            // A dark under-stroke so the dance reads on bright honey by day.
            trail(pen, &b.trail, 4.5, [90.0, 40.0, 0.0], 0.5 * (1.0 - nt));
            trail(pen, &b.trail, 2.4, AMBER, 1.0 - nt);
        }
        let c = if amber { AMBER } else { b.who.species.fin };
        trail(pen, &b.trail, if amber { 2.6 } else { 1.8 }, c, a0);
    }
}

/// The bees, the idle cluster first, each over its mood's aura.
pub fn bees(pen: &mut Pen, h: &Hive) {
    let (t, nt, s) = (h.t, h.nt, h.s);
    for b in h.draw_order() {
        let (o, l, fin) = ([b.x, b.y], b.len(s), b.who.species.fin);
        match b.who.mood {
            Mood::Waiting => {
                let pu = 0.5 + 0.5 * (t * 4.5 + b.ph).sin();
                pen.glow(
                    o,
                    l * 2.2,
                    AMBER,
                    (0.45 + 0.25 * pu) * (0.45 + 0.55 * nt) * 0.8,
                );
                let ring = ((t + b.ph) % 1.4) / 1.4;
                pen.ring(o, l * (0.6 + ring * 1.8), 2.0, AMBER, 0.8 * (1.0 - ring));
                if b.burst > 0.0 {
                    pen.ring(o, l * (3.2 - b.burst * 2.4), 3.0, AMBER, b.burst);
                }
            }
            Mood::Done => {
                pen.glow(o, l * 1.5, MINT, 0.32);
                if (t * 2.0 + b.ph).sin() > 0.92 {
                    let sa = t * 3.0 + b.ph;
                    let at = [o[0] + sa.cos() * l * 0.8, o[1] + sa.sin() * l * 0.6];
                    pen.disc(at, 1.6, MINT, 0.9);
                }
            }
            Mood::Working if nt > 0.05 => pen.glow(o, l * 1.3, fin, 0.3 * nt),
            _ => {}
        }
        let idle = b.who.mood == Mood::Idle;
        let look = Look {
            flap: b.flap,
            still: idle,
            desat: if idle { 0.55 } else { 0.0 },
            pollen: b.pollen && b.who.mood == Mood::Working,
            tier: b.who.tier,
        };
        bee(pen, o, b.ang, l, b.who.species, &look);
    }
}

/// Sleepy z's rising over each frame's idle cluster.
pub fn sleepers(pen: &mut Pen, h: &Hive) {
    let l0 = 22.0 * h.s;
    for (i, f) in h.frames.iter().enumerate() {
        if !h
            .bees
            .iter()
            .any(|b| b.who.host == i && b.who.mood == Mood::Idle)
        {
            continue;
        }
        for k in 0..2 {
            let p = (h.t * 0.35 + k as f32 * 0.5 + f.x * 0.01) % 1.0;
            let sz = 9.0 + p * 6.0;
            let x = f.idle[0] + l0 * 0.8 + p * 10.0 + k as f32 * 6.0;
            let y = f.idle[1] - l0 * 0.6 - p * 22.0;
            let (w, hz) = (sz * 0.5, sz * 0.32);
            let z = [[x, y - hz], [x + w, y - hz], [x, y + hz], [x + w, y + hz]];
            pen.stroke(&z, false, sz * 0.13, [255.0, 248.0, 230.0], 0.7 * (1.0 - p));
        }
    }
}

pub fn drift(pen: &mut Pen, h: &Hive) {
    let (t, nt) = (h.t, h.nt);
    let pollen = mix([255.0, 214.0, 90.0], [255.0, 170.0, 60.0], nt);
    for m in &h.motes {
        let a = (0.35 + 0.3 * (t + m.ph).sin()) * (1.0 - nt * 0.4);
        pen.disc([m.x, m.y], m.s, pollen, a);
    }
    if nt > 0.02 {
        for f in &h.flies {
            let bl = (t * 1.6 * f.sp + f.ph * 3.0).sin().max(0.0).powi(3) * nt;
            if bl > 0.02 {
                pen.glow([f.x, f.y], 10.0, [210.0, 255.0, 120.0], 0.6 * bl);
                pen.disc([f.x, f.y], 1.4, [240.0, 255.0, 190.0], bl);
            }
        }
    }
}
