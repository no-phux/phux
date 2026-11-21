//! The inbox: a drawer of everything waiting on you, answerable in place,
//! and the card a focused pane shows when its agent is asking.

use crate::ui::tokens::Voice;
use phux_client_runtime::approvals::{Approval, Decision};
use phux_design::{State, Tone};
use phux_protocol::ids::ResourceId;
use rune::{Cursor, Edge, El, TextStyle, Transition, col, el, row, text};
use rune_kit::{Button, ButtonVariant, Card, ControlSize, Spinner};

use super::overlay::{EXTENTS, Glass, glass, key_hint, notice_words};
use super::tokens::{Paint, Tier, Tokens};
use super::{MOD, badge, icons, lifted, pressable};
use crate::activity::{Ask, Mark};
use crate::desk::{Desk, NoticeKind};
use crate::inbox::{self, Item, Waiting};

mod inline;
mod reply;
pub use inline::ask_card;

/// What an ask says: the label role on the desktop, the body role on a
/// phone, where it is read at arm's length.
fn words_font(t: &Tokens, phone: bool) -> TextStyle {
    if phone { t.ty.body } else { t.ty.label }
}

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

/// One suggested answer (an OWN chip): the selected wash inside the
/// control outline. `index` is its ⌘ digit, while the row is picked. While
/// the answer sends, every chip is disabled, not hidden. On
/// a phone it is a full-width touch target whose answer wraps.
fn chip(
    terminal: &ResourceId,
    question: &str,
    answer: &str,
    index: Option<usize>,
    busy: bool,
    phone: bool,
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
        .px(s.s2)
        .gap(s.icon_gap)
        .radius(t.radius.sm)
        .items_center()
        .border(s.hairline, edge.paint());
    let chip = if phone {
        chip.w_full().min_h(s.touch_min).py(s.s1).child(
            text(answer.to_owned())
                .w_full()
                .font(t.ty.body)
                .color(ink.paint())
                .wrap()
                .a11y_hidden(),
        )
    } else {
        chip.min_h(s.pointer_min)
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
            }))
    };
    if busy {
        return chip.disabled(true);
    }
    t.kit
        .interactive(chip.fill(r.wash.selected.paint()))
        .focusable(true)
        .cursor(Cursor::Pointer)
        .on_press(move |d: &mut Desk| d.answer(&terminal, &question, &send))
}

fn chips(terminal: &ResourceId, ask: &Ask, keyed: bool, busy: bool, phone: bool, t: &Tokens) -> El {
    let line = if phone { col() } else { row().flex_wrap() };
    let mut line = line.w_full().gap(t.space.icon_gap);
    for (index, answer) in ask.suggestions.iter().enumerate() {
        line = line.child(chip(
            terminal,
            &ask.id,
            answer,
            keyed.then_some(index),
            busy,
            phone,
            t,
        ));
    }
    line
}

