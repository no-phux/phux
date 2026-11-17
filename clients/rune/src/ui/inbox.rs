//! The inbox: a drawer of everything waiting on you, answerable in place,
//! and the card a focused pane shows when its agent is asking.

use crate::ui::tokens::Voice;
use phux_client_runtime::approvals::{Approval, Decision};
use phux_design::{State, Tone};
use phux_protocol::ids::ResourceId;
use rune::{Cursor, Edge, El, Transition, col, el, row, text, text_input};
use rune_kit::{Button, ButtonVariant, Card, ControlSize, Spinner};

use super::overlay::{EXTENTS, Glass, glass, key_hint, notice_words, pop};
use super::tokens::{Paint, Tier, Tokens};
use super::{MOD, badge, icons, lifted};
use crate::activity::{Ask, Mark};
use crate::desk::Desk;
use crate::inbox::{self, Item, Waiting};

/// `mark`'s vector mark and tone, colour and shape together.
fn mark_icon(mark: Mark, t: &Tokens) -> (El, rune::Color) {
    let size = t.icon.size.sm;
    match mark.state() {
        Some(state) => (
            badge::mark(t, state, size).a11y_hidden(),
            t.status.state(state).tone.ink,
        ),
        None => {
            let ink = t.status.tone(Tone::Bell).ink;
            (rune::icon(&icons::BELL, size).color(ink).a11y_hidden(), ink)
        }
    }
}

/// One suggested answer (§4.1.3 OWN chip): the selected wash inside the
/// control outline. `index` is its ⌘ digit, while the row is picked. While
/// the answer sends, every chip is disabled, not hidden (§1.10 loading).
fn chip(
    terminal: &ResourceId,
    question: &str,
    answer: &str,
    index: Option<usize>,
    busy: bool,
    t: &Tokens,
) -> El {
    let s = &t.space;
    let r = &t.roles;
    let (terminal, question, send) = (terminal.clone(), question.to_owned(), answer.to_owned());
    let (ink, edge) = if busy {
        (r.text.faint, r.border.muted)
    } else {
        (r.text.text, r.border.control)
    };
    let chip = row()
        .key(format!("chip-{terminal}-{answer}"))
        .role("button")
        .label(answer.to_owned())
        .min_h(s.pointer_min)
        .px(s.s2)
        .gap(s.icon_gap)
        .radius(t.radius.sm)
        .items_center()
        .border(s.hairline, edge.paint())
        .child(
            text(answer.to_owned())
                .font(t.ty.caption)
                .color(ink.paint())
                .a11y_hidden(),
        )
        .children(index.filter(|i| *i < 9).map(|i| {
            text(format!("{MOD}{}", i + 1))
                .font(t.ty.readout)
                .color(r.text.dim.paint())
                .a11y_hidden()
        }));
    if busy {
        return chip.disabled(true);
    }
    t.kit
        .interactive(chip.fill(r.wash.selected.paint()))
        .focusable(true)
        .cursor(Cursor::Pointer)
        .on_press(move |d: &mut Desk| d.answer(&terminal, &question, &send))
}

fn chips(terminal: &ResourceId, ask: &Ask, keyed: bool, busy: bool, t: &Tokens) -> El {
    let mut line = row().w_full().gap(t.space.icon_gap).flex_wrap();
    for (index, answer) in ask.suggestions.iter().enumerate() {
        line = line.child(chip(
            terminal,
            &ask.id,
            answer,
            keyed.then_some(index),
            busy,
            t,
        ));
    }
    line
}

/// A card's top line: the mark, who, and the state word with its age. A
/// send in flight shows the kit spinner beside its word.
fn header(mark: Mark, title: String, detail: String, busy: bool, t: &Tokens) -> El {
    let r = &t.roles;
    let (icon, _) = mark_icon(mark, t);
    row()
        .w_full()
        .gap(t.space.s2)
        .items_center()
        .child(icon)
        .child(
            text(title)
                .font(t.ty.label.semibold())
                .color(r.text.text.paint())
                .truncate()
                .min_w(0.0)
                .shrink(),
        )
        .child(el().grow(1.0))
        .children(busy.then(|| {
            Spinner::new()
                .size(t.icon.size.sm)
                .color(r.text.dim.paint())
                .build(&t.kit)
        }))
        .child(text(detail).font(t.ty.caption).color(r.text.dim.paint()))
}

