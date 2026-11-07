use phux_protocol::native_agent::{NativeCapabilities, NativeQuestionItem};
use serde_json::json;

use super::*;
use crate::launch::Resolved;

fn question(kind: &str, fields: Value) -> NativeQuestion {
    NativeQuestion {
        id: "acp-question:1".into(),
        kind: kind.into(),
        title: "Title".into(),
        message: "Message".into(),
        options: vec![],
        questions: vec![],
        fields,
    }
}

fn option(id: &str) -> NativeOption {
    NativeOption {
        id: id.into(),
        label: id.to_uppercase(),
        description: None,
    }
}

fn item(id: &str, options: &[&str], multi: bool) -> NativeQuestionItem {
    NativeQuestionItem {
        id: id.into(),
        question: format!("Question {id}"),
        header: String::new(),
        options: options.iter().map(|o| option(o)).collect(),
        multi,
        recommended: options.first().map(|o| (*o).to_owned()),
    }
}

fn view(questions: Vec<NativeQuestion>, answers: bool) -> NativeView {
    NativeView {
        activation: "activation-A".into(),
        backend: "acp".into(),
        capabilities: NativeCapabilities {
            questions: answers.into(),
            ..NativeCapabilities::default()
        },
        connected: true,
        turn_id: None,
        provider_session_id: None,
        questions,
    }
}

fn typed(draft: &mut Draft, id: &str, text: &str) {
    draft.texts.insert(id.into(), TextEdit::new(text));
}

#[test]
fn input_and_editor_send_the_text_starting_from_the_initial_value() {
    let mut draft = Draft::new(&question("input", json!({"initial_value": "main"})));
    assert_eq!(response::build(&draft).unwrap(), json!({"text": "main"}));
    typed(&mut draft, "text", "release");
    assert_eq!(response::build(&draft).unwrap(), json!({"text": "release"}));
    let editor = Draft::new(&question("editor", Value::Null));
    assert_eq!(response::build(&editor).unwrap(), json!({"text": ""}));
}

#[test]
fn ask_answers_every_item_with_offered_options_and_never_the_recommendation() {
    let mut ask = question("ask", Value::Null);
    ask.questions = vec![item("a", &["one", "two"], true), item("b", &["x"], false)];
    let mut draft = Draft::new(&ask);
    let unanswered = response::build(&draft).unwrap_err();
    assert_eq!(unanswered, "Answer ‘Question a’ before submitting.");
    draft
        .choices
        .insert("a".into(), vec!["one".into(), "two".into()]);
    typed(&mut draft, "b", "my own");
    assert_eq!(
        response::build(&draft).unwrap(),
        json!({"answers": [
            {"id": "a", "selected_options": ["one", "two"]},
            {"id": "b", "selected_options": [], "custom_input": "my own"},
        ]})
    );
    draft.choices.insert("a".into(), vec!["invented".into()]);
    assert!(response::build(&draft).is_err());
    draft.choices.insert("a".into(), vec!["one".into()]);
    draft
        .choices
        .insert("b".into(), vec!["x".into(), "x".into()]);
    assert!(response::build(&draft).is_err(), "single choice took two");
}

#[test]
fn ask_without_custom_input_has_no_text_fields() {
    let mut ask = question("ask", json!({"allow_custom_input": false}));
    ask.questions = vec![item("a", &["one"], false)];
    assert!(Draft::new(&ask).texts.is_empty());
}

#[test]
fn forms_keep_types_offer_fixed_sets_and_refuse_coercion() {
    let fields = json!({
        "properties": {
            "count": {"type": "integer"},
            "mode": {"enum": ["fast", "safe"]},
            "tags": {"type": "array", "items": {"oneOf": [{"const": "a"}, {"const": 2}]}},
            "note": {"type": "string"},
        },
        "required": ["count", "mode"],
    });
    let mut draft = Draft::new(&question("form", fields));
    assert_eq!(draft.options.keys().collect::<Vec<_>>(), ["mode", "tags"]);
    assert!(draft.texts.contains_key("count") && !draft.texts.contains_key("mode"));
    typed(&mut draft, "count", "12.5");
    assert!(response::build(&draft).unwrap_err().starts_with("count:"));
    typed(&mut draft, "count", "12");
    let unchosen = response::build(&draft).unwrap_err();
    assert_eq!(unchosen, "Choose an explicit option for ‘mode’.");
    draft.choices.insert("mode".into(), vec!["\"safe\"".into()]);
    draft
        .choices
        .insert("tags".into(), vec!["\"a\"".into(), "2".into()]);
    assert_eq!(
        response::build(&draft).unwrap(),
        json!({"values": {"count": 12, "mode": "safe", "tags": ["a", 2]}})
    );
    assert_eq!(
        response::form_value(&json!({"type": "string"}), "false", None).unwrap(),
        json!("false")
    );
    assert!(response::form_value(&json!({"type": "boolean"}), "yes", None).is_err());
    assert!(response::form_value(&json!({"type": "null"}), "null", None).is_err());
}

