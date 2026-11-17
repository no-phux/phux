//! An ACP agent's question, answerable in place: its options as kit radios,
//! checkboxes and buttons, its fields as text, each sent only through the
//! agent's protocol.

use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::{NativeOption, NativeQuestion};
use rune::{El, TextEdit, TextStyle, col, row, text, text_input};
use rune_kit::{Button, ButtonVariant, Checkbox, RadioGroup, RadioItem};
use serde_json::{Value, json};

use super::inbox::field_shell;
use super::overlay::strong;
use super::tokens::{MONO, Paint, Tokens};
use crate::activity::Ask;
use crate::desk::Desk;
use crate::desk::questions::{CLOSED, Draft, response};

/// The agent session's view isn't here: only a streamed pane brings it.
const UNLOADED: &str = "Show its pane to load this ACP question.";
const UNSUPPORTED: &str =
    "Unsupported native request type. No approval or terminal fallback will be sent.";

/// Where a control's press goes.
struct At<'a> {
    terminal: &'a ResourceId,
    question: &'a str,
    held: bool,
}

/// An ACP ask on `terminal`: its question's controls, or why there are none.
pub fn panel(desk: &Desk, terminal: &ResourceId, ask: &Ask, p: &impl AsRef<Tokens>) -> El {
    let t = p.as_ref();
    let questions = &desk.inbox.questions;
    let Some((view, draft)) = questions.open(terminal, &ask.id) else {
        let known = questions.views.contains_key(terminal);
        return note(if known { CLOSED } else { UNLOADED }, t);
    };
    let question = &draft.question;
    let at = At {
        terminal,
        question: &question.id,
        held: questions.held(view, &question.id),
    };
    let mut panel = col()
        .w_full()
        .gap(t.space.s2)
        .role("group")
        .label(question.title.clone())
        .description(question.message.clone());
    if !question.title.is_empty() && question.title != ask.text {
        panel = panel.child(say(&question.title, strong(t.ty.label), t));
    }
    if !question.message.is_empty() && question.message != ask.text {
        panel = panel.child(say(&question.message, t.ty.label, t));
    }
    if let Some(details) = details(question) {
        let mono = TextStyle {
            family: MONO,
            ..t.ty.caption
        };
        panel = panel.child(
            text(details)
                .font(mono)
                .color(t.roles.text.dim.paint())
                .wrap(),
        );
    }
    panel = controls(panel, draft, &at, t);
    let mut actions = row().w_full().gap(t.space.icon_gap).flex_wrap();
    if matches!(question.kind.as_str(), "input" | "editor" | "ask" | "form") {
        actions = actions.child(action(
            &at,
            "submit",
            "Submit response",
            None,
            Kind::Primary,
            t,
        ));
    }
    let cancel = Some(json!({"cancelled": true}));
    panel = panel.child(actions.child(action(
        &at,
        "cancel",
        "Cancel request",
        cancel,
        Kind::Quiet,
        t,
    )));
    if !view.connected {
        panel = panel.child(note("ACP controller disconnected. Nothing can be sent.", t));
    } else if !view.capabilities.questions.is_supported() {
        panel = panel.child(note("This agent takes no answers over ACP.", t));
    }
    if let Some(status) = questions.status.get(&question.id) {
        panel = panel.child(say(status, t.ty.caption, t).role("status"));
    }
    if let Some(error) = questions.error.get(&question.id) {
        // The invalid state's message: the failed tone, caption type.
        let line = text(error.clone())
            .font(t.ty.caption)
            .color(t.roles.status.failed.paint())
            .wrap();
        panel = panel.child(line.role("alert"));
    }
    panel
}

fn say(words: &str, font: TextStyle, t: &Tokens) -> El {
    text(words.to_owned())
        .font(font)
        .color(t.roles.text.text.paint())
        .wrap()
}

fn note(words: &str, t: &Tokens) -> El {
    text(words.to_owned())
        .font(t.ty.caption)
        .color(t.roles.text.dim.paint())
        .wrap()
}

/// A permission request's raw fields, for knowing what is being allowed.
fn details(question: &NativeQuestion) -> Option<String> {
    let fields = question.fields.as_object()?;
    (question.kind == "permission" && !fields.is_empty())
        .then(|| serde_json::to_string_pretty(&question.fields).unwrap_or_default())
}

fn controls(mut panel: El, draft: &Draft, at: &At, t: &Tokens) -> El {
    let question = &draft.question;
    match question.kind.as_str() {
        "confirm" => panel.child(
            row()
                .w_full()
                .gap(t.space.icon_gap)
                .child(action(
                    at,
                    "yes",
                    "Confirm",
                    Some(json!({"confirmed": true})),
                    Kind::Primary,
                    t,
                ))
                .child(action(
                    at,
                    "no",
                    "Decline",
                    Some(json!({"confirmed": false})),
                    Kind::Quiet,
                    t,
                )),
        ),
        // Each option answers at once: a column of buttons.
        "select" | "permission" => {
            let mut list = col().w_full().gap(t.space.icon_gap);
            for option in &question.options {
                let answer = Some(json!({"option_id": option.id}));
                let label = labelled(option);
                list = list.child(action(at, &option.id, &label, answer, Kind::Quiet, t));
            }
            panel.child(list)
        }
        "input" | "editor" => {
            let multiline = question.kind == "editor";
            panel.children(field(at, draft, "text", &question.title, multiline, t))
        }
        "ask" => {
            for item in &question.questions {
                let multi = if item.multi { " (choose multiple)" } else { "" };
                let heading = format!("{} {}{multi}", item.header, item.question);
                panel = panel
                    .child(say(heading.trim(), t.ty.caption, t))
                    .child(choices(at, draft, &item.id, &item.options, item.multi, t));
                let label = format!("Custom response to {}", item.question);
                panel = panel.children(field(at, draft, &item.id, &label, false, t));
            }
            panel
        }
        "form" => form(panel, draft, at, t),
        _ => panel.child(note(UNSUPPORTED, t)),
    }
}

