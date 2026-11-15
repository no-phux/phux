//! The hive's painter (the scene is `crate::hive`), back to front as the
//! prototype: the still backdrop (painted once per layout, day and night)
//! stretched through the camera; flowers, the bees' cells and the comb's
//! light; the plaques; trails, bees, drift and the vignette; then the
//! selection ring, name pills, the bees' press targets and the readout.

use std::f32::consts::TAU;

use rune::{Cx, El, TextStyle, canvas, el, row, text};

use super::playground::{
    self, Who, badge, name_pill, pill_label, pill_w, place_pills, stage, target,
};
use super::reef::drift::edges;
use super::reef::pen::{Pen, clip_convex};
use super::reef::rgba;
use super::{CAPTION, Ink};
use crate::creature;
use crate::desk::Desk;
use crate::hive::{Hive, RANKS};
use crate::reef::{Mood, Pt, Rgb, mix};
use crate::ui::Cover;

mod bee;
mod comb;
mod scene;

/// A hexagonal cell's outline, point up.
fn hex(c: Pt, r: f32) -> Vec<Pt> {
    (0..6)
        .map(|k| {
            let a = -TAU / 4.0 + k as f32 * TAU / 6.0;
            [c[0] + a.cos() * r, c[1] + a.sin() * r]
        })
        .collect()
}

/// A glow in `n` stacked discs, cut to the convex `clip`. Sixteen sides:
/// Rune fills each fan triangle's box, so a big many-sided disc costs the
/// GPU many times its area, and a faint soft one shows no corners.
fn glow_in(pen: &mut Pen, c: Pt, r: f32, col: Rgb, a: f32, clip: &[Pt], n: usize) {
    if a < 0.01 {
        return;
    }
    for k in 0..n {
        let rk = r * (1.0 - k as f32 / n as f32);
        let disc: Vec<Pt> = (0..16)
            .map(|i| {
                let (s, co) = (i as f32 * TAU / 16.0).sin_cos();
                [c[0] + co * rk, c[1] + s * rk]
            })
            .collect();
        pen.fill(&clip_convex(&disc, clip), col, a * 0.9 / n as f32);
    }
}

/// A horizontal gradient over a rectangle in `n` columns, each reaching a
/// point into the next. `at(k)` is the colour `k` of the way across.
fn hgrad(pen: &mut Pen, x: f32, y: f32, w: f32, h: f32, n: usize, at: impl Fn(f32) -> (Rgb, f32)) {
    let band = w / n as f32;
    for k in 0..n {
        let (c, a) = at((k as f32 + 0.5) / n as f32);
        let bw = if k + 1 == n { band } else { band + 1.0 };
        pen.rect(x + band * k as f32, y, bw, h, c, a);
    }
}