/// A reply field in the control outline (`TextInput`'s look). The editor
/// keeps its own key: native text input finds fields by it.
pub(super) fn field_shell(input: El, t: &Tokens, enabled: bool) -> El {
    let s = &t.space;
    let edge = if enabled {
        t.roles.border.control
    } else {
        t.roles.border.muted
    };
    row()
        .w_full()
        .min_h(s.row_h)
        .px(s.s2)
        .py(s.s1)
        .radius(t.radius.sm)
        .items_center()
        .border(s.hairline, edge.paint())
        .child(input)
}

fn waiting_card(desk: &Desk, waiting: &Waiting, picked: bool, t: &Tokens) -> El {
    let r = &t.roles;
    let who = waiting
        .agent
        .clone()
        .unwrap_or_else(|| waiting.label.clone());
    let age = waiting
        .ask
        .as_ref()
        .and_then(|ask| ask.since)
        .map(|since| format!(" · {}", inbox::waited(since)))
        .unwrap_or_default();
    let busy = desk.answering(&waiting.terminal);
    let word = if busy {
        "sending…"
    } else {
        waiting.mark.word()
    };
    let (_, tone) = mark_icon(waiting.mark, t);
    let place = match &waiting.kind {
        Some(kind) if Some(kind) != waiting.agent.as_ref() => {
            format!("{} · {} · {kind}", waiting.label, waiting.session)
        }
        _ => format!("{} · {}", waiting.label, waiting.session),
    };
    let mut body = col()
        .w_full()
        .gap(t.space.s2)
        .child(header(waiting.mark, who, format!("{word}{age}"), busy, t))
        .child(
            text(place)
                .font(t.ty.caption)
                .color(r.text.dim.paint())
                .truncate(),
        );
    let Some(ask) = &waiting.ask else {
        let hint = (waiting.mark == Mark::Blocked).then(|| {
            text("Waiting on you in its terminal.")
                .font(t.ty.label)
                .color(r.text.text.paint())
        });
        return card(body.children(hint), tone, t);
    };
    body = body.child(
        text(ask.text.clone())
            .font(t.ty.label)
            .color(r.text.text.paint())
            .wrap(),
    );
    if !ask.typed() {
        let panel = super::questions::panel(desk, &waiting.terminal, ask, t);
        return card(body.child(panel), tone, t);
    }
    if !ask.suggestions.is_empty() {
        body = body.child(chips(&waiting.terminal, ask, picked, busy, t));
    }
    if picked {
        let input = text_input(&desk.inbox.reply, t.ty.body)
            .key("inbox-reply")
            .label("Reply to question")
            .single_line()
            .grow(1.0)
            .color(r.text.text.paint())
            .placeholder("Reply…", r.text.faint.paint())
            .on_edit(|d: &mut Desk| &mut d.inbox.reply)
            .on_input_key(|d: &mut Desk, event, tx| d.inbox_key(event, tx))
            .on_submit(|d: &mut Desk, tx| d.submit_picked(tx));
        body = body.child(field_shell(input, t, true));
    }
    card(body, tone, t)
}

/// The kit card: a rule in the item's tone beside its content.
fn card(body: El, tone: rune::Color, t: &Tokens) -> El {
    Card::new().accent(tone).child(body).build(&t.kit)
}

