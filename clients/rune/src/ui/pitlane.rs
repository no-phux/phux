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

use rune::{Cx, El, canvas, col, el, row, text};

use super::reef::pen::{Pen, rgba};
use super::{CAPTION, Ink, LABEL_STRONG, TITLEBAR_H, overlay, pressable};
use crate::creature;
use crate::desk::Desk;
use crate::pitlane::{AMBER, Mode, PitLane, RANKS};
use crate::reef::{Mood, Pt};

mod car;
mod ground;

const ON_AMBER: [f32; 3] = [42.0, 22.0, 0.0];
const PILL_H: f32 = 19.0;

pub(super) fn view(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> El {
    desk.pitlane.night.set(p.bg.luminance() < 0.5);
    desk.pitlane.still.set(cx.is_reduced_motion());
    let r = &desk.pitlane.scene;
    let count = |m: Mood| r.cars.iter().filter(|c| c.who.mood == m).count();
    let waiting = count(Mood::Waiting);
    let summary = format!(
        "Pit Lane with {} agents on {} hosts: {} working, {waiting} need you, {} done, {} idle",
        r.cars.len(),
        r.garages.len(),
        count(Mood::Working),
        count(Mood::Done),
        count(Mood::Idle),
    );
    let root = el()
        .key("pitlane")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .fill(p.bg)
        .modal(false, Desk::close_pitlane)
        .label(summary)
        .on_layout(|desk: &mut Desk, rect| desk.pitlane_laid_out(rect[2], rect[3]));
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
        let host = r.garages.get(r.host_of(c)).map_or("", |g| g.host.as_str());
        let (press, hover) = (c.who.id.clone(), c.who.id.clone());
        el().key(format!("pitlane-car-{:?}", c.who.id))
            .absolute(c.x - rad, c.y - rad)
            .size(2.0 * rad, 2.0 * rad)
            .radius(rad)
            .cursor(rune::Cursor::Pointer)
            .label(format!("{}, {}, {host}", c.who.name, c.who.mood.word()))
            .on_press(move |desk: &mut Desk| desk.pitlane_press(press.clone()))
            .on_hover_change(move |desk: &mut Desk, on, _| desk.pitlane_hover(hover.clone(), on))
    });

    let full =
        |shape: rune::Shape, key: &str| canvas(shape).key(key).absolute(0.0, 0.0).w_full().h_full();
    root.children((r.nt < 0.99).then(|| full(day, "pitlane-day")))
        .children((r.nt > 0.01).then(|| {
            let night = full(night, "pitlane-night");
            if r.nt < 0.99 {
                night.opacity(r.nt)
            } else {
                night
            }
        }))
        .child(full(pen.into_shape(), "pitlane-cars").on_press(Desk::pitlane_deselect))
        .children(r.garages.iter().enumerate().map(|(i, _)| board(r, i)))
        .children(pills(r))
        .children(targets)
        .child(readout(r, waiting, p))
}

/// The grounds by day and by night, redrawn only when the layout changes.
fn grounds(desk: &Desk, r: &PitLane) -> (rune::Shape, rune::Shape) {
    let mut cache = desk.pitlane.ground.borrow_mut();
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
                .children((waiting > 0).then(|| {
                    el().size(15.0, 15.0)
                        .radius(7.5)
                        .center()
                        .fill(rgba(AMBER, 1.0))
                        .border(1.0, ink)
                        .child(
                            text(waiting.to_string())
                                .font(CAPTION)
                                .color(rgba(ON_AMBER, 1.0)),
                        )
                })),
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
            let amber = c.who.mood == Mood::Waiting;
            let text = if amber {
                format!("{} · needs you", c.who.name)
            } else {
                c.who.name.clone()
            };
            // ponytail: an estimated width (the pill sizes to its text) for
            // keeping pills apart and in view; measure if they ever overlap.
            let w = text.chars().count() as f32 * 5.6 + 14.0;
            (text, amber, [c.x, c.y - c.r(r.k) - 14.0], w)
        })
        .collect();
    let spots = stack(&items.iter().map(|i| (i.2, i.3)).collect::<Vec<_>>(), r.w);
    items
        .into_iter()
        .zip(spots)
        .map(|((label, amber, _, _), [x, y])| {
            let (fill, ink) = if amber {
                (rgba(AMBER, 0.96), rgba(ON_AMBER, 1.0))
            } else {
                (
                    rgba([10.0, 14.0, 20.0], 0.78),
                    rgba([238.0, 242.0, 246.0], 1.0),
                )
            };
            row()
                .absolute(x, y - PILL_H / 2.0)
                .h(PILL_H)
                .px(7.0)
                .radius(PILL_H / 2.0)
                .items_center()
                .fill(fill)
                .child(text(label).font(CAPTION).color(ink))
        })
        .collect()
}

