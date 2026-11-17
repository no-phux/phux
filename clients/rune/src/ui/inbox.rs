//! The inbox: a drawer of everything waiting on you, answerable in place,
//! and the card a focused pane shows when its agent is asking.

use phux_client_runtime::approvals::{Approval, Decision};
use phux_protocol::ids::ResourceId;
use rune::{Color, El, Family, TextStyle, Transition, col, el, row, text, text_input};

use super::sidebar::mark_color;
use super::status::notice_line;
use super::{CAPTION, Ink, LABEL, LABEL_STRONG, pressable};
use crate::activity::{Ask, Mark};
use crate::desk::Desk;
use crate::inbox::{self, Item, Waiting};
use crate::ui::Cover;

#[cfg(test)]
mod tests;

const WIDTH: f32 = 400.0;
const CARD_W: f32 = 340.0;
const TOUCH_TEXT: TextStyle = TextStyle::new(15.0, 400.0).lh(21.0);

fn body_font(phone: bool) -> TextStyle {
    if phone { TOUCH_TEXT } else { LABEL }
}
/// Decisions' chords, shown on their buttons.
const DENY: &str = if cfg!(target_os = "macos") {
    "Deny  ⌘⌫"
} else {
    "Deny  Ctrl+Shift+⌫"
};
const APPROVE: &str = if cfg!(target_os = "macos") {
    "Approve  ⌘↩"
} else {
    "Approve  Ctrl+Shift+↩"
};

fn pick_hint(keys: &'static str, what: &'static str, ink: &Ink) -> El {
    row()
        .gap(5.0)
        .items_center()
        .child(
            el().px(5.0)
                .radius(4.0)
                .fill(ink.separator)
                .child(text(keys).font(CAPTION).color(ink.text)),
        )
        .child(text(what).font(CAPTION).color(ink.dim))
}

/// One suggested answer. `index` is its ⌘ digit, while the row is picked.
fn chip(
    terminal: &ResourceId,
    question: &str,
    answer: &str,
    index: Option<usize>,
    busy: bool,
    phone: bool,
    ink: &Ink,
) -> El {
    let (terminal, question, send) = (terminal.clone(), question.to_owned(), answer.to_owned());
    let mut chip = row()
        .key(format!("chip-{terminal}-{answer}"))
        .px(9.0)
        .py(4.0)
        .gap(6.0)
        .radius(6.0)
        .items_center()
        .border(1.0, ink.accent.alpha(0.5))
        .hover(ink.accent.alpha(0.16))
        .cursor(rune::Cursor::Pointer);
    chip = if phone {
        chip.w_full()
            .min_h(44.0)
            .border(1.0, ink.separator)
            .fill(ink.text.alpha(0.035))
            .child(
                text(answer.to_owned())
                    .w_full()
                    .font(TOUCH_TEXT)
                    .color(ink.text)
                    .wrap(),
            )
    } else {
        chip.child(text(answer.to_owned()).font(CAPTION).color(ink.text))
            .children(index.filter(|i| *i < 9).map(|i| {
                text(format!("{}{}", super::MOD, i + 1))
                    .font(CAPTION)
                    .color(ink.dim)
            }))
    };
    if busy {
        return chip.opacity(0.5);
    }
    chip.on_press(move |d: &mut Desk| d.answer(&terminal, &question, &send))
}

fn chips(terminal: &ResourceId, ask: &Ask, keyed: bool, busy: bool, phone: bool, ink: &Ink) -> El {
    let mut line = if phone { col() } else { row().flex_wrap() }
        .w_full()
        .gap(6.0);
    for (index, answer) in ask.suggestions.iter().enumerate() {
        line = line.child(chip(
            terminal,
            &ask.id,
            answer,
            keyed.then_some(index),
            busy,
            phone,
            ink,
        ));
    }
    line
}

fn header(dot: Color, title: String, detail: String, phone: bool, ink: &Ink) -> El {
    if phone {
        return col()
            .w_full()
            .gap(4.0)
            .child(
                text(title)
                    .w_full()
                    .font(TextStyle::new(15.0, 600.0))
                    .color(ink.text)
                    .truncate(),
            )
            .child(
                row()
                    .w_full()
                    .gap(6.0)
                    .items_center()
                    .child(el().size(6.0, 6.0).radius(3.0).fill(dot))
                    .child(text(detail).font(CAPTION).color(ink.dim)),
            );
    }
    row()
        .w_full()
        .gap(8.0)
        .items_center()
        .child(el().size(7.0, 7.0).radius(3.5).fill(dot))
        .child(text(title).font(LABEL_STRONG).color(ink.text).truncate())
        .child(el().grow(1.0))
        .child(text(detail).font(CAPTION).color(ink.dim))
}