fn approval_card(approval: &Approval, t: &Tokens) -> El {
    let r = &t.roles;
    let requester = inbox::approval_requester(approval);
    let mono = t.ty.mono_caption;
    let mut body = col()
        .w_full()
        .gap(t.space.s2)
        .child(header(
            Mark::Blocked,
            inbox::approval_title(approval),
            inbox::expiry(approval),
            false,
            t,
        ))
        .child(
            text(format!("Held for {requester}"))
                .font(t.ty.caption)
                .color(r.text.dim.paint())
                .truncate(),
        );
    if let Some(credential) = &approval.credential {
        for detail in inbox::credential_details(approval) {
            body = body.child(
                text(detail)
                    .font(t.ty.caption)
                    .color(r.text.text.paint())
                    .wrap(),
            );
        }
        for reference in &credential.request.references {
            body = body.child(
                text(reference.clone())
                    .font(mono)
                    .color(r.text.text.paint())
                    .wrap(),
            );
        }
    }
    for subject in &approval.subjects {
        body = body.child(
            text(subject.clone())
                .font(mono)
                .color(r.text.text.paint())
                .truncate(),
        );
    }
    // Deny is destructive; Approve is the one primary action (§1.10).
    let decide = |decision, label: &'static str, chord: &'static str, variant| {
        let id = approval.id.clone();
        let key = format!("decide-{label}-{id}");
        Button::new(label)
            .variant(variant)
            .control_size(ControlSize::Large)
            .shortcut(format!("{MOD}{chord}"))
            .on_press(move |d: &mut Desk| d.decide(&id, decision))
            .build(&t.kit)
            .key(key)
            .grow(1.0)
    };
    body = body.child(
        row()
            .w_full()
            .gap(t.space.icon_gap)
            .child(decide(Decision::Deny, "Deny", "⌫", ButtonVariant::Danger))
            .child(decide(
                Decision::Approve,
                "Approve",
                "↩",
                ButtonVariant::Submit,
            )),
    );
    card(body, t.status.state(State::NeedsYou).tone.ink, t)
}

fn item_row(desk: &Desk, item: &Item, index: usize, picked: bool, t: &Tokens) -> El {
    let body = match item {
        Item::Approval(approval) => approval_card(approval, t),
        Item::Terminal(waiting) => waiting_card(desk, waiting, picked, t),
    };
    let row = el()
        .key(format!("inbox-item-{index}"))
        .w_full()
        .padding(t.space.s1)
        .radius(t.radius.md)
        .border(t.space.hairline, t.roles.border.muted.paint());
    let row = if picked { lifted(row, t) } else { row };
    t.kit
        .interactive(row)
        .cursor(Cursor::Pointer)
        .on_press_tx(move |d: &mut Desk, tx| d.pick_item(index, tx))
        .child(body)
}

/// Nothing waiting: a neutral illustration (no status tone is spent when
/// nothing needs you), and what lands here.
fn empty(t: &Tokens) -> El {
    let r = &t.roles;
    col()
        .w_full()
        .py(t.space.s12)
        .gap(t.space.s2)
        .items_center()
        .child(
            rune::icon(&icons::INBOX, t.icon.size.xl)
                .color(r.text.faint.paint())
                .a11y_hidden(),
        )
        .child(
            text("Nothing needs you")
                .font(t.ty.body)
                .color(r.text.text.paint()),
        )
        .child(
            text("Asks, approvals and finished agents land here.")
                .font(t.ty.caption)
                .color(r.text.dim.paint()),
        )
}