pub(super) fn view(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> El {
    let h = &desk.hive.scene;
    let (root, waiting) = stage(&desk.hive, "hive", "Hive", h.frames.len(), p, cx);
    let root = root.clip_rounded(0.0);
    if h.frames.is_empty() {
        return root.child(readout(h, waiting, p));
    }
    let (z, o) = h.view();
    let backdrop = {
        let mut cache = desk.hive.paint.borrow_mut();
        if cache.as_ref().is_none_or(|(g, ..)| *g != h.generation) {
            *cache = Some((h.generation, comb::paint(h, false), comb::paint(h, true)));
        }
        let (_, day, night) = cache.as_ref().expect("painted above");
        let stretch = |shape: &rune::Shape, key: &str| {
            canvas(shape.clone())
                .key(key.to_owned())
                .absolute(o[0], o[1])
                .size(h.w * z, h.h * z)
                .cache()
        };

        // Full day or full night is one layer; only the dusk between blends.
        let mut layers = Vec::new();
        if h.nt < 0.99 {
            layers.push(stretch(day, "hive-day"));
        }
        if h.nt >= 0.99 {
            layers.push(stretch(night, "hive-night"));
        } else if h.nt > 0.01 {
            layers.push(stretch(night, "hive-night").opacity(h.nt));
        }
        layers
    };

    let mut mid = Pen::new();
    mid.view(z, o);
    scene::flowers(&mut mid, h);
    scene::home_cells(&mut mid, h);
    scene::light(&mut mid, h);

    let mut front = Pen::new();
    front.view(z, o);
    scene::trails(&mut front, h);
    scene::bees(&mut front, h);
    scene::sleepers(&mut front, h);
    scene::drift(&mut front, h);
    let (w, hh) = (h.w, h.h);
    let a = 0.25 + h.nt * 0.25;
    edges(
        &mut front,
        [w, hh],
        hh / 2.0,
        [w.min(hh) * 0.4, w.max(hh) * 0.8],
        [20.0, 8.0, 0.0],
        a,
    );
    if let Some(b) = h.selected_bee() {
        ring(
            &mut front,
            h.to_screen([b.x, b.y]),
            b.len(h.s) * 0.6 * z + 7.0,
            h.t,
        );
    }

    let targets = h.bees.iter().map(|b| {
        let at = h.to_screen([b.x, b.y]);
        let rad = (b.len(h.s) * 0.6 * z).max(10.0) + 8.0;
        target::<Hive>("hive-bee", &who(h, b), at, rad)
    });

    root.children(backdrop)
        .child(canvas(mid.into_shape()).key("hive-mid").cover())
        .children((0..h.frames.len()).map(|i| plaque(h, i)))
        .child(
            canvas(front.into_shape())
                .key("hive-front")
                .cover()
                .on_press(Desk::scene_deselect::<Hive>),
        )
        .children(pills(h))
        .children(targets)
        .child(readout(h, waiting, p))
}

/// The shell's selection ring: white dashes turning round the bee, in
/// screen points.
fn ring(pen: &mut Pen, at: Pt, rad: f32, t: f32) {
    pen.screen();
    let n = ((TAU * rad / 9.0) as usize).max(6);
    let step = TAU / n as f32;
    let turn = -t * 20.0 / rad;
    for d in 0..n {
        let from = turn + d as f32 * step;
        let dash = pen.ellipse(at, rad, rad, 0.0, from, from + step * 5.0 / 9.0);
        pen.stroke(&dash, false, 2.0, [255.0; 3], 1.0);
    }
}

/// A frame's stamped plaque on its top bar: the host and how many of its
/// agents need you.
fn plaque(h: &Hive, i: usize) -> El {
    let f = &h.frames[i];
    let (z, nt) = (h.cam.z, h.nt);
    let at = h.to_screen([f.x + f.w / 2.0, f.y + f.bar / 2.0]);
    let bh = (f.bar - 6.0).min(22.0) * z;
    let size = (10.0 + 2.0 * h.s).round() * z;
    let ink = rgba([70.0, 34.0, 8.0], 0.9);
    let wood = rgba(mix([238.0, 206.0, 156.0], [120.0, 92.0, 60.0], nt), 1.0);
    let nail = || {
        el().size(2.6 * z, 2.6 * z)
            .radius(1.3 * z)
            .fill(rgba([70.0, 34.0, 8.0], 0.6))
    };
    let w8 = h.waiting_in(i);
    let pulse = 1.0 + 0.08 * (h.t * 5.0).sin();
    let badge = (w8 > 0).then(|| {
        let d = bh - 4.0 * z;
        badge(w8, d, TextStyle::new(size, 600.0)).scale(pulse)
    });
    row()
        .absolute(at[0] - 300.0, at[1] - bh / 2.0)
        .w(600.0)
        .justify_center()
        .child(
            row()
                .h(bh)
                .px(3.0 * z)
                .gap(5.0 * z)
                .radius(3.0 * z)
                .items_center()
                .fill(wood)
                .border(1.0, rgba([80.0, 40.0, 10.0], 0.55))
                .child(nail())
                .child(crate::host_color::dot(&f.host, 7.0 * z, wood))
                .child(
                    text(f.host.to_uppercase())
                        .font(TextStyle::new(size, 600.0))
                        .color(ink),
                )
                .children(badge)
                .child(nail()),
        )
}

/// Name pills: always for a bee that needs you, and for the selected and
/// hovered ones; stacked upward so none overlap.
fn pills(h: &Hive) -> Vec<El> {
    let z = h.cam.z;
    let mut shown: Vec<(Pt, String, bool)> = h
        .bees
        .iter()
        .filter(|b| {
            b.who.mood == Mood::Waiting
                || h.selected.as_ref() == Some(&b.who.id)
                || h.hovered.as_ref() == Some(&b.who.id)
        })
        .map(|b| {
            let at = h.to_screen([b.x, b.y]);
            let r = b.len(h.s) * 0.6 * z;
            let (label, amber) = pill_label(&b.who.name, b.who.mood);
            ([at[0], at[1] - r - 14.0], label, amber)
        })
        .collect();
    shown.sort_by(|a, b| b.0[1].total_cmp(&a.0[1]));
    let spots: Vec<(Pt, f32)> = shown
        .iter()
        .map(|(at, l, _)| (*at, pill_w(l, 6.6)))
        .collect();
    place_pills(&spots, h.w)
        .into_iter()
        .zip(spots)
        .zip(shown)
        .map(|(([x, y], (_, w)), (_, label, amber))| {
            name_pill(label, amber, CAPTION)
                .absolute(x, y - 9.5)
                .w(w)
                .justify_center()
        })
        .collect()
}

fn who<'a>(h: &'a Hive, b: &'a crate::hive::Bee) -> Who<'a> {
    Who {
        id: &b.who.id,
        name: &b.who.name,
        mood: b.who.mood,
        host: &h.frame_of(b).host,
    }
}

fn readout(h: &Hive, waiting: usize, p: &Ink) -> El {
    // Its rank and work, when its machine publishes agent stats.
    let picked = h.selected_bee().map(|b| {
        let card = b.who.work.map(|w| {
            let rank = RANKS[b.who.tier.min(4)];
            format!("Rank: {rank} · Work: {}", creature::work_line(&w))
        });
        (who(h, b), card)
    });
    playground::readout("hive", h, [h.frames.len(), waiting], h.h, picked, p)
}