fn waiting_card(desk: &Desk, waiting: &Waiting, picked: bool, ink: &Ink) -> El {
    let phone = desk.phone.is_some();
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
    let mut card = col().w_full().gap(8.0).child(header(
        mark_color(waiting.mark, ink),
        who,
        format!("{word}{age}"),
        phone,
        ink,
    ));
    let place = match &waiting.kind {
        Some(kind) if Some(kind) != waiting.agent.as_ref() => {
            format!("{} · {} · {kind}", waiting.label, waiting.session)
        }
        _ => format!("{} · {}", waiting.label, waiting.session),
    };
    card = card.child(text(place).font(CAPTION).color(ink.dim).truncate());
    let Some(ask) = &waiting.ask else {
        let hint = (waiting.mark == Mark::Blocked).then(|| {
            text("Waiting on you in its terminal.")
                .font(LABEL)
                .color(ink.text)
        });
        return card.children(hint);
    };
    card = card.child(
        text(ask.text.clone())
            .font(body_font(phone))
            .color(ink.text)
            .wrap(),
    );
    if !ask.typed() {
        return card.child(super::questions::panel(desk, &waiting.terminal, ask, ink));
    }
    if !ask.suggestions.is_empty() {
        card = card.child(chips(&waiting.terminal, ask, picked, busy, phone, ink));
    }
    if picked && !phone {
        card = card.child(reply(desk, ink));
    }
    card
}

fn reply(desk: &Desk, ink: &Ink) -> El {
    let phone = desk.phone.is_some();
    let mut input = text_input(
        &desk.inbox.reply,
        if phone {
            TOUCH_TEXT
        } else {
            TextStyle::new(13.0, 400.0).lh(18.0)
        },
    )
    .key("inbox-reply")
    .label("Reply to question")
    .single_line()
    .grow(1.0)
    .color(ink.text)
    .placeholder("Reply…", ink.dim)
    .on_edit(|d: &mut Desk| &mut d.inbox.reply)
    .on_input_key(|d: &mut Desk, event, tx| d.inbox_key(event, tx))
    .on_submit(|d: &mut Desk, tx| d.submit_picked(tx));
    if phone {
        input = input
            .min_h(44.0)
            .min_w(0.0)
            .py((44.0 - TOUCH_TEXT.line_px()) / 2.0);
    }
    row()
        .w_full()
        .px(10.0)
        .py(if phone { 0.0 } else { 7.0 })
        .radius(7.0)
        .fill(ink.separator)
        .child(input)
}

fn approval_card(approval: &Approval, phone: bool, ink: &Ink) -> El {
    let requester = inbox::approval_requester(approval);
    let mut card = col().w_full().gap(8.0).child(header(
        ink.danger,
        inbox::approval_title(approval),
        inbox::expiry(approval),
        phone,
        ink,
    ));
    card = card.child(
        text(format!("Held for {requester}"))
            .font(CAPTION)
            .color(ink.dim)
            .truncate(),
    );
    if let Some(credential) = &approval.credential {
        for detail in inbox::credential_details(approval) {
            card = card.child(text(detail).font(CAPTION).color(ink.text).wrap());
        }
        for reference in &credential.request.references {
            card = card.child(
                text(reference.clone())
                    .font(CAPTION.family(Family::SystemMono))
                    .color(ink.text)
                    .wrap(),
            );
        }
    }
    for subject in &approval.subjects {
        card = card.child(approval_subject(subject, phone, ink));
    }
    let decide = |decision, key: &'static str, label: &'static str, primary: bool| {
        let id = approval.id.clone();
        let mut button = pressable(format!("decide-{key}-{id}"), ink)
            .grow(1.0)
            .py(6.0)
            .radius(6.0)
            .center()
            .on_press(move |d: &mut Desk| d.decide(&id, decision))
            .child(
                text(label)
                    .font(if phone { TOUCH_TEXT } else { CAPTION })
                    .color(ink.text),
            );
        if phone {
            button = button.min_h(44.0).min_w(44.0);
        }
        if primary {
            button.fill(ink.accent.alpha(0.3))
        } else {
            button.border(1.0, ink.separator)
        }
    };
    card.child(
        row()
            .w_full()
            .gap(6.0)
            .child(decide(
                Decision::Deny,
                DENY,
                if phone { "Deny" } else { DENY },
                false,
            ))
            .child(decide(
                Decision::Approve,
                APPROVE,
                if phone { "Approve" } else { APPROVE },
                true,
            )),
    )
}

