//! The pitch: the fleet as football training, each machine a squad in its
//! own share of the pitch, every agent a player in its harness's kit acting
//! out its state. A press on a player selects it; a press on the selected
//! one (or Return) shows its terminal, as the galaxy does. Escape lets go,
//! then closes. Night follows a dark theme.
//!
//! Drawn back to front as the prototype: the still ground (painted once per
//! layout), the stands' passing light, the squads' boards, then pigeons,
//! players and balls by depth, confetti, and the shell's overlays: shirt
//! numbers, the selection ring, name pills, press targets and the readout.

use rune::{Cx, El, TextStyle, canvas, col, el, row, text};

use super::reef::pen::Pen;
use super::reef::rgba;
use super::{CAPTION, Ink, LABEL_STRONG, overlay, pressable};
use crate::creature;
use crate::desk::Desk;
use crate::pitch::{AMBER, MINT, Mood, Pitch, Pt, RANKS, mix};

mod players;

use players::Spot;

const ON_AMBER: [f32; 3] = [42.0, 22.0, 0.0];
const PILL: TextStyle = TextStyle::new(11.0, 500.0).mono();
const BOARD: TextStyle = TextStyle::new(10.5, 600.0).mono();
const BENCH: TextStyle = TextStyle::new(9.5, 500.0).mono();

