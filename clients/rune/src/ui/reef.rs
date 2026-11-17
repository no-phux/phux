// style-literals: illustration
//! The reef: the fleet as an aquarium, each machine a reef with its buoy,
//! every machine's agents it can reach as fish of their harness's species
//! acting out their state. A press on a fish selects it and the camera follows it
//! in; a press on the selected one (or Return) shows its terminal, as the
//! galaxy does. Escape lets go, then closes. Night follows a dark theme.
//!
//! Drawn back to front as the prototype: scenery under the sand's caustics
//! in one canvas, the caustics as a scrolling tile, the rest in another,
//! then labels, the fish's press targets and the readout.

use rune::{Cx, El, canvas, el};

use super::playground::hud::{self, Picked, Readout};
use super::playground::labels::{Label, pill};
use super::playground::pen::Pen;
use super::{Ink, Tokens};
use crate::creature;
use crate::desk::Desk;
use crate::playground::Mood;
use crate::reef::{Reef, TILE};

mod drift;
mod fish;
mod forms;
mod scene;

impl Pen {
    /// Reef's camera with parallax `p` (1 is the scene itself).
    fn set(&mut self, reef: &Reef, p: f32) {
        let z = 1.0 + (reef.cam.z - 1.0) * p;
        self.view(z, reef.to_screen(p)([0.0, 0.0]));
    }
}

pub(super) fn view(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> El {
    let t: &Tokens = &p.tokens;
    desk.reef.night.set(p.bg.luminance() < 0.5);
    desk.reef.still.set(cx.is_reduced_motion());
    let r = &desk.reef.scene;
    let count = |m: Mood| r.fish.iter().filter(|f| f.who.mood == m).count();
    let waiting = count(Mood::Waiting);
    let summary = format!(
        "Reef with {} agents on {} hosts: {} working, {waiting} need you, {} done, {} idle",
        r.fish.len(),
        r.zones.len(),
        count(Mood::Working),
        count(Mood::Done),
        count(Mood::Idle),
    );
    let root = el()
        .key("reef")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .fill(p.bg)
        .modal(false, Desk::close_reef)
        .label(summary)
        .on_layout(|desk: &mut Desk, rect| desk.reef_laid_out(rect[2], rect[3]));
    if r.zones.is_empty() {
        return root.child(foot(r, waiting, t));
    }
    let pal = r.pal();

    let mut back = Pen::new();
    back.set(r, 1.0);
    scene::water(&mut back, r, &pal);
    back.set(r, 0.45);
    scene::far(&mut back, r, &pal);
    back.set(r, 1.0);
    scene::rays(&mut back, r);
    scene::sky(&mut back, r, &pal);
    for k in r.kelp.iter().filter(|k| k.back) {
        scene::kelp(&mut back, r, &pal, k, 0.7);
    }
    scene::sand(&mut back, r, &pal);

    let mut front = Pen::new();
    front.set(r, 1.0);
    let mut labels: Vec<Label> = r
        .zones
        .iter()
        .enumerate()
        .map(|(i, z)| scene::buoy(&mut front, r, z, i))
        .collect();
    for k in r.kelp.iter().filter(|k| !k.back && !k.front) {
        scene::kelp(&mut front, r, &pal, k, 1.0);
    }
    for z in &r.zones {
        scene::reef_back(&mut front, r, &pal, z);
    }
    for z in &r.zones {
        scene::reef_front(&mut front, r, &pal, z);
    }
    drift::sparks(&mut front, r);
    for f in &r.fish {
        fish::fish(&mut front, r, &pal, f, &mut labels);
    }
    drift::bubbles(&mut front, r);
    drift::snow(&mut front, r);
    for k in r.kelp.iter().filter(|k| k.front) {
        scene::kelp(&mut front, r, &pal, k, 1.0);
    }
    drift::vignette(&mut front, r);

    let to = r.to_screen(1.0);
    let targets = r.fish.iter().map(|f| {
        let at = to([f.x, f.y]);
        let rad = (f.len(r.s) * 0.75 + 6.0) * r.cam.z;
        let host = &r.zone_of(f).host;
        let (press, hover) = (f.who.id.clone(), f.who.id.clone());
        el().key(format!("reef-fish-{:?}", f.who.id))
            .absolute(at[0] - rad, at[1] - rad)
            .size(2.0 * rad, 2.0 * rad)
            .radius(rad)
            .cursor(rune::Cursor::Pointer)
            .label(format!("{}, {}, {host}", f.who.name, f.who.mood.word()))
            .on_press(move |desk: &mut Desk| desk.reef_press(press.clone()))
            .on_hover_change(move |desk: &mut Desk, on, _| desk.reef_hover(hover.clone(), on))
    });

    root.child(
        canvas(back.into_shape())
            .key("reef-back")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full(),
    )
    .children(caustics(desk, r))
    .child(
        canvas(front.into_shape())
            .key("reef-front")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .on_press(Desk::reef_deselect),
    )
    .children(labels.iter().map(|l| pill(t, l, to(l.at), r.w)))
    .children(targets)
    .child(foot(r, waiting, t))
}

/// The prototype's two scrolling layers of caustic light on the sand, cut
/// at the sand's lowest dip.
fn caustics(desk: &Desk, r: &Reef) -> Vec<El> {
    let to = r.to_screen(1.0);
    let z = r.cam.z;
    let top = to([0.0, r.floor_y + 10.5 * r.s])[1].max(0.0);
    let h = r.h - top;
    if h <= 0.0 {
        return Vec::new();
    }
    let tile = TILE as f32;
    let base = r.floor_y - 12.0 * r.s;
    [(13.0, 6.0, 1.0, 0.3), (-9.0, 10.0, 1.37, 0.2)]
        .into_iter()
        .map(|(vx, vy, scale, alpha): (f32, f32, f32, f32)| {
            let (sx, sy) = (scale * r.s, scale * r.s * 0.42);
            let (tw, th) = (tile * sx * z, tile * sy * z);
            let o = to([((r.t * vx) % tile) * sx, base + ((r.t * vy) % tile) * sy]);
            let (x0, y0) = (o[0].rem_euclid(tw) - tw, (o[1] - top).rem_euclid(th) - th);
            let mut tiles = Vec::new();
            let mut y = y0;
            while y < h {
                let mut x = x0;
                while x < r.w {
                    tiles.push(el().absolute(x, y).size(tw, th).pixels(&desk.reef.caustic));
                    x += tw;
                }
                y += th;
            }
            el().absolute(0.0, top)
                .w(r.w)
                .h(h)
                .clip_rounded(0.0)
                .opacity(alpha * (1.0 - r.nt * 0.9))
                .children(tiles)
        })
        .collect()
}

/// The readout's counts and the picked fish's card.
fn foot(r: &Reef, waiting: usize, t: &Tokens) -> El {
    let working = (r.fish.iter())
        .filter(|f| f.who.mood == Mood::Working)
        .count();
    let picked = r.selected_fish().map(|f| Picked {
        name: f.who.name.clone(),
        mood: f.who.mood,
        host: r.zone_of(f).host.clone(),
        // Its form and work, when its machine publishes agent stats.
        card: f.who.work.map(|w| {
            let form = creature::form_name(f.tier, f.who.species.form);
            format!("Form: {form} · Work: {}", creature::work_line(&w))
        }),
        open: Desk::open_fish,
    });
    hud::readout(
        t,
        Readout {
            key: "reef",
            h: r.h,
            hosts: r.zones.len(),
            agents: r.fish.len(),
            working,
            waiting,
            next: Desk::reef_next_waiting,
            close: Desk::close_reef,
            picked,
        },
    )
}
