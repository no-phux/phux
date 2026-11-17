//! An ACP ask drawn in the inbox: one choice as kit radios, several as
//! checkboxes, the field in its control outline, Submit the primary.

use phux_client_runtime::control::Event;
use phux_protocol::native_agent::{
    NativeCapabilities, NativeOption, NativeQuestion, NativeQuestionItem, NativeView,
};

fn option(id: &str) -> NativeOption {
    NativeOption {
        id: id.into(),
        label: id.to_uppercase(),
        description: None,
    }
}

fn item(id: &str, multi: bool) -> NativeQuestionItem {
    NativeQuestionItem {
        id: id.into(),
        question: format!("Question {id}"),
        header: String::new(),
        options: vec![option("one"), option("two")],
        multi,
        recommended: None,
    }
}

#[test]
fn an_ask_draws_radios_checkboxes_and_a_primary_submit() {
    let (_, mut rt) = crate::test_support::input_fixture();
    rt.frame(0.0);
    let terminal = rt.state.connection.shown.as_ref().unwrap().resource.clone();
    rt.state.connection.topology = rt
        .state
        .connection
        .client
        .as_ref()
        .and_then(|c| c.topology());
    let question = NativeQuestion {
        id: "acp-question:pick".into(),
        kind: "ask".into(),
        title: "Pick".into(),
        message: String::new(),
        options: vec![],
        questions: vec![item("single", false), item("multi", true)],
        fields: serde_json::Value::Null,
    };
    rt.state.inbox.questions.views.insert(
        terminal.clone(),
        (
            phux_protocol::ids::ResourceId::local(2),
            NativeView {
                activation: "active".into(),
                backend: "acp".into(),
                connected: true,
                capabilities: NativeCapabilities {
                    questions: true.into(),
                    ..Default::default()
                },
                turn_id: None,
                provider_session_id: None,
                questions: vec![question.clone()],
            },
        ),
    );
    rt.state.inbox.questions.drafts.insert(
        question.id.clone(),
        crate::desk::questions::Draft::new(&question),
    );
    rt.state.activities.apply(
        &Event::AgentAskedState {
            terminal_id: terminal,
            asked: true,
        },
        &[],
    );
    rt.update(|d, _| d.toggle_inbox());
    rt.frame(0.0);
    let single = crate::ui::element(&rt, "acp-acp-question:pick-single");
    assert_eq!(single.role, "radiogroup", "one choice is a radio group");
    let radios = (rt.elements().into_iter())
        .filter(|e| e.role == "radio")
        .count();
    assert_eq!(radios, 2);
    for option in ["one", "two"] {
        let check = crate::ui::element(&rt, &format!("acp-acp-question:pick-multi-{option}"));
        assert_eq!(check.role, "checkbox", "several choices are checkboxes");
    }
    let submit = crate::ui::element(&rt, "acp-acp-question:pick-submit");
    assert_eq!(submit.role, "button");
    assert_eq!(submit.label.as_deref(), Some("Submit response"));

    // Choosing a radio records the choice in the draft, once the drawer
    // has slid in.
    for t in [0.5, 1.0, 2.0] {
        rt.frame(t);
    }
    let radio = (rt.elements().into_iter())
        .find(|e| e.role == "radio")
        .expect("the first radio");
    let [rx, ry, rw, rh] = radio.rect;
    let (x, y) = (rx + rw / 2.0, ry + rh / 2.0);
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(0.0);
    let draft = &rt.state.inbox.questions.drafts["acp-question:pick"];
    assert_eq!(draft.choices.get("single").map(Vec::len), Some(1));
}