/// The drawer, on the trailing edge: it slides in on the drawer spring and
/// fades out.
pub fn view(desk: &Desk, p: &Tokens) -> El {
    let t = p;
    let s = &t.space;
    let r = &t.roles;
    let items = desk.inbox_items();
    let picked = desk.picked_index(&items);
    let mut list = col().key("inbox-list").w_full().padding(s.s2).gap(s.s2);
    for (index, item) in items.iter().enumerate() {
        list = list.child(item_row(desk, item, index, Some(index) == picked, t));
    }
    if items.is_empty() {
        list = list.child(empty(t));
    }
    let mut foot = row().w_full().px(s.s3).py(s.s2).gap(s.s3).items_center();
    foot = match &desk.notice {
        Some(notice) => foot.child(notice_words(notice, t)),
        // Only the hints that apply: nothing to move to or open when empty.
        None if items.is_empty() => foot.child(key_hint("esc", "close", t)),
        None => foot
            .child(key_hint("↑↓", "move", t))
            .child(key_hint("↩", "send or open", t))
            .child(key_hint("esc", "close", t)),
    };
    let rule = |key: &'static str| {
        rune::rule(s.hairline, r.border.base.paint())
            .w_full()
            .key(key)
    };
    let drawer = col()
        .key("inbox")
        // The phone form is narrower than the drawer.
        .w_full()
        .max_w(EXTENTS.drawer_w)
        .h_full()
        .border(s.hairline, r.border.base.paint())
        .transition(Transition::asymmetric(
            Transition::move_edge(Edge::Trailing).springs(t.motion.drawer),
            Transition::opacity(),
        ))
        .child(
            row()
                .w_full()
                .h(s.s12)
                .px(s.s4)
                .items_center()
                .justify_between()
                .child(text("Inbox").font(t.ty.title).color(r.text.text.paint()))
                .child(
                    text(items.len().to_string())
                        .font(t.ty.readout)
                        .color(r.text.dim.paint()),
                ),
        )
        .child(rule("inbox-header-rule"))
        .child(
            col()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(rune::ScrollSpec {
                    stick_bottom: false,
                })
                .child(list),
        )
        .child(rule("inbox-footer-rule"))
        .child(foot);
    let drawer = t.elevate(
        glass(
            drawer,
            t,
            r.layer.raised,
            Glass::Overlay,
            desk.phone.is_some(),
        ),
        Tier::Overlay,
    );
    super::scrim(el().key("inbox-scrim"), "Inbox", Some(Desk::close_inbox), t)
        .child(row().w_full().h_full().child(el().grow(1.0)).child(drawer))
}

/// A visible inline question and its text-input ownership.
pub struct AskCard {
    pub content: El,
    /// Only a field in the card actually shown can take the terminal's IME.
    pub editing: bool,
}

/// A focused pane's agent is asking: the question over the pane's corner,
/// answerable with a click. Hidden while find or the inbox covers it.
pub fn ask_card(desk: &Desk, id: &ResourceId, area: [f32; 2], p: &Tokens) -> Option<AskCard> {
    let t = p;
    let s = &t.space;
    let r = &t.roles;
    if desk.connection.find.is_some() || desk.inbox.open {
        return None;
    }
    let pane = desk.connection.topology.as_ref()?.pane(id)?;
    let ask = inbox::question(pane, &desk.activities).or_else(|| desk.inbox.questions.ask(id))?;
    let width = EXTENTS.ask_w.min(area[0] - s.s5);
    if width < EXTENTS.ask_min_w {
        return None;
    }
    let busy = desk.answering(id);
    let who = desk.activities.agent_name(id).unwrap_or("Agent").to_owned();
    let mut card = col()
        .key(format!("ask-{id}"))
        .absolute(super::PAD + area[0] - width - s.s2, super::PAD + s.s2)
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
            ask_hint(busy, ask.typed()),
            busy,
            t,
        ))
        .child(
            text(ask.text.clone())
                .font(t.ty.label)
                .color(r.text.text.paint())
                .wrap(),
        );
    if !ask.typed() {
        let panel = super::questions::panel(desk, id, &ask, t);
        return Some(AskCard {
            editing: desk.inbox.questions.field_focused(id, &ask.id),
            content: t.elevate(card.child(panel), Tier::Floating),
        });
    }
    if !ask.suggestions.is_empty() {
        card = card.child(chips(id, &ask, false, busy, t));
    }
    Some(AskCard {
        content: t.elevate(card, Tier::Floating),
        editing: false,
    })
}

fn ask_hint(busy: bool, typed: bool) -> String {
    match (busy, typed) {
        (true, _) => "sending…".to_owned(),
        (false, true) => format!("{MOD}I to reply"),
        (false, false) => "ACP".to_owned(),
    }
}

#[cfg(test)]
mod tests;
