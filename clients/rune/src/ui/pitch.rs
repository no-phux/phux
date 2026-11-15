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

use rune::{Cx, El, TextStyle, canvas, el, row, text};

use super::Ink;
use super::playground::{self, Who, badge, name_pill, pill_label, pill_w, stage, target};
use super::reef::pen::Pen;
use super::reef::rgba;
use crate::creature;
use crate::desk::Desk;
use crate::pitch::{MINT, Mood, Pitch, Pt, RANKS, mix};

mod players;

use crate::ui::Cover;
use players::Spot;

const PILL: TextStyle = TextStyle::new(11.0, 500.0).mono();
const BOARD: TextStyle = TextStyle::new(10.5, 600.0).mono();
const BENCH: TextStyle = TextStyle::new(9.5, 500.0).mono();

pub(super) fn view(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> El {
    let r = &desk.pitch.scene;
    let (root, waiting) = stage(&desk.pitch, "pitch", "Pitch", r.zones.len(), p, cx);
    if r.zones.is_empty() {
        return root.child(readout(r, waiting, p));
    }
    let ground = [(r.nt < 0.99, 1.0), (r.nt > 0.01, r.nt)]
        .into_iter()
        .enumerate()
        .filter(|(_, (shown, _))| *shown)
        .map(|(i, (_, alpha))| {
            el().key(format!("pitch-ground-{i}"))
                .cover()
                .pixels(&desk.pitch.paint.ground[i])
                .opacity(alpha)
        });

    // The prototype's cloud shadows by day, each one image.
    let clouds = players::clouds(r)
        .filter(|_| r.nt < 0.98)
        .map(|([x, y], rad)| {
            el().absolute(x - rad, y - rad)
                .size(2.0 * rad, 2.0 * rad)
                .pixels(&desk.pitch.paint.cloud)
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
        target::<Pitch>("pitch-player", &who(r, q), s.at, rad)
    });
    let pills = pills(r, &spots);

    root.children(ground)
        .children(clouds)
        .child(canvas(air.into_shape()).key("pitch-air").cover())
        .children(r.zones.iter().enumerate().map(|(i, _)| board(r, i)))
        .children((0..r.zones.len()).filter_map(|i| bench(r, i)))
        .child(
            canvas(play.into_shape())
                .key("pitch-play")
                .cover()
                .on_press(Desk::scene_deselect::<Pitch>),
        )
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
            let (label, amber) = pill_label(&q.who.name, q.who.mood);
            // Monospace: a character is 0.6 of the size across.
            let w = pill_w(&label, 6.6);
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
            name_pill(label, amber, PILL)
                .key(format!("pitch-pill-{:?}", q.who.id))
                .absolute(x, y - h / 2.0)
                .px(7.0)
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
                .child(crate::host_color::dot(
                    &z.host,
                    7.0,
                    rgba([12.0, 18.0, 28.0], 1.0),
                ))
                .child(text(label).font(BOARD).color(rgba(ink, 1.0)))
                .children((waiting > 0).then(|| badge(waiting, 15.0, BOARD))),
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

fn who<'a>(r: &'a Pitch, q: &'a crate::pitch::Player) -> Who<'a> {
    Who {
        id: &q.who.id,
        name: &q.who.name,
        mood: q.who.mood,
        host: &r.zone_of(q).host,
    }
}

fn readout(r: &Pitch, waiting: usize, p: &Ink) -> El {
    // Its rank and work, when its machine publishes agent stats.
    let picked = r.selected_player().map(|q| {
        let card = q.who.work.map(|w| {
            format!(
                "Rank: {} · Work: {}",
                RANKS[q.tier.min(4)],
                creature::work_line(&w)
            )
        });
        (who(r, q), card)
    });
    playground::readout("pitch", r, [r.zones.len(), waiting], r.h, picked, p)
}
