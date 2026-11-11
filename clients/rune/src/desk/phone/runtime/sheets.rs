//! Real touches through sheet scrims, including the smallest phone preview.
use super::*;
use phux_client_runtime::control::Event;

fn phone(size: (f32, f32)) -> (std::sync::Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = fixture();
    rt.state.topology = client.with_control(|control| control.topology().cloned());
    rt.set_viewport(size, 2.0);
    (client, rt)
}

fn visible_editor(rt: &Runtime<Desk>, key: &str) {
    let editor = rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .unwrap();
    let keyboard = rt.presented("phone-keyboard").unwrap().0;
    assert!(!editor.inert, "{key} is behind its own scrim");
    assert!(
        editor.rect[2] > 0.0 && editor.rect[3] > 0.0,
        "{key}: {:?}",
        editor.rect
    );
    assert!(
        editor.rect[1] >= 0.0 && editor.rect[1] + editor.rect[3] <= keyboard[1],
        "{key} behind keyboard: {:?}, {keyboard:?}",
        editor.rect
    );
    let cap = rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some("phone-cap-120"))
        .unwrap();
    assert!(!cap.inert, "the sheet must not block the keyboard");
}

fn ask(rt: &mut Runtime<Desk>, id: &str) {
    let terminal_id = rt.state.shown.as_ref().unwrap().resource.clone();
    rt.state.activities.apply(
        &Event::AgentAsked {
            terminal_id,
            question_id: id.into(),
            text: "Continue?".into(),
            suggestions: vec![],
            waiting_seconds: None,
        },
        &[],
    );
}

#[test]
fn rename_accepts_drawn_text_submit_and_cancel_without_terminal_input() {
    for size in [(393.0, 852.0), (320.0, 568.0)] {
        let (client, mut rt) = phone(size);
        rt.state
            .rename_session(phux_protocol::ids::SessionKey::new([1; 16]).unwrap(), "old");
        rt.frame(0.0);
        visible_editor(&rt, "rename-field");
        // The keyboard's own shift: at 320 points the strip's ⇧ is scrolled away.
        tap(&mut rt, "phone-cap-shift", 0.1);
        tap(&mut rt, "phone-cap-120", 0.3);
        assert_eq!(rt.state.tabs.rename_field.text, "X");
        tap(&mut rt, "phone-cap-backspace", 0.5);
        assert!(rt.state.tabs.rename_field.text.is_empty());
        tap(&mut rt, "phone-cap-120", 0.7);
        tap(&mut rt, "phone-cap-return", 0.9);
        assert!(rt.state.tabs.renaming.is_none());
        rt.update(|d, tx| {
            d.rename_session(phux_protocol::ids::SessionKey::new([1; 16]).unwrap(), "old");
            tx.instant();
        });
        settle(&mut rt);
        tap(&mut rt, "phone-key-Escape", 1.1);
        assert!(rt.state.tabs.renaming.is_none());
        no_terminal_input(&client);
    }
}

#[test]
fn machine_setup_accepts_drawn_text_and_reports_submission_errors() {
    for size in [(393.0, 852.0), (320.0, 568.0)] {
        let (client, mut rt) = phone(size);
        rt.state.machines.phux = None; // Never spawn an installed CLI in a unit test.
        rt.state.toggle_machines();
        rt.frame(0.0);
        visible_editor(&rt, "machine-field");
        tap(&mut rt, "phone-cap-return", 0.1);
        assert!(
            rt.state
                .machines
                .error
                .as_deref()
                .unwrap()
                .contains("ssh destination")
        );
        tap(&mut rt, "phone-cap-120", 0.3);
        assert_eq!(rt.state.machines.field.text, "x");
        tap(&mut rt, "phone-cap-return", 0.5);
        assert!(
            rt.state
                .machines
                .error
                .as_deref()
                .unwrap()
                .contains("Install the phux CLI")
        );
        tap(&mut rt, "phone-key-Escape", 0.7);
        assert!(!rt.state.machines.open);
        no_terminal_input(&client);
    }
}

#[test]
fn inbox_reply_types_and_stale_or_missing_editors_fail_closed() {
    for size in [(393.0, 852.0), (320.0, 568.0)] {
        let (client, mut rt) = phone(size);
        ask(&mut rt, "first");
        rt.state.toggle_inbox();
        rt.frame(0.0);
        visible_editor(&rt, "inbox-reply");
        tap(&mut rt, "phone-cap-120", 0.1);
        assert_eq!(rt.state.inbox.reply.text, "x");
        ask(&mut rt, "second"); // A held edit cannot slide onto a different question.
        rt.update(|d, tx| d.soft(Soft::Char('y'), tx));
        assert_eq!(rt.state.inbox.reply.text, "x");
        assert!(rt.state.inbox.answers.is_empty());
        rt.state.activities = Default::default();
        settle(&mut rt);
        tap(&mut rt, "phone-cap-120", 0.3);
        assert_eq!(rt.state.inbox.reply.text, "x");
        tap(&mut rt, "phone-key-Escape", 0.5);
        assert!(!rt.state.inbox.open);
        no_terminal_input(&client);
    }
}

#[test]
fn an_inbox_return_submits_only_the_picked_reply() {
    let (client, mut rt) = phone((393.0, 852.0));
    ask(&mut rt, "first");
    rt.state.toggle_inbox();
    rt.frame(0.0);
    tap(&mut rt, "phone-cap-120", 0.1);
    tap(&mut rt, "phone-cap-return", 0.3);
    assert_eq!(
        rt.state.inbox.answers.len(),
        1,
        "used the existing acknowledged-answer gate"
    );
    // That gate refuses this test's UI-only ask: no live runtime ask exists.
    no_terminal_input(&client);
}

#[test]
fn retained_acp_focus_cannot_capture_the_drawn_inbox_keyboard() {
    use phux_protocol::native_agent::{NativeCapabilities, NativeQuestion, NativeView};
    let (client, mut rt) = phone((393.0, 852.0));
    let terminal = rt.state.shown.as_ref().unwrap().resource.clone();
    let question = |id: &str| NativeQuestion {
        id: format!("acp-question:{id}"),
        kind: "input".into(),
        title: "Input".into(),
        message: String::new(),
        options: vec![],
        questions: vec![],
        fields: serde_json::Value::Null,
    };
    let (current, hidden) = (question("current"), question("hidden"));
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
                questions: vec![current.clone(), hidden.clone()],
            },
        ),
    );
    for q in [current, hidden] {
        rt.state
            .inbox
            .questions
            .drafts
            .insert(q.id.clone(), crate::desk::questions::Draft::new(&q));
    }
    rt.state.activities.apply(
        &Event::AgentAskedState {
            terminal_id: terminal,
            asked: true,
        },
        &[],
    );
    rt.state.toggle_inbox();
    let hidden = rt
        .state
        .inbox
        .questions
        .drafts
        .get_mut("acp-question:hidden")
        .unwrap()
        .texts
        .get_mut("text")
        .unwrap();
    hidden.focused = true;
    hidden.replace_text("preserved");
    rt.frame(0.0);
    assert!(rt.find("acp-acp-question:current-field-text").is_some());
    assert!(rt.find("acp-acp-question:hidden-field-text").is_none());
    tap(&mut rt, "phone-cap-120", 0.1);
    tap(&mut rt, "phone-cap-return", 0.3);
    assert_eq!(
        rt.state.inbox.questions.drafts["acp-question:hidden"].text("text"),
        "preserved"
    );
    assert!(rt.state.inbox.questions.pending.is_empty());
    no_terminal_input(&client);
}
