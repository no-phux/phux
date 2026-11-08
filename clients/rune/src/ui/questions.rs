//! An ACP agent's question, answerable in place: its options as buttons and
//! its fields as text, each sent only through the agent's protocol.

use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::{NativeOption, NativeQuestion};
use rune::{El, Family, TextEdit, TextStyle, col, row, text, text_input};
use serde_json::{Value, json};

use super::{CAPTION, Ink, LABEL, LABEL_STRONG, pressable};
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
pub fn panel(desk: &Desk, terminal: &ResourceId, ask: &Ask, ink: &Ink) -> El {
    let questions = &desk.inbox.questions;
    let Some((view, draft)) = questions.open(terminal, &ask.id) else {
        let known = questions.views.contains_key(terminal);
        return note(if known { CLOSED } else { UNLOADED }, ink);
    };
    let question = &draft.question;
    let at = At {
        terminal,
        question: &question.id,
        held: questions.held(view, &question.id),
    };
    let mut panel = col()
        .w_full()
        .gap(8.0)
        .role("group")
        .label(question.title.clone())
        .description(question.message.clone());
    if !question.title.is_empty() && question.title != ask.text {
        panel = panel.child(say(&question.title, LABEL_STRONG, ink));
    }
    if !question.message.is_empty() && question.message != ask.text {
        panel = panel.child(say(&question.message, LABEL, ink));
    }
    if let Some(details) = details(question) {
        let mono = CAPTION.family(Family::SystemMono);
        panel = panel.child(text(details).font(mono).color(ink.dim).wrap());
    }
    panel = controls(panel, draft, &at, ink);
    let mut actions = row().w_full().gap(6.0).flex_wrap();
    if matches!(question.kind.as_str(), "input" | "editor" | "ask" | "form") {
        actions = actions.child(action(&at, "submit", "Submit response", None, true, ink));
    }
    let cancel = Some(json!({"cancelled": true}));
    panel = panel.child(actions.child(action(&at, "cancel", "Cancel request", cancel, false, ink)));
    if !view.connected {
        panel = panel.child(note(
            "ACP controller disconnected. Nothing can be sent.",
            ink,
        ));
    } else if !view.capabilities.questions.is_supported() {
        panel = panel.child(note("This agent takes no answers over ACP.", ink));
    }
    if let Some(status) = questions.status.get(&question.id) {
        panel = panel.child(say(status, CAPTION, ink).role("status"));
    }
    if let Some(error) = questions.error.get(&question.id) {
        let line = text(error.clone()).font(CAPTION).color(ink.danger).wrap();
        panel = panel.child(line.role("alert"));
    }
    panel
}

fn say(words: &str, font: TextStyle, ink: &Ink) -> El {
    text(words.to_owned()).font(font).color(ink.text).wrap()
}

fn note(words: &str, ink: &Ink) -> El {
    text(words.to_owned()).font(CAPTION).color(ink.dim).wrap()
}

/// A permission request's raw fields, for knowing what is being allowed.
fn details(question: &NativeQuestion) -> Option<String> {
    let fields = question.fields.as_object()?;
    (question.kind == "permission" && !fields.is_empty())
        .then(|| serde_json::to_string_pretty(&question.fields).unwrap_or_default())
}

fn controls(mut panel: El, draft: &Draft, at: &At, ink: &Ink) -> El {
    let question = &draft.question;
    match question.kind.as_str() {
        "confirm" => panel.child(
            row()
                .w_full()
                .gap(6.0)
                .child(action(
                    at,
                    "yes",
                    "Confirm",
                    Some(json!({"confirmed": true})),
                    false,
                    ink,
                ))
                .child(action(
                    at,
                    "no",
                    "Decline",
                    Some(json!({"confirmed": false})),
                    false,
                    ink,
                )),
        ),
        "select" | "permission" => {
            let mut list = col().w_full().gap(6.0);
            for option in &question.options {
                let answer = Some(json!({"option_id": option.id}));
                let label = labelled(option);
                list = list.child(action(at, &option.id, &label, answer, false, ink));
            }
            panel.child(list)
        }
        "input" | "editor" => {
            let multiline = question.kind == "editor";
            panel.children(field(at, draft, "text", &question.title, multiline, ink))
        }
        "ask" => {
            for item in &question.questions {
                let multi = if item.multi { " (choose multiple)" } else { "" };
                let heading = format!("{} {}{multi}", item.header, item.question);
                panel = panel
                    .child(say(heading.trim(), CAPTION, ink))
                    .child(choices(at, draft, &item.id, &item.options, item.multi, ink));
                let label = format!("Custom response to {}", item.question);
                panel = panel.children(field(at, draft, &item.id, &label, false, ink));
            }
            panel
        }
        "form" => form(panel, draft, at, ink),
        _ => panel.child(note(UNSUPPORTED, ink)),
    }
}

