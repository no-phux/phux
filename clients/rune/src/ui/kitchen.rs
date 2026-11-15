//! The kitchen: the fleet as a restaurant, each machine a line with its
//! sign, every agent it can reach a chef in its harness's colours acting out
//! its state. A press on a chef selects it; a press on the selected one (or
//! Return) shows its terminal, as the galaxy does. Escape lets go, then
//! closes. Night follows a dark theme: the lights go down to warm pools.
//!
//! Drawn back to front as the prototype: the room once per layout in one
//! canvas, the stations, pass, chefs, air and night in another, then the
//! line signs, the neon, name pills, the chefs' press targets and the readout.

use rune::{Cx, El, TextStyle, canvas, el, row, text};

use super::playground::{self, Who, badge, pill_label, pill_w, place_pills, stage, target};
use super::reef::pen::{Pen, rgba};
use super::reef::{Label, pill};
use super::{CAPTION, Ink};
use crate::creature;
use crate::desk::Desk;
use crate::kitchen::{Goal, Kitchen, PartKind, RANK};
use crate::reef::{Mood, Pt};

mod chef;
mod room;

use chef::{AMBER, MINT, pool};

/// The night's darkness is worked out on a grid this many points across.
const CELL: f32 = 4.0;

pub(super) fn view(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> El {
    let k = &desk.kitchen.scene;
    let (root, waiting) = stage(&desk.kitchen, "kitchen", "Kitchen", k.lines.len(), p, cx);
    if k.lines.is_empty() {
        return root.child(readout(k, waiting, p));
    }
    let room = {
        let mut cache = desk.kitchen.paint.room.borrow_mut();
        if cache.0 != k.generation {
            *cache = (k.generation, room::room(k));
        }
        cache.1.clone()
    };
    let mut pen = Pen::new();
    scene(&mut pen, k);
    let mut over = Pen::new();
    glows(&mut over, k);
    let dark = (k.nt > 0.02).then(|| {
        let (w, h, rgba) = night(k);
        desk.kitchen.paint.mask.set(w, h, rgba);
        el().absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .pixels(&desk.kitchen.paint.mask)
    });
    let targets = k.chefs.iter().map(|c| {
        let (at, r) = k.hit(c);
        let rad = r.max(10.0) + 8.0;
        target::<Kitchen>("kitchen-chef", &who(k, c), at, rad)
    });
    root.child(
        canvas(room)
            .key("kitchen-room")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full(),
    )
    .child(
        canvas(pen.into_shape())
            .key("kitchen-front")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .on_press(Desk::scene_deselect::<Kitchen>),
    )
    .children(dark)
    .child(
        canvas(over.into_shape())
            .key("kitchen-over")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full(),
    )
    .children(
        k.lines
            .iter()
            .enumerate()
            .map(|(i, l)| sign(k, i, [l.x0 + 2.0, l.y0])),
    )
    .child(neon(k))
    .children(pills(k))
    .children(targets)
    .child(readout(k, waiting, p))
}

/// Everything that moves, in the prototype's order.
fn scene(pen: &mut Pen, k: &Kitchen) {
    let (t, cs) = (k.t, k.cs);
    for c in k
        .chefs
        .iter()
        .filter(|c| c.arrived() && c.who.mood == Mood::Working)
    {
        if let Goal::Station(s) = c.goal {
            chef::prop(pen, k, k.line_of(c), c, s);
        }
    }
    for (i, l) in k.lines.iter().enumerate() {
        chef::pass(pen, k, l, i);
    }
    let pulse = 0.6 + 0.4 * (t * 5.0).sin();
    for c in k.chefs.iter().filter(|c| c.who.mood == Mood::Waiting) {
        pool(pen, [c.x, c.y], 26.0 * cs, 11.0 * cs, AMBER, 0.5 * pulse);
    }
    let mut order: Vec<_> = k.chefs.iter().collect();
    order.sort_by(|a, b| a.y.total_cmp(&b.y));
    for c in order {
        chef::chef(pen, k, c);
    }
    chef::parts(pen, &k.parts);
}

/// What shines over the dark: warm lamp pools, the bells' glow, the neon's
/// halo, and the selection ring over it all.
fn glows(pen: &mut Pen, k: &Kitchen) {
    let (t, cs, nt) = (k.t, k.cs, k.nt);
    if nt > 0.02 {
        for l in &k.lines {
            for &lamp in &l.lamps {
                pen.glow(lamp, 90.0 * cs, [255.0, 170.0, 80.0], 0.16 * nt);
            }
        }
        for c in k.chefs.iter().filter(|c| c.who.mood == Mood::Waiting) {
            pen.glow([c.x, c.y - 16.0], 40.0 * cs, AMBER, 0.3 * nt);
        }
    }
    let on = neon_on(k);
    pen.glow(k.neon, 40.0, [255.0, 70.0, 120.0], 0.45 * on);
    if let Some(c) = k.selected_chef() {
        // The selection ring, its dashes turning.
        let (at, r) = k.hit(c);
        let rad = r + 7.0;
        let per = 9.0 / rad;
        let n = (std::f32::consts::TAU / per).floor() as usize;
        let shift = (t * 20.0 / rad) % per;
        for d in 0..n {
            let a = d as f32 * per + shift;
            pen.stroke(
                &room::arc(at, rad, a, a + 5.0 / rad),
                false,
                2.0,
                room::WHITE,
                1.0,
            );
        }
    }
}

/// How lit the neon is: dim by day, flickering now and then.
fn neon_on(k: &Kitchen) -> f32 {
    let flicker = (k.t * 31.0).sin() > 0.96 && (k.t * 7.0).sin() > 0.0;
    (0.25 + 0.75 * k.nt) * if flicker { 0.35 } else { 1.0 }
}

/// The dark, with clear pools where the lamps, the pass and the chefs are,
/// as the prototype's mask canvas: each light takes off its share of what's
/// left. A small image the GPU stretches smooth: width, height, RGBA.
fn night(k: &Kitchen) -> (u32, u32, Vec<u8>) {
    let cs = k.cs;
    let mut lit: Vec<(Pt, f32, f32)> = Vec::new();
    for l in &k.lines {
        lit.extend(l.lamps.iter().map(|&at| (at, 95.0 * cs, 0.85)));
        lit.push((
            [(l.x0 + l.brk + l.x1) / 2.0, l.py0 + 8.0],
            (l.x1 - l.x0 - l.brk) * 0.55,
            0.75,
        ));
        lit.push(([l.x0 + l.brk * 0.4, l.y1 - 20.0], 50.0 * cs, 0.35));
    }
    for c in &k.chefs {
        match c.who.mood {
            Mood::Waiting => lit.push(([c.x, c.y - 14.0], 48.0 * cs, 0.95)),
            Mood::Done => lit.push(([c.x, c.y], 34.0 * cs, 0.7)),
            Mood::Working if c.arrived() => {
                if let Goal::Station(s) = c.goal {
                    let st = &k.line_of(c).work[s];
                    if st.kind == 0 || st.kind == 2 {
                        lit.push(([st.x, st.iy], 34.0 * cs, 0.8));
                    }
                }
            }
            _ => {}
        }
    }
    lit.extend(
        k.parts
            .iter()
            .filter(|p| p.kind == PartKind::Flame)
            .map(|p| ([p.x, p.y], p.r * 4.0, 0.5)),
    );
    lit.push((k.neon, 70.0, 0.8));
    let (nx, ny) = ((k.w / CELL).ceil() as usize, (k.h / CELL).ceil() as usize);
    let mut left = vec![1.0f32; nx * ny];
    for (at, r, a) in lit {
        let (i0, i1) = (
            ((at[0] - r) / CELL).floor().max(0.0) as usize,
            (((at[0] + r) / CELL).ceil() as usize).min(nx),
        );
        let (j0, j1) = (
            ((at[1] - r) / CELL).floor().max(0.0) as usize,
            (((at[1] + r) / CELL).ceil() as usize).min(ny),
        );
        for j in j0..j1 {
            for i in i0..i1 {
                let d = ((i as f32 + 0.5) * CELL - at[0]).hypot((j as f32 + 0.5) * CELL - at[1]);
                if d < r {
                    left[j * nx + i] *= 1.0 - a * (1.0 - d / r);
                }
            }
        }
    }
    let dark = 0.78 * k.nt;
    let rgba = left
        .iter()
        .flat_map(|v| [8, 8, 24, (v * dark * 255.0).round() as u8])
        .collect();
    (nx as u32, ny as u32, rgba)
}

/// A line's sign: its machine, a lamp, and how many there need you.
fn sign(k: &Kitchen, i: usize, [x, y]: Pt) -> El {
    let l = &k.lines[i];
    let waiting = k.waiting_in(i);
    let lamp = if l.known {
        rgba(MINT, 0.8)
    } else {
        rgba([150.0, 156.0, 160.0], 0.6)
    };
    el().absolute(x - 2.0, y - 2.0)
        .px(2.0)
        .py(2.0)
        .radius(4.0)
        .fill(rgba([122.0, 84.0, 52.0], 1.0))
        .child(
            row()
                .h(19.0)
                .px(8.0)
                .gap(6.0)
                .radius(3.0)
                .items_center()
                .fill(rgba([38.0, 48.0, 44.0], 1.0))
                .child(crate::host_color::dot(
                    &l.host,
                    7.0,
                    rgba([38.0, 48.0, 44.0], 1.0),
                ))
                .child(
                    text(l.host.to_uppercase())
                        .font(TextStyle::new(10.5, 600.0))
                        .color(rgba([242.0, 239.0, 228.0], 1.0)),
                )
                .child(el().size(5.0, 5.0).radius(2.5).fill(lamp))
                .children((waiting > 0).then(|| badge(waiting, 15.0, CAPTION))),
        )
}

/// The OPEN sign on the back wall, brighter at night.
fn neon(k: &Kitchen) -> El {
    let on = neon_on(k);
    let ink = [255.0, 130.0 + 90.0 * on, 170.0 + 60.0 * on];
    let [x, y] = k.neon;
    el().absolute(x - 34.0, y - 13.0)
        .size(68.0, 26.0)
        .center()
        .child(
            text("OPEN")
                .font(TextStyle::new(18.0, 800.0))
                .color(rgba(ink, 0.45 + 0.55 * on)),
        )
}

/// Name pills: always for a chef that needs you, else for the selected or
/// hovered one; stacked upward from the lowest so neighbours stay readable.
fn pills(k: &Kitchen) -> Vec<El> {
    let mut shown: Vec<_> = k
        .chefs
        .iter()
        .filter(|c| {
            c.who.mood == Mood::Waiting
                || k.selected.as_ref() == Some(&c.who.id)
                || k.hovered.as_ref() == Some(&c.who.id)
        })
        .collect();
    shown.sort_by(|a, b| b.y.total_cmp(&a.y));
    let items: Vec<(String, bool, Pt, f32)> = shown
        .iter()
        .map(|c| {
            let (text, amber) = pill_label(&c.who.name, c.who.mood);
            let (at, r) = k.hit(c);
            // As `ui::reef::pill` estimates and clamps its width.
            let w = pill_w(&text, 6.6);
            (text, amber, [at[0], at[1] - r - 14.0], w)
        })
        .collect();
    let spots = place_pills(&items.iter().map(|i| (i.2, i.3)).collect::<Vec<_>>(), k.w);
    items
        .into_iter()
        .zip(spots)
        .map(|((text, amber, at, _), [_, y])| {
            let label = Label {
                at: [at[0], y],
                text,
                badge: None,
                amber,
                host: None,
            };
            pill(&label, [at[0], y], k.w)
        })
        .collect()
}

fn who<'a>(k: &'a Kitchen, c: &'a crate::kitchen::Chef) -> Who<'a> {
    Who {
        id: &c.who.id,
        name: &c.who.name,
        mood: c.who.mood,
        host: &k.line_of(c).host,
    }
}

fn readout(k: &Kitchen, waiting: usize, p: &Ink) -> El {
    let picked = k.selected_chef().map(|c| {
        let rank = match c.who.work {
            Some(w) => format!("Rank: {} · Work: {}", RANK[c.tier], creature::work_line(&w)),
            None => format!("Rank: {}", RANK[c.tier]),
        };
        (who(k, c), Some(rank))
    });
    playground::readout("kitchen", k, [k.lines.len(), waiting], k.h, picked, p)
}
