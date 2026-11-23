//! Real-touch regressions for action()'s held phone branch. No GPU/exporting.
//! Each control/reason/kind starts fresh; Cancel never inherits Submit's error.
use super::*;

#[derive(Clone, Copy, Debug)]
enum Held {
    Submitting,
    Disconnected,
    Unsupported,
}

const REASONS: [Held; 3] = [Held::Submitting, Held::Disconnected, Held::Unsupported];
const ACTIONS: [(&str, &str); 7] = [
    ("confirm", "yes"),
    ("select", "one"),
    ("permission", "one"),
    ("input", "submit"),
    ("editor", "submit"),
    ("ask", "submit"),
    ("form", "submit"),
];

fn write_field(rt: &mut Runtime<Desk>, id: &str) {
    let part = format!("field-{id}");
    let key = key(rt, &part);
    show(rt, &part, true);
    assert!(rt.accessibility_action(&key, rune::accesskit::Action::Focus, None));
    settle(rt);
    rt.ime(rune::Ime::Commit(NOTE.into()));
    settle(rt);
    assert_eq!(draft(&rt.state).text(id), NOTE);
}

/// Prepare valid responses through real handlers so an accidental Submit
/// cannot be mistaken for harmless validation of an incomplete draft.
fn ready(view: View, kind: &str) -> (Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = fixture((view.size.0 as f32, view.size.1 as f32), kind);
    match kind {
        "input" | "editor" => write_field(&mut rt, "text"),
        "ask" => {
            activate(&mut rt, "false-one");
            activate(&mut rt, "true-one");
            write_field(&mut rt, "false");
        }
        "form" => {
            select(&mut rt, true);
            write_field(&mut rt, "note");
        }
        _ => {}
    }
    assert!(rt.state.inbox.questions.error.is_empty());
    assert!(rt.state.inbox.questions.pending.is_empty());
    no_input(&client);
    (client, rt)
}

fn hold(rt: &mut Runtime<Desk>, reason: Held) {
    rt.update(|d, tx| {
        let terminal = d.connection.shown.as_ref().unwrap().resource.clone();
        let questions = &mut d.inbox.questions;
        match reason {
            Held::Submitting => {
                let owner = questions.owner(&terminal, ID).unwrap().clone();
                questions.pending.insert(7, owner.clone());
                questions
                    .status
                    .insert(owner, crate::desk::questions::SUBMITTING.into());
            }
            Held::Disconnected => questions.views.get_mut(&terminal).unwrap().1.connected = false,
            Held::Unsupported => {
                questions
                    .views
                    .get_mut(&terminal)
                    .unwrap()
                    .1
                    .capabilities
                    .questions = false.into();
            }
        }
        tx.instant();
    });
    settle(rt);
    let terminal = &rt.state.connection.shown.as_ref().unwrap().resource;
    rt.state.inbox.questions.open(terminal, ID).unwrap();
    assert!(rt.state.inbox.questions.held(owner(&rt.state)));
    assert!(
        rt.state.inbox.questions.error.is_empty(),
        "holding is not an error"
    );
}

/// Text/choices/provider question must survive a disabled touch unchanged.
fn draft_state(rt: &Runtime<Desk>) -> Value {
    let draft = draft(&rt.state);
    let texts: std::collections::BTreeMap<_, _> = draft
        .texts
        .iter()
        .map(|(id, edit)| (id.clone(), edit.text.clone()))
        .collect();
    json!({"question": draft.question, "texts": texts, "choices": draft.choices})
}

fn control_node<'a>(tree: &'a Value, key: &str) -> Option<&'a Value> {
    match tree {
        Value::Object(fields) if fields.get("key").is_some_and(|value| value == key) => Some(tree),
        Value::Object(fields) => fields.values().find_map(|value| control_node(value, key)),
        Value::Array(values) => values.iter().find_map(|value| control_node(value, key)),
        _ => None,
    }
}

fn disabled_control(rt: &Runtime<Desk>, key: &str, context: &str) {
    let element = crate::ui::element(rt, key);
    assert!(
        !element.focusable,
        "{context}: disabled control is not focusable"
    );
    assert!(
        !element.focused,
        "{context}: disabled control did not take focus"
    );
    let tree = rt.accessibility_json();
    let node = control_node(&tree, key).expect("the actual disabled button");
    assert_eq!(node["role"], "button", "{context}: keep the button role");
    assert!(
        node["states"]
            .as_array()
            .unwrap()
            .iter()
            .any(|state| state == "disabled"),
        "{context}: actual accessibility state {node:?}"
    );
}

fn held_touch(view: View, kind: &str, action: &str, reason: Held, keyboard: bool) {
    let (client, mut rt) = ready(view, kind);
    rt.update(|d, _| d.phone.as_mut().unwrap().keyboard = keyboard);
    hold(&mut rt, reason);
    let key = key(&rt, action);
    let context = format!("{} {kind} {action} {reason:?}", view.name);
    show(&mut rt, action, true);
    let before = crate::ui::element(&rt, &key);
    let draft = draft_state(&rt);
    let pending = rt.state.inbox.questions.pending.clone();
    let status = rt.state.inbox.questions.status.clone();
    let error = rt.state.inbox.questions.error.clone();
    touch(&mut rt, &key, TouchPhase::Ended);
    // Check wire/state invariants before the error equality that is known red
    // on the old product. No callback is invoked directly.
    no_input(&client);
    assert_eq!(rt.state.inbox.questions.pending, pending, "{context}");
    assert_eq!(rt.state.inbox.questions.status, status, "{context}");
    assert_eq!(
        draft_state(&rt),
        draft,
        "{context}: draft/provider unchanged"
    );
    assert!(rt.state.inbox.open, "{context}: request remains open");
    assert_eq!(
        rt.state.phone.as_ref().unwrap().keyboard,
        keyboard,
        "{context}"
    );
    assert!(
        !before.focusable && !before.focused,
        "{context}: initial disabled focus state"
    );
    disabled_control(&rt, &key, &context);
    assert_eq!(
        rt.state.inbox.questions.error, error,
        "{context}: a disabled finished touch must not run the action handler"
    );
}

#[test]
fn phone_held_acp_response_actions_ignore_finished_touches() {
    for view in [PHONES[0], PHONES[1]] {
        for (kind, action) in ACTIONS {
            for reason in REASONS {
                for keyboard in [false, true] {
                    held_touch(view, kind, action, reason, keyboard);
                }
            }
        }
    }
}

#[test]
fn phone_held_acp_cancel_ignores_finished_touches_independently() {
    for view in [PHONES[0], PHONES[1]] {
        for (kind, _) in ACTIONS {
            for reason in REASONS {
                for keyboard in [false, true] {
                    held_touch(view, kind, "cancel", reason, keyboard);
                }
            }
        }
    }
}
