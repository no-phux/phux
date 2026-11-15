//! The pit lane: the fleet as a race circuit, each machine a team garage on
//! the pit lane, every machine's agents it can reach as race cars in their
//! harness's livery acting out their state. A press on a car selects it; a
//! press on the selected one (or Return) shows its terminal, as the galaxy
//! does. Escape lets go, then closes. Night follows a dark theme.
//!
//! Drawn back to front as the prototype: the grounds, drawn once per layout
//! by day and by night (night faded in over day), the cars and the sky in
//! one canvas, then the garages' boards, name pills, the cars' press targets
//! and the readout. The camera stays put: a car laps too fast to follow.

use rune::{Cx, El, canvas, row, text};

use super::playground::{
    self, Who, badge, name_pill, pill_label, pill_w, place_pills, stage, target,
};
use super::reef::pen::{Pen, rgba};
use super::{CAPTION, Ink};
use crate::creature;
use crate::desk::Desk;
use crate::pitlane::{AMBER, Mode, PitLane, RANKS};
use crate::reef::{Mood, Pt};
use crate::ui::Cover;

mod car;
mod ground;

const PILL_H: f32 = 19.0;

pub(super) fn view(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> El {
    let r = &desk.pitlane.scene;
    let (root, waiting) = stage(&desk.pitlane, "pitlane", "Pit Lane", r.garages.len(), p, cx);
    if r.trk.is_empty() {
        return root.child(readout(r, waiting, p));
    }
    let (day, night) = grounds(desk, r);

    let mut pen = Pen::new();
    let mut order: Vec<_> = r.cars.iter().filter(|c| c.placed).collect();
    // Cars in the pit lane under those racing past.
    order.sort_by_key(|c| c.mode == Mode::Track);
    for c in &order {
        car::car(&mut pen, r, c);
        if c.landed && c.who.mood == Mood::Waiting {
            let team = r.garages.get(r.host_of(c)).map_or(AMBER, |g| g.c);
            car::crew(&mut pen, r, c, team);
        }
        if c.landed && c.who.mood == Mood::Done {
            car::flag(&mut pen, r, c);
        }
    }
    if let Some(c) = r.selected_car() {
        car::ring(&mut pen, r, c);
    }
    car::sky(&mut pen, r);

    let targets = order.iter().map(|c| {
        let rad = c.r(r.k).max(10.0) + 8.0;
        target::<PitLane>("pitlane-car", &who(r, c), [c.x, c.y], rad)
    });

    let full = |shape: rune::Shape, key: &str| canvas(shape).key(key).cover();
    root.children((r.nt < 0.99).then(|| full(day, "pitlane-day")))
        .children((r.nt > 0.01).then(|| {
            let night = full(night, "pitlane-night");
            if r.nt < 0.99 {
                night.opacity(r.nt)
            } else {
                night
            }
        }))
        .child(full(pen.into_shape(), "pitlane-cars").on_press(Desk::scene_deselect::<PitLane>))
        .children(r.garages.iter().enumerate().map(|(i, _)| board(r, i)))
        .children(pills(r))
        .children(targets)
        .child(readout(r, waiting, p))
}

/// The grounds by day and by night, redrawn only when the layout changes.
fn grounds(desk: &Desk, r: &PitLane) -> (rune::Shape, rune::Shape) {
    let mut cache = desk.pitlane.paint.borrow_mut();
    if cache.0 != r.rev {
        *cache = (r.rev, ground::paint(r, false), ground::paint(r, true));
    }
    (cache.1.clone(), cache.2.clone())
}

/// A garage's board under it: the machine's name in its team colour, in
/// whichever ink reads on it, and the amber count of its agents that need you.
fn board(r: &PitLane, i: usize) -> El {
    let g = &r.garages[i];
    let waiting = r.waiting_in(i);
    let at = r.to_s(
        (g.u0 + g.u1) / 2.0,
        0.43 + if r.portrait { 0.07 } else { 0.035 },
    );
    let ink = crate::theme::on_fill(rgba(g.c, 1.0));
    row()
        .absolute(at[0] - 160.0, at[1] - PILL_H / 2.0)
        .w(320.0)
        .justify_center()
        .child(
            row()
                .h(PILL_H)
                .px(9.0)
                .gap(6.0)
                .radius(4.0)
                .items_center()
                .fill(rgba(g.c, 1.0))
                .child(text(g.host.to_uppercase()).font(CAPTION).color(ink))
                .children((waiting > 0).then(|| badge(waiting, 15.0, CAPTION).border(1.0, ink))),
        )
}

/// Name pills over the cars that need you, the selected one and the
/// hovered one, stacked so none overlaps.
fn pills(r: &PitLane) -> Vec<El> {
    let mut shown: Vec<_> = r
        .cars
        .iter()
        .filter(|c| c.placed)
        .filter(|c| {
            c.who.mood == Mood::Waiting
                || r.selected.as_ref() == Some(&c.who.id)
                || r.hovered.as_ref() == Some(&c.who.id)
        })
        .collect();
    // Bottom-most first, so stacks grow upward from the cars.
    shown.sort_by(|a, b| b.y.total_cmp(&a.y));
    let items: Vec<(String, bool, Pt, f32)> = shown
        .iter()
        .map(|c| {
            let (text, amber) = pill_label(&c.who.name, c.who.mood);
            let w = pill_w(&text, 5.6);
            (text, amber, [c.x, c.y - c.r(r.k) - 14.0], w)
        })
        .collect();
    let spots = place_pills(&items.iter().map(|i| (i.2, i.3)).collect::<Vec<_>>(), r.w);
    items
        .into_iter()
        .zip(spots)
        .map(|((label, amber, _, _), [x, y])| {
            name_pill(label, amber, CAPTION)
                .absolute(x, y - 9.5)
                .px(7.0)
        })
        .collect()
}

fn who<'a>(r: &'a PitLane, c: &'a crate::pitlane::Car) -> Who<'a> {
    Who {
        id: &c.who.id,
        name: &c.who.name,
        mood: c.who.mood,
        host: r.garages.get(r.host_of(c)).map_or("", |g| g.host.as_str()),
    }
}

fn readout(r: &PitLane, waiting: usize, p: &Ink) -> El {
    // Its rank and work, when its machine publishes agent stats.
    let picked = r.selected_car().map(|c| {
        let card = c.who.work.map(|w| {
            format!(
                "Rank: {} · Work: {}",
                RANKS[c.tier.min(4)],
                creature::work_line(&w)
            )
        });
        (who(r, c), card)
    });
    playground::readout("pitlane", r, [r.garages.len(), waiting], r.h, picked, p)
}