/// Left edges and centre heights for pills of width `w` wanted centred on
/// `at`, in order: each kept in the view and nudged up past those already
/// placed, up to six times, as the prototype's `pill`.
fn stack(pills: &[(Pt, f32)], view_w: f32) -> Vec<Pt> {
    let mut placed: Vec<[f32; 3]> = Vec::new();
    for &(at, w) in pills {
        let x = (at[0] - w / 2.0).min(view_w - w - 6.0).max(6.0);
        let mut y = at[1];
        for _ in 0..6 {
            let hit = placed.iter().any(|p| {
                (p[1] - y).abs() < PILL_H + 2.0 && x < p[0] + p[2] + 4.0 && p[0] < x + w + 4.0
            });
            if !hit {
                break;
            }
            y -= PILL_H + 3.0;
        }
        placed.push([x, y, w]);
    }
    placed.into_iter().map(|p| [p[0], p[1]]).collect()
}

fn readout(r: &PitLane, waiting: usize, p: &Ink) -> El {
    let working = r
        .cars
        .iter()
        .filter(|c| c.who.mood == Mood::Working)
        .count();
    let picked = r.selected_car().map(|c| {
        let host = r.garages.get(r.host_of(c)).map_or("", |g| g.host.as_str());
        let line = format!("{} · {} · {host}  ↩", c.who.name, c.who.mood.word());
        // Its rank and work, when its machine publishes agent stats.
        let card = c.who.work.map(|w| {
            format!(
                "Rank: {} · Work: {}",
                RANKS[c.tier.min(4)],
                creature::work_line(&w)
            )
        });
        pressable("pitlane-open", p)
            .px(12.0)
            .py(5.0)
            .radius(12.0)
            .fill(p.raised.alpha(0.85))
            .label(format!("Open {}", c.who.name))
            .on_press(Desk::open_car)
            .child(
                col()
                    .gap(2.0)
                    .child(text(line).font(LABEL_STRONG).color(p.text))
                    .children(card.map(|c| text(c).font(CAPTION).color(p.dim))),
            )
    });
    let y = if r.h > 0.0 {
        r.h - 44.0
    } else {
        TITLEBAR_H + 4.0
    };
    row()
        .absolute(16.0, y)
        .gap(10.0)
        .items_center()
        .child(
            el().px(10.0)
                .py(4.0)
                .radius(10.0)
                .fill(p.raised.alpha(0.85))
                .child(
                    text(format!(
                        "{} hosts · {} agents · {working} working",
                        r.garages.len(),
                        r.cars.len()
                    ))
                    .font(CAPTION)
                    .color(p.dim),
                ),
        )
        .children((waiting > 0).then(|| {
            pressable("pitlane-next-waiting", p)
                .px(12.0)
                .py(5.0)
                .radius(12.0)
                .fill(p.marks.alert)
                .label("Show the next agent that needs you")
                .on_press(Desk::pitlane_next_waiting)
                .child(
                    text(format!("{waiting} need you"))
                        .font(LABEL_STRONG)
                        .color(crate::theme::on_fill(p.marks.alert)),
                )
        }))
        .children(picked)
        .child(overlay::icon_button(
            &super::icons::CLOSE,
            "Close",
            "pitlane-close",
            p,
            Desk::close_pitlane,
        ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn close_pills_stack_upward_and_stay_in_view() {
        let spots = super::stack(
            &[
                ([100.0, 200.0], 80.0),
                ([110.0, 200.0], 80.0),
                ([400.0, 200.0], 80.0),
                ([2.0, 50.0], 80.0),
            ],
            500.0,
        );
        assert_eq!(spots[0], [60.0, 200.0]);
        assert_eq!(spots[1], [70.0, 178.0], "nudged up one pill");
        assert_eq!(spots[2], [360.0, 200.0], "clear of the others");
        assert_eq!(spots[3], [6.0, 50.0], "kept inside the view");
    }
}