pub(super) fn view(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> El {
    desk.pitch.night.set(p.bg.luminance() < 0.5);
    desk.pitch.still.set(cx.is_reduced_motion());
    let r = &desk.pitch.scene;
    let count = |m: Mood| r.players.iter().filter(|q| q.who.mood == m).count();
    let waiting = count(Mood::Waiting);
    let summary = format!(
        "Pitch with {} agents on {} hosts: {} working, {waiting} need you, {} done, {} idle",
        r.players.len(),
        r.zones.len(),
        count(Mood::Working),
        count(Mood::Done),
        count(Mood::Idle),
    );
    let root = el()
        .key("pitch")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .fill(p.bg)
        .modal(false, Desk::close_pitch)
        .label(summary)
        .on_layout(|desk: &mut Desk, rect| desk.pitch_laid_out(rect[2], rect[3]));
    if r.zones.is_empty() {
        return root.child(readout(r, waiting, p));
    }
    let full = |e: El| e.absolute(0.0, 0.0).w_full().h_full();
    let ground = [(r.nt < 0.99, 1.0), (r.nt > 0.01, r.nt)]
        .into_iter()
        .enumerate()
        .filter(|(_, (shown, _))| *shown)
        .map(|(i, (_, alpha))| {
            full(el().key(format!("pitch-ground-{i}")))
                .pixels(&desk.pitch.ground[i])
                .opacity(alpha)
        });

    // The prototype's cloud shadows by day, each one image.
    let clouds = players::clouds(r)
        .filter(|_| r.nt < 0.98)
        .map(|([x, y], rad)| {
            el().absolute(x - rad, y - rad)
                .size(2.0 * rad, 2.0 * rad)
                .pixels(&desk.pitch.cloud)
                .opacity(0.09 * (1.0 - r.nt))
        });
    let mut air = Pen::new();
    players::ambient(&mut air, r);

    let mut play = Pen::new();
    players::pigeons(&mut play, r);
    // Players and balls by depth, as the prototype sorts its items.
    enum Item<'a> {
        Player(&'a crate::pitch::Player, Pt),
        Ball(&'a crate::pitch::Ball),
    }
    let mut items: Vec<(f32, Item)> = r
        .players
        .iter()
        .map(|q| {
            let at = r.to_s(q.l, q.s);
            (at[1], Item::Player(q, at))
        })
        .chain(
            r.zones
                .iter()
                .map(|z| (r.to_s(z.ball.l, z.ball.s)[1] + 0.5, Item::Ball(&z.ball))),
        )
        .collect();
    items.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut spots: Vec<(&crate::pitch::Player, Spot)> = Vec::new();
    for (_, item) in items {
        match item {
            Item::Player(q, at) => spots.push((q, players::player(&mut play, r, q, at[0], at[1]))),
            Item::Ball(b) => players::ball(&mut play, r, b),
        }
    }
    players::confetti(&mut play, r);

    let numbers = spots.iter().map(|(q, s)| {
        let ([x, y], size, c) = s.number;
        // Keyed: depth order reshuffles every frame.
        row()
            .key(format!("pitch-number-{:?}", q.who.id))
            .absolute(x - 20.0, y - size)
            .size(40.0, size * 2.0)
            .justify_center()
            .items_center()
            .child(
                text(q.who.number.to_string())
                    .font(TextStyle::new(size, 700.0).mono())
                    .color(rgba(c, 1.0)),
            )
    });
    let ring = spots
        .iter()
        .find(|(q, _)| r.selected.as_ref() == Some(&q.who.id))
        .map(|(_, s)| {
            let rad = s.r + 7.0;
            el().key("pitch-ring")
                .absolute(s.at[0] - rad, s.at[1] - rad)
                .size(2.0 * rad, 2.0 * rad)
                .radius(rad)
                .border(2.0, rgba([255.0; 3], 1.0))
                .border_dash(5.0, 4.0)
                .border_dash_phase(-r.t * 20.0)
        });
    let targets = spots.iter().map(|(q, s)| {
        let rad = s.r.max(10.0) + 8.0;
        let host = &r.zone_of(q).host;
        let (press, hover) = (q.who.id.clone(), q.who.id.clone());
        el().key(format!("pitch-player-{:?}", q.who.id))
            .absolute(s.at[0] - rad, s.at[1] - rad)
            .size(2.0 * rad, 2.0 * rad)
            .radius(rad)
            .cursor(rune::Cursor::Pointer)
            .label(format!("{}, {}, {host}", q.who.name, q.who.mood.word()))
            .on_press(move |desk: &mut Desk| desk.pitch_press(press.clone()))
            .on_hover_change(move |desk: &mut Desk, on, _| desk.pitch_hover(hover.clone(), on))
    });
    let pills = pills(r, &spots);

    root.children(ground)
        .children(clouds)
        .child(full(canvas(air.into_shape()).key("pitch-air")))
        .children(r.zones.iter().enumerate().map(|(i, _)| board(r, i)))
        .children((0..r.zones.len()).filter_map(|i| bench(r, i)))
        .child(full(canvas(play.into_shape()).key("pitch-play")).on_press(Desk::pitch_deselect))
        .children(numbers)
        .children(ring)
        .children(dozing(r, &spots))
        .children(pills)
        .children(targets)
        .child(readout(r, waiting, p))
}

/// Name pills over players that need you, are selected or hovered: the
/// lowest first, each nudged up past the ones already placed.
fn pills(r: &Pitch, spots: &[(&crate::pitch::Player, Spot)]) -> Vec<El> {
    let mut shown: Vec<(&crate::pitch::Player, &Spot)> = spots
        .iter()
        .filter(|(q, _)| {
            q.who.mood == Mood::Waiting
                || r.selected.as_ref() == Some(&q.who.id)
                || r.hovered.as_ref() == Some(&q.who.id)
        })
        .map(|(q, s)| (*q, s))
        .collect();
    shown.sort_by(|a, b| b.1.at[1].total_cmp(&a.1.at[1]));
    let mut placed: Vec<[f32; 3]> = Vec::new();
    let h = 19.0;
    shown
        .into_iter()
        .map(|(q, s)| {
            let amber = q.who.mood == Mood::Waiting;
            let label = if amber {
                format!("{} · needs you", q.who.name)
            } else {
                q.who.name.clone()
            };
            // Monospace: a character is 0.6 of the size across.
            let w = label.chars().count() as f32 * 6.6 + 14.0;
            let x = (s.at[0] - w / 2.0).clamp(6.0, (r.w - w - 6.0).max(6.0));
            // Up past the pills already placed, as the prototype; where the
            // view's top runs out, down instead.
            let base = s.at[1] - s.r - 14.0;
            let free = |y: f32| {
                !placed.iter().any(|p| {
                    (p[1] - y).abs() < h + 2.0 && x < p[0] + p[2] + 4.0 && p[0] < x + w + 4.0
                })
            };
            let y = (0..7)
                .map(|i| base - i as f32 * (h + 3.0))
                .take_while(|y| *y >= h / 2.0 + 4.0)
                .chain((1..7).map(|i| base + i as f32 * (h + 3.0)))
                .find(|y| free(*y))
                .unwrap_or(base);
            placed.push([x, y, w]);
            let (fill, ink) = if amber {
                (rgba(AMBER, 0.96), rgba(ON_AMBER, 1.0))
            } else {
                (
                    rgba([10.0, 14.0, 20.0], 0.78),
                    rgba([238.0, 242.0, 246.0], 1.0),
                )
            };
            row()
                .key(format!("pitch-pill-{:?}", q.who.id))
                .absolute(x, y - h / 2.0)
                .h(h)
                .px(7.0)
                .radius(h / 2.0)
                .items_center()
                .fill(fill)
                .child(text(label).font(PILL).color(ink))
        })
        .collect()
}

/// A squad's board on the pitch: its machine, its players and how many need you.
fn board(r: &Pitch, i: usize) -> El {
    let z = &r.zones[i];
    let at = if r.portrait {
        r.to_s(z.z0 + 13.0 * r.k, r.sp / 2.0)
    } else {
        r.to_s(z.z0 + z.zw / 2.0, 13.0 * r.k)
    };
    let waiting = r.waiting_in(i);
    let name = z.host.to_uppercase();
    let label = if z.known {
        format!("{name}  {}", r.players_in(i))
    } else {
        name
    };
    let ink = mix([210.0, 255.0, 236.0], MINT, r.nt * 0.5);
    row()
        .key(format!("pitch-board-{i}"))
        .absolute(at[0] - 160.0, at[1] - 9.5)
        .w(320.0)
        .h(19.0)
        .justify_center()
        .child(
            row()
                .h(19.0)
                .pl(9.0)
                .pr(if waiting > 0 { 3.0 } else { 9.0 })
                .gap(7.0)
                .radius(4.0)
                .items_center()
                .fill(rgba([12.0, 18.0, 28.0], 0.9))
                .border(1.0, rgba(MINT, 0.35 + 0.3 * r.nt))
                .child(text(label).font(BOARD).color(rgba(ink, 1.0)))
                .children((waiting > 0).then(|| {
                    el().size(15.0, 15.0)
                        .radius(7.5)
                        .center()
                        .fill(rgba(AMBER, 1.0))
                        .child(
                            text(waiting.to_string())
                                .font(BOARD)
                                .color(rgba(ON_AMBER, 1.0)),
                        )
                })),
        )
}

/// Over a squad's dugout, while any of its players sit there: they are
/// idle, waiting on nothing but a new task.
fn bench(r: &Pitch, i: usize) -> Option<El> {
    let z = &r.zones[i];
    if z.idle == 0 {
        return None;
    }
    let (l, t) = (z.z0 + z.zw * 0.3, r.strip);
    // Above the dugout's roof; standing up the side, on its middle.
    let [x, y] = if r.portrait {
        r.to_s(l, -t * 0.64)
    } else {
        // Clear of the heads of the players sat in it.
        let [x, y] = r.to_s(l, -t * 0.94);
        [x, y - 20.0]
    };
    let ink = mix([210.0, 255.0, 236.0], MINT, r.nt * 0.5);
    Some(
        row()
            .key(format!("pitch-bench-{i}"))
            .absolute(x - 80.0, y - 7.5)
            .w(160.0)
            .h(15.0)
            .justify_center()
            .inert()
            .child(
                row()
                    .h(15.0)
                    .px(6.0)
                    .radius(3.0)
                    .items_center()
                    .fill(rgba([12.0, 18.0, 28.0], 0.62))
                    .border(1.0, rgba(MINT, 0.18 + 0.15 * r.nt))
                    .child(
                        text(format!("bench · {} idle", z.idle))
                            .font(BENCH)
                            .color(rgba(ink, 0.8)),
                    ),
            ),
    )
}

/// A soft "z" drifting up from each player sat on the bench; held still
/// with reduced motion.
fn dozing<'a>(
    r: &'a Pitch,
    spots: &'a [(&crate::pitch::Player, Spot)],
) -> impl Iterator<Item = El> + 'a {
    spots
        .iter()
        .filter(|(q, _)| q.who.mood == Mood::Idle && q.sit > 0.5)
        .map(|(q, s)| {
            let k = if r.reduce {
                0.35
            } else {
                (r.t * 0.3 + q.who.number as f32 * 0.37).fract()
            };
            let alpha = 0.9 * (k * 6.0).min(1.0) * (1.0 - k);
            text("z")
                .key(format!("pitch-z-{:?}", q.who.id))
                .absolute(
                    s.at[0] + 0.4 * s.r + 5.0 * k,
                    s.at[1] - 0.9 * s.r - 10.0 * k,
                )
                .font(TextStyle::new(10.0 + 4.0 * k, 700.0).mono())
                .color(rgba(
                    mix([24.0, 40.0, 58.0], [236.0, 242.0, 248.0], r.nt),
                    alpha,
                ))
                .inert()
        })
}