#[test]
fn unsupported_kinds_and_schemas_submit_nothing() {
    for kind in ["confirm", "permission", "select", "mystery"] {
        assert!(response::build(&Draft::new(&question(kind, Value::Null))).is_err());
    }
    let schemaless = response::build(&Draft::new(&question("form", json!({}))));
    assert!(schemaless.unwrap_err().contains("Nothing was submitted"));
}

#[test]
fn the_gate_sends_only_to_a_connected_answering_view_with_the_same_question() {
    let asked = question("confirm", Value::Null);
    let open = view(vec![asked.clone()], true);
    assert_eq!(admit(true, true, Some(&open), &asked), Ok("activation-A"));
    let unsupported = view(vec![asked.clone()], false);
    assert!(admit(true, true, Some(&unsupported), &asked).is_err());
    let closed = view(vec![], true);
    assert_eq!(admit(true, true, Some(&closed), &asked), Err(CLOSED));
    let changed = question("confirm", json!({"other": true}));
    assert_eq!(admit(true, true, Some(&open), &changed), Err(CLOSED));
    let mut disconnected = open.clone();
    disconnected.connected = false;
    assert!(admit(true, true, Some(&disconnected), &asked).is_err());
    assert!(admit(true, true, None, &asked).is_err());
    assert!(admit(false, true, Some(&open), &asked).is_err());
    assert!(admit(true, false, Some(&open), &asked).is_err());
}

fn asking_desk(answers: bool) -> (Desk, ResourceId) {
    let mut desk = Desk::new(&Resolved::offline("not launched"));
    desk.client = Some(std::sync::Arc::new(phux_client_runtime::Runtime::embedded(
        phux_client_runtime::control::ControlOptions::default(),
    )));
    let (terminal, session) = (ResourceId::local(1), ResourceId::local(2));
    let asked = question("confirm", Value::Null);
    let questions = &mut desk.inbox.questions;
    questions
        .drafts
        .insert(asked.id.clone(), Draft::new(&asked));
    let published = view(vec![asked], answers);
    questions
        .views
        .insert(terminal.clone(), (session, published));
    (desk, terminal)
}

#[test]
fn a_closed_or_unsupported_question_sends_nothing_and_says_why() {
    let id = "acp-question:1";
    let (mut desk, terminal) = asking_desk(false);
    desk.answer_question(&terminal, id, Some(json!({"confirmed": true})));
    let questions = &desk.inbox.questions;
    assert!(questions.pending.is_empty() && !questions.status.contains_key(id));
    assert!(questions.error.contains_key(id));
    let (mut desk, terminal) = asking_desk(true);
    desk.inbox.questions.drafts.clear();
    desk.answer_question(&terminal, id, Some(json!({"confirmed": true})));
    assert!(desk.inbox.questions.pending.is_empty());
    assert_eq!(
        desk.inbox.questions.error.get(id).map(String::as_str),
        Some(CLOSED)
    );
}

#[test]
fn a_response_on_its_way_holds_the_controls_until_its_receipt() {
    let id = "acp-question:1";
    let (mut desk, terminal) = asking_desk(true);
    let questions = &mut desk.inbox.questions;
    questions.pending.insert(7, id.into());
    questions.status.insert(id.into(), SUBMITTING.into());
    desk.answer_question(&terminal, id, Some(json!({"confirmed": false})));
    assert_eq!(
        desk.inbox.questions.pending.len(),
        1,
        "a second answer went out"
    );
    let questions = &mut desk.inbox.questions;
    assert!(questions.receipt(7, NativeDisposition::Queued, None));
    assert!(questions.pending.contains_key(&7) && questions.status.contains_key(id));
    assert!(
        !questions.receipt(8, NativeDisposition::Completed, None),
        "not ours"
    );
    assert!(questions.receipt(7, NativeDisposition::Refused, Some("stale")));
    assert!(questions.pending.is_empty() && !questions.status.contains_key(id));
    assert!(questions.drafts.contains_key(id), "the draft stays");
    assert_eq!(
        questions.error.get(id).map(String::as_str),
        Some("Refused. Nothing retried; draft preserved: stale")
    );
}

#[test]
fn a_lost_connection_marks_waiting_answers_unknown() {
    let id = "acp-question:1";
    let (mut desk, _) = asking_desk(true);
    let questions = &mut desk.inbox.questions;
    questions.pending.insert(3, id.into());
    questions.status.insert(id.into(), SUBMITTING.into());
    assert!(questions.disconnected());
    assert!(questions.status[id].starts_with("Outcome unknown"));
}

#[test]
fn choosing_replaces_a_single_choice_toggles_many_and_holds_while_sending() {
    let id = "acp-question:1";
    let (mut desk, _) = asking_desk(true);
    let questions = &mut desk.inbox.questions;
    questions.choose(id, "a", "one", false);
    questions.choose(id, "a", "two", false);
    assert_eq!(questions.drafts[id].choices["a"], ["two"]);
    questions.choose(id, "b", "one", true);
    questions.choose(id, "b", "two", true);
    questions.choose(id, "b", "one", true);
    assert_eq!(questions.drafts[id].choices["b"], ["two"]);
    questions.status.insert(id.into(), SUBMITTING.into());
    questions.choose(id, "b", "one", true);
    assert_eq!(questions.drafts[id].choices["b"], ["two"]);
}
