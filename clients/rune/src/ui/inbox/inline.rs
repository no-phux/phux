//! The card a focused pane shows when its agent is asking: the question
//! over the pane's corner, answerable in place.

use phux_protocol::ids::ResourceId;
use rune::{El, col, text};

use super::{chips, header, words_font};
use crate::activity::{Ask, Mark};
use crate::desk::Desk;
use crate::inbox;
use crate::ui::overlay::{EXTENTS, pop};
use crate::ui::tokens::{Paint, Tier, Tokens};
use crate::ui::{MOD, terminal_padding};

/// A visible inline question and its text-input ownership.
pub struct AskCard {
    pub content: El,
    /// Only a field in the card actually shown can take the terminal's IME.
    pub editing: bool,
}

/// The inline card's size in a pane of `area`, if it fits: a phone also
/// needs room for a line or two of the question.
fn inline_size(area: [f32; 2], phone: bool, t: &Tokens) -> Option<[f32; 2]> {
    let s = &t.space;
    let width = EXTENTS.ask_w.min(area[0] - s.s5);
    let height = area[1] - s.s5;
    if width < EXTENTS.ask_min_w || (phone && height < s.s16) {
        return None;
    }
    Some([width, height])
}

/// A focused pane's agent is asking: the question over the pane's corner,
/// answerable with a click. Hidden while find or the inbox covers it. On a
/// phone it scrolls within the pane, independent of the terminal.
pub fn ask_card(desk: &Desk, id: &ResourceId, area: [f32; 2], p: &Tokens) -> Option<AskCard> {
    let t = p;
    let s = &t.space;
    let r = &t.roles;
    if desk.connection.find.is_some() || desk.inbox.open {
        return None;
    }
    let pane = desk.connection.topology.as_ref()?.pane(id)?;
    let ask = inbox::question(pane, &desk.activities).or_else(|| desk.inbox.questions.ask(id))?;
    let phone = desk.phone.is_some();
    let [width, height] = inline_size(area, phone, t)?;
    let pad = terminal_padding(desk);
    let busy = desk.answering(id);
    let who = desk.activities.agent_name(id).unwrap_or("Agent").to_owned();
    let mut card = col()
        .key(format!("ask-{id}"))
        .absolute(pad + area[0] - width - s.s2, pad + s.s2)
        .w(width)
        .padding(s.card_inset)
        .gap(s.s2)
        .radius(t.radius.md)
        .fill(r.layer.overlay.paint())
        .border(s.hairline, r.status.needs_you_edge.paint())
        .transition(pop(t))
        .child(header(
            Mark::Blocked,
            who,
            ask_hint(busy, ask.typed(), phone),
            busy,
            phone,
            t,
        ))
        .child(
            text(ask.text.clone())
                .font(words_font(t, phone))
                .color(r.text.text.paint())
                .wrap(),
        );
    if phone {
        card = card
            .max_h(height)
            .scroll(rune::ScrollSpec {
                stick_bottom: false,
            })
            // Keep background and text presses here instead of the
            // terminal's immediate drag handler, so Rune can pan this
            // scroll viewport.
            .role("group")
            .label("Agent question")
            .on_background_press(|_: &mut Desk| {});
    }
    Some(inline_answers(desk, id, &ask, card, t))
}

fn inline_answers(desk: &Desk, id: &ResourceId, ask: &Ask, card: El, t: &Tokens) -> AskCard {
    if !ask.typed() {
        let panel = crate::ui::questions::panel(desk, id, ask, t);
        return AskCard {
            editing: desk.inbox.questions.field_focused(id, &ask.id),
            content: t.elevate(card.child(panel), Tier::Floating),
        };
    }
    let phone = desk.phone.is_some();
    let card = if ask.suggestions.is_empty() {
        card
    } else {
        card.child(chips(id, ask, false, desk.answering(id), phone, t))
    };
    AskCard {
        content: t.elevate(card, Tier::Floating),
        editing: false,
    }
}

fn ask_hint(busy: bool, typed: bool, phone: bool) -> String {
    match (busy, typed, phone) {
        (true, _, _) => "sending…".to_owned(),
        (false, true, true) => "Reply in Inbox".to_owned(),
        (false, true, false) => format!("{MOD}I to reply"),
        (false, false, _) => "ACP".to_owned(),
    }
}