fn approval_subject(subject: &str, phone: bool, ink: &Ink) -> El {
    let label = text(subject.to_owned())
        .font(
            if phone {
                TextStyle::new(12.0, 400.0)
            } else {
                CAPTION
            }
            .family(Family::SystemMono),
        )
        .color(ink.text);
    if phone {
        label.w_full().wrap()
    } else {
        label.truncate()
    }
}

fn item_row(desk: &Desk, item: &Item, index: usize, picked: bool, ink: &Ink) -> El {
    let body = match item {
        Item::Approval(approval) => approval_card(approval, desk.phone.is_some(), ink),
        Item::Terminal(waiting) => waiting_card(desk, waiting, picked, ink),
    };
    let mut card = pressable(format!("inbox-item-{index}"), ink)
        .w_full()
        .padding(12.0)
        .radius(10.0)
        .fill(if picked {
            ink.selected
        } else {
            ink.text.alpha(0.025)
        })
        .on_press_tx(move |d: &mut Desk, tx| d.pick_item(index, tx))
        .child(body);
    if desk.phone.is_some() {
        card = card.radius(12.0).fill(ink.text.alpha(0.035)).border(
            1.0,
            if picked {
                ink.accent.alpha(0.4)
            } else {
                ink.separator
            },
        );
    }
    card
}

fn empty(ink: &Ink) -> El {
    col()
        .w_full()
        .py(48.0)
        .gap(6.0)
        .items_center()
        .child(
            text("✓")
                .font(TextStyle::new(22.0, 500.0))
                .color(mark_color(Mark::Succeeded, ink)),
        )
        .child(text("Nothing needs you").font(LABEL_STRONG).color(ink.text))
        .child(
            text("Asks, approvals and finished agents land here.")
                .font(CAPTION)
                .color(ink.dim),
        )
}

fn footer(desk: &Desk, ink: &Ink) -> El {
    let mut foot = row().w_full().px(14.0).py(8.0).gap(12.0).items_center();
    foot = match &desk.notice {
        Some(notice) => foot.child(notice_line(Some(notice), ink)),
        None if desk.phone.is_some() => return el(),
        None => foot
            .child(pick_hint("↑↓", "move", ink))
            .child(pick_hint("↩", "send or open", ink))
            .child(pick_hint("esc", "close", ink)),
    };
    foot
}

fn title(desk: &Desk, count: usize, ink: &Ink) -> El {
    let phone = desk.phone.is_some();
    let heading = text("Inbox")
        .font(TextStyle::new(if phone { 20.0 } else { 14.0 }, 650.0))
        .color(ink.text);
    let count = text(count.to_string()).font(CAPTION).color(ink.dim);
    let mut title = row()
        .w_full()
        .h(if desk.phone.is_some() { 56.0 } else { 48.0 })
        .px(16.0)
        .items_center()
        .justify_between();
    if phone {
        title = title.child(row().gap(10.0).items_center().child(heading).child(count));
        title = title.child(
            pressable("inbox-close", ink)
                .label("Close Inbox")
                .min_w(44.0)
                .h(44.0)
                .px(8.0)
                .radius(7.0)
                .center()
                .child(text("Close").font(TOUCH_TEXT).color(ink.accent))
                .on_press_tx(|d: &mut Desk, tx| {
                    // The same dismissal as Escape, including clearing the draft.
                    d.close_inbox();
                    tx.blur();
                }),
        );
    } else {
        title = title.child(heading).child(count);
    }
    title
}

/// On phones the active reply stays above the keyboard while questions scroll.
fn phone_reply(desk: &Desk, items: &[Item], picked: Option<usize>, ink: &Ink) -> Option<El> {
    desk.phone.as_ref()?;
    let Item::Terminal(waiting) = items.get(picked?)? else {
        return None;
    };
    if !waiting.ask.as_ref()?.typed() {
        return None;
    }
    Some(col().w_full().px(14.0).py(8.0).child(reply(desk, ink)))
}