fn form(mut panel: El, draft: &Draft, at: &At, t: &Tokens) -> El {
    let question = &draft.question;
    let Some(properties) = question.fields.get("properties").and_then(Value::as_object) else {
        return panel.child(note(
            "Unsupported provider form schema; no response submitted.",
            t,
        ));
    };
    for (id, schema) in properties {
        let title = schema.get("title").and_then(Value::as_str).unwrap_or(id);
        let star = if response::required(question, id) {
            " *"
        } else {
            ""
        };
        panel = panel.child(say(&format!("{title}{star}"), t.ty.caption, t));
        if let Some(about) = schema.get("description").and_then(Value::as_str) {
            panel = panel.child(note(about, t));
        }
        panel = match draft.options.get(id) {
            Some(offered) => {
                let multi = schema.get("type").and_then(Value::as_str) == Some("array");
                panel.child(choices(at, draft, id, offered, multi, t))
            }
            None => panel.children(field(at, draft, id, title, false, t)),
        };
    }
    panel
}

fn labelled(option: &NativeOption) -> String {
    match &option.description {
        Some(detail) => format!("{} — {detail}", option.label),
        None => option.label.clone(),
    }
}

/// Which button an answer is: the one primary action, or a quiet one.
#[derive(Clone, Copy)]
enum Kind {
    Primary,
    Quiet,
}

/// A button that answers (`response`) or submits the draft (`None`). While
/// the question is held, disabled (the kit's look), never hidden.
fn action(at: &At, id: &str, label: &str, answer: Option<Value>, kind: Kind, t: &Tokens) -> El {
    let variant = match kind {
        Kind::Primary => ButtonVariant::Submit,
        Kind::Quiet => ButtonVariant::Outline,
    };
    let (terminal, question) = (at.terminal.clone(), at.question.to_owned());
    Button::new(label)
        .variant(variant)
        .disabled(at.held)
        .on_press(move |d: &mut Desk| d.answer_question(&terminal, &question, answer.clone()))
        .build(&t.kit)
        .key(format!("acp-{}-{id}", at.question))
}

/// The options offered for one item or field: radios for one choice,
/// checkboxes for several. None starts chosen, not even the provider's
/// recommendation.
fn choices(
    at: &At,
    draft: &Draft,
    item: &str,
    offered: &[NativeOption],
    multi: bool,
    t: &Tokens,
) -> El {
    let chosen = draft.choices.get(item).map(Vec::as_slice).unwrap_or(&[]);
    let pick = |option: &NativeOption| {
        let (question, item, id) = (at.question.to_owned(), item.to_owned(), option.id.clone());
        move |d: &mut Desk| d.inbox.questions.choose(&question, &item, &id, multi)
    };
    if multi {
        let mut list = col().w_full().gap(t.space.s2).role("group");
        for option in offered {
            let on = chosen.contains(&option.id);
            let mut check = Checkbox::new(on).label(option.label.clone());
            if let Some(detail) = &option.description {
                check = check.description(detail.clone());
            }
            list = list.child(
                check
                    .disabled(at.held)
                    .on_toggle(pick(option))
                    .build(&t.kit)
                    .key(format!("acp-{}-{item}-{}", at.question, option.id)),
            );
        }
        return list;
    }
    let mut group = RadioGroup::new().disabled(at.held);
    for option in offered {
        let on = chosen.contains(&option.id);
        let mut radio = RadioItem::new(option.label.clone(), on).on_select(pick(option));
        if let Some(detail) = &option.description {
            radio = radio.description(detail.clone());
        }
        group = group.item(radio);
    }
    group
        .build(&t.kit)
        .key(format!("acp-{}-{item}", at.question))
}

/// A text field, when the draft has one by `id`. Return submits, except in
/// an editor, where it breaks the line.
fn field(at: &At, draft: &Draft, id: &str, label: &str, multiline: bool, t: &Tokens) -> Option<El> {
    let r = &t.roles;
    let edit: &TextEdit = draft.texts.get(id)?;
    let ink = if at.held { r.text.faint } else { r.text.text };
    let mut input = text_input(edit, t.ty.body)
        .key(format!("acp-{}-field-{id}", at.question))
        .grow(1.0)
        .color(ink.paint())
        .label(label.to_owned())
        .disabled(at.held);
    if !multiline {
        input = input.single_line();
    }
    if !at.held {
        let (question, id) = (at.question.to_owned(), id.to_owned());
        let edits = (question.clone(), id.clone());
        input = input.on_edit(move |d: &mut Desk| d.inbox.questions.text_mut(&edits.0, &edits.1));
        let terminal = at.terminal.clone();
        input = input.on_submit(move |d: &mut Desk, _| {
            if multiline {
                d.inbox.questions.text_mut(&question, &id).insert("\n");
            } else {
                d.answer_question(&terminal, &question, None);
            }
        });
    }
    Some(field_shell(input, t, !at.held))
}

#[cfg(test)]
mod tests;
