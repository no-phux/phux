//! The line along a fleet scene's foot: its counts, the way to the next
//! agent that needs you, the picked agent's card, and close. Each scene
//! fills in what it counts and what its card says.

use rune::{El, col, el, row, text};

use crate::desk::Desk;
use crate::playground::Mood;
use crate::ui::{CAPTION, Ink, LABEL_STRONG, overlay, pressable};

/// The picked agent, as its card shows it.
pub(in crate::ui) struct Picked<'a> {
    pub name: &'a str,
    pub mood: Mood,
    /// Its machine's name.
    pub host: &'a str,
    /// The card's second line, when the scene has one for it.
    pub card: Option<String>,
}

/// A fleet scene's readout.
pub(in crate::ui) struct Readout<'a> {
    /// The widget ids' prefix: `<scene>-open`, `<scene>-next-waiting`,
    /// `<scene>-close`.
    pub scene: &'static str,
    pub hosts: usize,
    pub agents: usize,
    pub working: usize,
    pub waiting: usize,
    /// The view's height, zero until it is laid out.
    pub h: f32,
    /// Where the line sits until the view is laid out.
    pub unlaid_y: f32,
    pub picked: Option<Picked<'a>>,
    pub open: fn(&mut Desk),
    pub next_waiting: fn(&mut Desk),
    pub close: fn(&mut Desk),
}

/// How many of `moods` are working.
pub(in crate::ui) fn working(moods: impl Iterator<Item = Mood>) -> usize {
    moods.filter(|m| *m == Mood::Working).count()
}

pub(in crate::ui) fn readout(r: Readout<'_>, p: &Ink) -> El {
    // Along the foot once laid out, clear of the scene's own labels.
    let y = if r.h > 0.0 { r.h - 44.0 } else { r.unlaid_y };
    let counts = format!(
        "{} hosts · {} agents · {} working",
        r.hosts, r.agents, r.working
    );
    let next = (r.waiting > 0).then(|| next_waiting(&r, p));
    let picked = r.picked.map(|q| picked_card(r.scene, q, r.open, p));
    row()
        .absolute(16.0, y)
        .gap(10.0)
        .items_center()
        .child(
            el().px(10.0)
                .py(4.0)
                .radius(10.0)
                .fill(p.raised.alpha(0.85))
                .child(text(counts).font(CAPTION).color(p.dim)),
        )
        .children(next)
        .children(picked)
        .child(overlay::icon_button(
            &crate::ui::icons::CLOSE,
            "Close",
            format!("{}-close", r.scene),
            p,
            r.close,
        ))
}

fn next_waiting(r: &Readout<'_>, p: &Ink) -> El {
    pressable(format!("{}-next-waiting", r.scene), p)
        .px(12.0)
        .py(5.0)
        .radius(12.0)
        .fill(p.marks.alert)
        .label("Show the next agent that needs you")
        .on_press(r.next_waiting)
        .child(
            text(format!("{} need you", r.waiting))
                .font(LABEL_STRONG)
                .color(crate::theme::on_fill(p.marks.alert)),
        )
}

fn picked_card(scene: &str, q: Picked<'_>, open: fn(&mut Desk), p: &Ink) -> El {
    let line = format!("{} · {} · {}  ↩", q.name, q.mood.word(), q.host);
    pressable(format!("{scene}-open"), p)
        .px(12.0)
        .py(5.0)
        .radius(12.0)
        .fill(p.raised.alpha(0.85))
        .label(format!("Open {}", q.name))
        .on_press(open)
        .child(
            col()
                .gap(2.0)
                .child(text(line).font(LABEL_STRONG).color(p.text))
                .children(q.card.map(|c| text(c).font(CAPTION).color(p.dim))),
        )
}