/// A card's top line: the mark, who, and the state word with its age. A
/// send in flight shows the kit spinner beside its word. On a phone the
/// word drops under the name, so a long name keeps its width.
fn header(mark: Mark, title: String, detail: String, busy: bool, phone: bool, t: &Tokens) -> El {
    let r = &t.roles;
    let (icon, _) = mark_icon(mark, t);
    let name = text(title)
        .font(t.ty.label.semibold())
        .color(r.text.text.paint())
        .truncate()
        .min_w(0.0)
        .shrink();
    let spinner = busy.then(|| {
        Spinner::new()
            .size(t.icon.size.sm)
            .color(r.text.dim.paint())
            .build(&t.kit)
    });
    let detail = text(detail).font(t.ty.caption).color(r.text.dim.paint());
    if phone {
        return col()
            .w_full()
            .gap(t.space.s1)
            .child(
                row()
                    .w_full()
                    .gap(t.space.s2)
                    .items_center()
                    .child(icon)
                    .child(name),
            )
            .child(
                row()
                    .w_full()
                    .gap(t.space.s2)
                    .items_center()
                    .children(spinner)
                    .child(detail),
            );
    }
    row()
        .w_full()
        .gap(t.space.s2)
        .items_center()
        .child(icon)
        .child(name)
        .child(el().grow(1.0))
        .children(spinner)
        .child(detail)
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

/// A text input that is a full touch target on a phone: at least the touch
/// minimum tall, its line centred in it, and free to narrow.
pub(super) fn touch_input(input: El, font: TextStyle, phone: bool, t: &Tokens) -> El {
    if !phone {
        return input;
    }
    let s = &t.space;
    input
        .min_h(s.touch_min)
        .min_w(0.0)
        .py((s.touch_min - font.line_px()) / 2.0)
}

fn waiting_card(desk: &Desk, waiting: &Waiting, picked: bool, t: &Tokens) -> El {
    let r = &t.roles;
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
        .child(header(
            waiting.mark,
            who,
            format!("{word}{age}"),
            busy,
            phone,
            t,
        ))
        .child(
            text(place)
                .font(t.ty.caption)
                .color(r.text.dim.paint())
                .truncate(),
        );
    let Some(ask) = &waiting.ask else {
        let hint = (waiting.mark == Mark::Blocked).then(|| {
            text("Waiting on you in its terminal.")
                .font(words_font(t, phone))
                .color(r.text.text.paint())
                .wrap()
        });
        return card(body.children(hint), tone, t);
    };
    body = body.child(
        text(ask.text.clone())
            .font(words_font(t, phone))
            .color(r.text.text.paint())
            .wrap(),
    );
    if !ask.typed() {
        let panel = super::questions::panel(desk, &waiting.terminal, ask, t);
        return card(body.child(panel), tone, t);
    }
    if !ask.suggestions.is_empty() {
        body = body.child(chips(&waiting.terminal, ask, picked, busy, phone, t));
    }
    // A phone pins the reply under the list, above the keyboard.
    if picked && !phone {
        body = body.child(reply::reply(desk, t));
    }
    card(body, tone, t)
}

/// The kit card: a rule in the item's tone beside its content.
fn card(body: El, tone: rune::Color, t: &Tokens) -> El {
    Card::new().accent(tone).child(body).build(&t.kit)
}

/// An approval's subject: one truncated line on the desktop; on a phone,
/// wrapped, so the whole target is readable.
fn approval_subject(subject: &str, phone: bool, t: &Tokens) -> El {
    let line = text(subject.to_owned())
        .font(t.ty.mono_caption)
        .color(t.roles.text.text.paint());
    if phone {
        line.w_full().wrap()
    } else {
        line.truncate()
    }
}

fn approval_card(approval: &Approval, phone: bool, t: &Tokens) -> El {
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
            phone,
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
        body = body.child(approval_subject(subject, phone, t));
    }
    // Deny is destructive; Approve is the one primary action. A
    // phone has no chords to show, and each is a full touch target.
    let decide = |decision, label: &'static str, chord: &'static str, variant| {
        let id = approval.id.clone();
        let key = format!("decide-{label}-{id}");
        let button = Button::new(label)
            .variant(variant)
            .control_size(ControlSize::Large);
        let button = if phone {
            button
        } else {
            button.shortcut(format!("{MOD}{chord}"))
        };
        let button = button
            .on_press(move |d: &mut Desk| d.decide(&id, decision))
            .build(&t.kit)
            .key(key)
            .grow(1.0);
        if phone {
            button.min_h(t.space.touch_min).min_w(t.space.touch_min)
        } else {
            button
        }
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
        Item::Approval(approval) => approval_card(approval, desk.phone.is_some(), t),
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

/// The drawer's foot: a notice, or the keys that apply. A phone has no
/// keys to hint, so without a notice it has no foot.
fn footer(desk: &Desk, empty: bool, t: &Tokens) -> El {
    let s = &t.space;
    let foot = row().w_full().px(s.s3).py(s.s2).gap(s.s3).items_center();
    match &desk.notice {
        // A phone's confirmation sits in the fixed header (`title`).
        Some(notice) if desk.phone.is_some() && notice.kind == NoticeKind::Info => el(),
        Some(notice) => foot.child(notice_words(notice, t)),
        None if desk.phone.is_some() => el(),
        // Only the hints that apply: nothing to move to or open when empty.
        None if empty => foot.child(key_hint("esc", "close", t)),
        None => foot
            .child(key_hint("↑↓", "move", t))
            .child(key_hint("↩", "send or open", t))
            .child(key_hint("esc", "close", t)),
    }
}

/// The drawer's title and count. A phone has no Escape key, so it also
/// shows Close, a full touch target that dismisses as Escape does.
fn title(desk: &Desk, count: usize, t: &Tokens) -> El {
    let s = &t.space;
    let r = &t.roles;
    let heading = text("Inbox").font(t.ty.title).color(r.text.text.paint());
    let count = text(count.to_string())
        .font(t.ty.readout)
        .color(r.text.dim.paint());
    let title = row()
        .w_full()
        .h(s.s12)
        .px(s.s4)
        .items_center()
        .justify_between();
    if desk.phone.is_none() {
        return title.child(heading).child(count);
    }
    let close = pressable("inbox-close", t)
        .role("button")
        .label("Close Inbox")
        .min_w(s.touch_min)
        .h(s.touch_min)
        .px(s.s2)
        .radius(t.radius.sm)
        .center()
        .child(
            text("Close")
                .font(t.ty.body)
                .color(r.accent.accent_text.paint())
                .a11y_hidden(),
        )
        .on_press_tx(|d: &mut Desk, tx| {
            // The same dismissal as Escape, including clearing the draft.
            d.close_inbox();
            tx.blur();
        });
    let mut leading = col()
        .grow(1.0)
        .min_w(0.0)
        .gap(s.s1)
        .child(row().gap(s.s2).items_center().child(heading).child(count));
    // Confirmations stay in the fixed header, so they do not displace the
    // question above a keyboard; errors wrap in the footer.
    if let Some(notice) = &desk.notice
        && notice.kind == NoticeKind::Info
    {
        leading = leading.child(
            text(notice.text.clone())
                .key("inbox-receipt-status")
                .role("status")
                .font(t.ty.caption)
                .color(r.text.text.paint())
                .w_full()
                .truncate(),
        );
    }
    title.gap(s.s2).child(leading).child(close)
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
        .child(title(desk, items.len(), t))
        .child(rule("inbox-header-rule"))
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
        .child(rule("inbox-footer-rule"))
        .children(reply::phone_reply(desk, &items, picked, t))
        .child(footer(desk, items.is_empty(), t));
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

#[cfg(test)]
mod tests;
