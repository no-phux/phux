//! The inbox: a drawer of everything waiting on you, answerable in place,
//! and the card a focused pane shows when its agent is asking.

use std::time::{SystemTime, UNIX_EPOCH};

use phux_client_runtime::approvals::{Approval, Decision};
use phux_protocol::ids::ResourceId;
use rune::{Color, El, Family, TextStyle, Transition, col, el, row, text, text_input};

use super::sidebar::mark_color;
use super::{CAPTION, Ink, LABEL, LABEL_STRONG};
use crate::activity::{Ask, Mark};
use crate::desk::Desk;
use crate::inbox::{self, Item, Waiting};

const WIDTH: f32 = 400.0;
const CARD_W: f32 = 340.0;

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
    ink: &Ink,
) -> El {
    let (terminal, question, send) = (terminal.clone(), question.to_owned(), answer.to_owned());
    let chip = row()
        .key(format!("chip-{terminal}-{answer}"))
        .px(9.0)
        .py(4.0)
        .gap(6.0)
        .radius(6.0)
        .items_center()
        .border(1.0, ink.accent.alpha(0.5))
        .hover(ink.accent.alpha(0.16))
        .cursor(rune::Cursor::Pointer)
        .child(text(answer.to_owned()).font(CAPTION).color(ink.text))
        .children(index.filter(|i| *i < 9).map(|i| {
            text(format!("{}{}", super::MOD, i + 1))
                .font(CAPTION)
                .color(ink.dim)
        }));
    if busy {
        return chip.opacity(0.5);
    }
    chip.on_press(move |d: &mut Desk| d.answer(&terminal, &question, &send))
}

fn chips(terminal: &ResourceId, ask: &Ask, keyed: bool, busy: bool, ink: &Ink) -> El {
    let mut line = row().w_full().gap(6.0).flex_wrap();
    for (index, answer) in ask.suggestions.iter().enumerate() {
        line = line.child(chip(
            terminal,
            &ask.id,
            answer,
            keyed.then_some(index),
            busy,
            ink,
        ));
    }
    line
}

fn header(dot: Color, title: String, detail: String, ink: &Ink) -> El {
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
    let who = waiting
        .agent
        .clone()
        .unwrap_or_else(|| waiting.label.clone());
    let age = waiting
        .ask
        .as_ref()
        .and_then(|ask| ask.since)
        .map(|since| format!(" · {}", inbox::age(since.elapsed().as_secs())))
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
        ink,
    ));
    card = card.child(
        text(format!("{} · {}", waiting.label, waiting.session))
            .font(CAPTION)
            .color(ink.dim)
            .truncate(),
    );
    let Some(ask) = &waiting.ask else {
        let hint = (waiting.mark == Mark::Blocked).then(|| {
            text("Waiting on you in its terminal.")
                .font(LABEL)
                .color(ink.text)
        });
        return card.children(hint);
    };
    card = card.child(text(ask.text.clone()).font(LABEL).color(ink.text).wrap());
    if !ask.suggestions.is_empty() {
        card = card.child(chips(&waiting.terminal, ask, picked, busy, ink));
    }
    if picked {
        card = card.child(
            row()
                .w_full()
                .px(10.0)
                .py(7.0)
                .radius(7.0)
                .fill(ink.separator)
                .child(
                    text_input(&desk.inbox.reply, TextStyle::new(13.0, 400.0).lh(18.0))
                        .key("inbox-reply")
                        .single_line()
                        .grow(1.0)
                        .color(ink.text)
                        .placeholder("Reply…", ink.dim)
                        .on_edit(|d: &mut Desk| &mut d.inbox.reply)
                        .on_input_key(|d: &mut Desk, event, tx| d.inbox_key(event, tx))
                        .on_submit(|d: &mut Desk, tx| d.submit_picked(tx)),
                ),
        );
    }
    card
}