pub fn view(desk: &Desk, ink: &Ink) -> El {
    let items = desk.inbox_items();
    let picked = desk.picked_index(&items);
    let mut list = col().key("inbox-list").w_full().padding(10.0).gap(8.0);
    for (index, item) in items.iter().enumerate() {
        list = list.child(item_row(desk, item, index, Some(index) == picked, ink));
    }
    if items.is_empty() {
        list = list.child(empty(ink));
    }
    let drawer = col()
        .key("inbox")
        // The phone form is narrower than the drawer.
        .w_full()
        .max_w(WIDTH)
        .h_full()
        .fill(ink.raised)
        .border(1.0, ink.separator)
        .shadow(18.0, 56.0, Color::BLACK.alpha(0.4))
        .transition(Transition::opacity())
        .child(title(desk, items.len(), ink))
        .child(rune::rule(1.0, ink.separator).w_full().key("inbox-header-rule"))
        .child(
            col()
                .key("inbox-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(rune::ScrollSpec {
                    stick_bottom: false,
                })
                .child(list),
        )
        .child(rune::rule(1.0, ink.separator).w_full().key("inbox-footer-rule"))
        .children(phone_reply(desk, &items, picked, ink))
        .child(footer(desk, ink));
    el().key("inbox-scrim")
        .cover()
        .fill(Color::BLACK.alpha(0.16))
        .transition(Transition::opacity())
        .modal(true, |d: &mut Desk| d.close_inbox())
        .label("Inbox")
        .child(row().w_full().h_full().child(el().grow(1.0)).child(drawer))
}

/// A visible inline question and its text-input ownership.
pub struct AskCard {
    pub content: El,
    /// Only a field in the card actually shown can take the terminal's IME.
    pub editing: bool,
}

fn inline_size(area: [f32; 2], phone: bool) -> Option<[f32; 2]> {
    let width = CARD_W.min(area[0] - 20.0);
    let height = area[1] - 20.0;
    if width < 160.0 || (phone && height < 64.0) {
        return None;
    }
    Some([width, height])
}

/// A focused pane's agent is asking: the question over the pane's corner,
/// answerable with a click. Hidden while find or the inbox covers it.
pub fn ask_card(desk: &Desk, id: &ResourceId, area: [f32; 2], ink: &Ink) -> Option<AskCard> {
    if desk.connection.find.is_some() || desk.inbox.open {
        return None;
    }
    let pane = desk.connection.topology.as_ref()?.pane(id)?;
    let ask = inbox::question(pane, &desk.activities).or_else(|| desk.inbox.questions.ask(id))?;
    let [width, height] = inline_size(area, desk.phone.is_some())?;
    let busy = desk.answering(id);
    let who = desk.activities.agent_name(id).unwrap_or("Agent").to_owned();
    let mut card = col()
        .key(format!("ask-{id}"))
        .absolute(super::PAD + area[0] - width - 10.0, super::PAD + 10.0)
        .w(width)
        .padding(12.0)
        .gap(8.0)
        .radius(10.0)
        .fill(ink.raised)
        .border(1.0, mark_color(Mark::Blocked, ink).alpha(0.5))
        .shadow(8.0, 24.0, Color::BLACK.alpha(0.35))
        .transition(Transition::scale(0.97).combined(Transition::opacity()))
        .child(header(
            mark_color(Mark::Blocked, ink),
            who,
            ask_hint(busy, ask.typed(), desk.phone.is_some()),
            desk.phone.is_some(),
            ink,
        ))
        .child(
            text(ask.text.clone())
                .font(body_font(desk.phone.is_some()))
                .color(ink.text)
                .wrap(),
        );
    if desk.phone.is_some() {
        card = card.max_h(height).scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        // Keep background/text presses here instead of the terminal's
        // immediate drag handler, so Rune can pan this scroll viewport.
            .role("group")
            .label("Agent question")
            .on_background_press(|_: &mut Desk| {});
    }
    Some(inline_answers(desk, id, &ask, card, ink))
}

fn inline_answers(desk: &Desk, id: &ResourceId, ask: &Ask, mut card: El, ink: &Ink) -> AskCard {
    if !ask.typed() {
        return AskCard {
            editing: desk.inbox.questions.field_focused(id, &ask.id),
            content: card.child(super::questions::panel(desk, id, ask, ink)),
        };
    }
    if !ask.suggestions.is_empty() {
        card = card.child(chips(
            id,
            ask,
            false,
            desk.answering(id),
            desk.phone.is_some(),
            ink,
        ));
    }
    AskCard {
        content: card,
        editing: false,
    }
}

fn ask_hint(busy: bool, typed: bool, phone: bool) -> String {
    match (busy, typed, phone) {
        (true, _, _) => "sending…".to_owned(),
        (false, true, true) => "Reply in Inbox".to_owned(),
        (false, true, false) => format!("{}I to reply", super::MOD),
        (false, false, _) => "ACP".to_owned(),
    }
}