fn readout(r: &Pitch, waiting: usize, p: &Ink) -> El {
    let working = r
        .players
        .iter()
        .filter(|q| q.who.mood == Mood::Working)
        .count();
    let picked = r.selected_player().map(|q| {
        let line = format!(
            "{} · {} · {}  ↩",
            q.who.name,
            q.who.mood.word(),
            r.zone_of(q).host
        );
        // Its rank and work, when its machine publishes agent stats.
        let card = q.who.work.map(|w| {
            format!(
                "Rank: {} · Work: {}",
                RANKS[q.tier.min(4)],
                creature::work_line(&w)
            )
        });
        pressable("pitch-open", p)
            .px(12.0)
            .py(5.0)
            .radius(12.0)
            .fill(p.raised.alpha(0.85))
            .label(format!("Open {}", q.who.name))
            .on_press(Desk::open_player)
            .child(
                col()
                    .gap(2.0)
                    .child(text(line).font(LABEL_STRONG).color(p.text))
                    .children(card.map(|c| text(c).font(CAPTION).color(p.dim))),
            )
    });
    let y = if r.h > 0.0 { r.h - 44.0 } else { 42.0 };
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
                        r.zones.len(),
                        r.players.len()
                    ))
                    .font(CAPTION)
                    .color(p.dim),
                ),
        )
        .children((waiting > 0).then(|| {
            pressable("pitch-next-waiting", p)
                .px(12.0)
                .py(5.0)
                .radius(12.0)
                .fill(p.marks.alert)
                .label("Show the next agent that needs you")
                .on_press(Desk::pitch_next_waiting)
                .child(
                    text(format!("{waiting} need you"))
                        .font(LABEL_STRONG)
                        .color(crate::theme::on_fill(p.marks.alert)),
                )
        }))
        .children(picked)
        .child(overlay::icon_button(
            "✕",
            "Close",
            "pitch-close",
            p,
            Desk::close_pitch,
        ))
}