fn form(mut panel: El, draft: &Draft, at: &At, ink: &Ink) -> El {
    let question = &draft.question;
    let Some(properties) = question.fields.get("properties").and_then(Value::as_object) else {
        return panel.child(note(
            "Unsupported provider form schema; no response submitted.",
            ink,
        ));
    };
    for (id, schema) in properties {
        let title = schema.get("title").and_then(Value::as_str).unwrap_or(id);
        let star = if response::required(question, id) {
            " *"
        } else {
            ""
        };
        panel = panel.child(say(&format!("{title}{star}"), CAPTION, ink));
        if let Some(about) = schema.get("description").and_then(Value::as_str) {
            panel = panel.child(note(about, ink));
        }
        panel = match draft.options.get(id) {
            Some(offered) => {
                let multi = schema.get("type").and_then(Value::as_str) == Some("array");
                panel.child(choices(at, draft, id, offered, multi, ink))
            }
            None => panel.children(field(at, draft, id, title, false, ink)),
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

/// A button that answers (`response`) or submits the draft (`None`).
fn action(at: &At, id: &str, label: &str, answer: Option<Value>, primary: bool, ink: &Ink) -> El {
    let button = pressable(format!("acp-{}-{id}", at.question), ink)
        .px(10.0)
        .py(5.0)
        .radius(6.0)
        .center()
        .role("button")
        .label(label.to_owned())
        .disabled(at.held)
        .child(say(label, CAPTION, ink).a11y_hidden());
    let button = if primary {
        button.fill(ink.accent.alpha(0.3))
    } else {
        button.border(1.0, ink.separator)
    };
    if at.held {
        return button.opacity(0.5);
    }
    let (terminal, question) = (at.terminal.clone(), at.question.to_owned());
    button.on_press(move |d: &mut Desk| d.answer_question(&terminal, &question, answer.clone()))
}

/// The options offered for one item or field. None starts chosen, not
/// even the provider's recommendation.
fn choices(
    at: &At,
    draft: &Draft,
    item: &str,
    offered: &[NativeOption],
    multi: bool,
    ink: &Ink,
) -> El {
    let chosen = draft.choices.get(item).map(Vec::as_slice).unwrap_or(&[]);
    let mut line = row().w_full().gap(6.0).flex_wrap();
    for option in offered {
        let on = chosen.contains(&option.id);
        let label = labelled(option);
        let chip = row()
            .key(format!("acp-{}-{item}-{}", at.question, option.id))
            .px(9.0)
            .py(4.0)
            .radius(6.0)
            .items_center()
            .role(if multi { "checkbox" } else { "radio" })
            .label(label.clone())
            .checked(on)
            .disabled(at.held)
            .border(
                1.0,
                if on {
                    ink.accent
                } else {
                    ink.accent.alpha(0.4)
                },
            )
            .child(say(&label, CAPTION, ink).a11y_hidden());
        let chip = if on {
            chip.fill(ink.accent.alpha(0.24))
        } else {
            chip
        };
        line = line.child(if at.held {
            chip.opacity(0.5)
        } else {
            let (question, item, id) = (at.question.to_owned(), item.to_owned(), option.id.clone());
            chip.focusable(true)
                .hover(ink.accent.alpha(0.16))
                .cursor(rune::Cursor::Pointer)
                .on_press(move |d: &mut Desk| {
                    d.inbox.questions.choose(&question, &item, &id, multi)
                })
        });
    }
    line
}

/// A text field, when the draft has one by `id`. Return submits, except in
/// an editor, where it breaks the line.
fn field(at: &At, draft: &Draft, id: &str, label: &str, multiline: bool, ink: &Ink) -> Option<El> {
    let edit: &TextEdit = draft.texts.get(id)?;
    let mut input = text_input(edit, TextStyle::new(13.0, 400.0).lh(18.0))
        .key(format!("acp-{}-field-{id}", at.question))
        .grow(1.0)
        .color(ink.text)
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
    Some(
        row()
            .w_full()
            .px(10.0)
            .py(7.0)
            .radius(7.0)
            .fill(ink.separator)
            .child(input),
    )
}