fn approval_card(approval: &Approval, ink: &Ink) -> El {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |t| t.as_millis() as u64);
    let left = approval.expires_at_ms.saturating_sub(now_ms) / 1000;
    let requester = inbox::approval_requester(approval);
    let mut card = col().w_full().gap(8.0).child(header(
        ink.danger,
        inbox::approval_title(approval),
        format!("expires in {}", inbox::age(left)),
        ink,
    ));
    card = card.child(
        text(format!("Held for {requester}"))
            .font(CAPTION)
            .color(ink.dim)
            .truncate(),
    );
    for subject in &approval.subjects {
        card = card.child(
            text(subject.clone())
                .font(CAPTION.family(Family::SystemMono))
                .color(ink.text)
                .truncate(),
        );
    }
    let decide = |decision, label: &'static str, primary: bool| {
        let id = approval.id.clone();
        let button = el()
            .key(format!("decide-{label}-{id}"))
            .grow(1.0)
            .py(6.0)
            .radius(6.0)
            .center()
            .hover(ink.hover)
            .cursor(rune::Cursor::Pointer)
            .on_press(move |d: &mut Desk| d.decide(&id, decision))
            .child(text(label).font(CAPTION).color(ink.text));
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
                if cfg!(target_os = "macos") {
                    "Deny  ⌘⌫"
                } else {
                    "Deny"
                },
                false,
            ))
            .child(decide(
                Decision::Approve,
                if cfg!(target_os = "macos") {
                    "Approve  ⌘↩"
                } else {
                    "Approve"
                },
                true,
            )),
    )
}

fn item_row(desk: &Desk, item: &Item, index: usize, picked: bool, ink: &Ink) -> El {
    let body = match item {
        Item::Approval(approval) => approval_card(approval, ink),
        Item::Terminal(waiting) => waiting_card(desk, waiting, picked, ink),
    };
    el().key(format!("inbox-item-{index}"))
        .w_full()
        .padding(12.0)
        .radius(10.0)
        .fill(if picked {
            ink.selected
        } else {
            ink.text.alpha(0.025)
        })
        .hover(ink.hover)
        .cursor(rune::Cursor::Pointer)
        .on_press_tx(move |d: &mut Desk, tx| d.pick_item(index, tx))
        .child(body)
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
    let mut foot = row().w_full().px(14.0).py(8.0).gap(12.0).items_center();
    foot = match &desk.notice {
        Some(notice) => foot.child(
            text(notice.clone())
                .font(CAPTION)
                .color(ink.danger)
                .truncate(),
        ),
        None => foot
            .child(pick_hint("↑↓", "move", ink))
            .child(pick_hint("↩", "send or open", ink))
            .child(pick_hint("esc", "close", ink)),
    };
    let drawer = col()
        .key("inbox")
        .w(WIDTH)
        .h_full()
        .fill(ink.raised)
        .border(1.0, ink.separator)
        .shadow(18.0, 56.0, Color::BLACK.alpha(0.4))
        .transition(Transition::opacity())
        .child(
            row()
                .w_full()
                .h(48.0)
                .px(16.0)
                .items_center()
                .justify_between()
                .child(
                    text("Inbox")
                        .font(TextStyle::new(14.0, 650.0))
                        .color(ink.text),
                )
                .child(text(items.len().to_string()).font(CAPTION).color(ink.dim)),
        )
        .child(el().w_full().h(1.0).fill(ink.separator))
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
        .child(el().w_full().h(1.0).fill(ink.separator))
        .child(foot);
    el().key("inbox-scrim")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .fill(Color::BLACK.alpha(0.16))
        .transition(Transition::opacity())
        .modal(true, |d: &mut Desk| d.close_inbox())
        .child(row().w_full().h_full().child(el().grow(1.0)).child(drawer))
}

/// A focused pane's agent is asking: the question over the pane's corner,
/// answerable with a click. Hidden while find or the inbox covers it.
pub fn ask_card(desk: &Desk, id: &ResourceId, area: [f32; 2], ink: &Ink) -> Option<El> {
    if desk.find.is_some() || desk.inbox.open {
        return None;
    }
    let pane = desk.topology.as_ref()?.pane(id)?;
    let ask = inbox::question(pane, &desk.activities)?;
    let width = CARD_W.min(area[0] - 20.0);
    if width < 160.0 {
        return None;
    }
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
            if busy {
                "sending…".to_owned()
            } else {
                format!("{}I to reply", super::MOD)
            },
            ink,
        ))
        .child(text(ask.text.clone()).font(LABEL).color(ink.text).wrap());
    if !ask.suggestions.is_empty() {
        card = card.child(chips(id, &ask, false, busy, ink));
    }
    Some(card)
}
