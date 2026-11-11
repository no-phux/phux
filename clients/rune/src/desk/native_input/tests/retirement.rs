//! Real mounted/unmounted ACP focus, rather than only mutating a draft flag.
use super::*;
use crate::desk::{phone::Soft, questions::Draft};
use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::{NativeCapabilities, NativeQuestion, NativeView};
use phux_protocol::wire::frame::FrameKind;

#[test]
fn unmounted_acp_focus_does_not_capture_later_terminal_accessory_input() {
    let (client, mut rt) = crate::grid::budget::input_fixture();
    native(&mut rt.state);
    rt.state.prefs = rune::prefs::Prefs::memory();
    rt.state.topology = client.with_control(|control| control.topology().cloned());
    let id = "acp-question:retained";
    let question = NativeQuestion {
        id: id.into(),
        kind: "editor".into(),
        title: "Editor".into(),
        message: "Draft".into(),
        options: vec![],
        questions: vec![],
        fields: serde_json::Value::Null,
    };
    let terminal = rt.state.shown.as_ref().unwrap().resource.clone();
    rt.state.inbox.questions.views.insert(
        terminal,
        (
            ResourceId::local(55),
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
    let mut draft = Draft::new(&question);
    draft
        .texts
        .get_mut("text")
        .unwrap()
        .replace_text("preserved");
    rt.state.inbox.questions.drafts.insert(id.into(), draft);
    rt.set_viewport((393.0, 852.0), 2.0);
    // Settle stage/camera/pane measurement with the actual motion clock.
    for _ in 0..120 {
        rt.update(|_, tx| tx.instant());
        rt.frame(1.0 / 60.0);
    }
    let key = format!("acp-{id}-field-text");
    assert!(
        rt.find(&key).is_some(),
        "fixture must mount the real ACP editor"
    );
    assert!(rt.accessibility_action(&key, Accessibility::Focus, None));
    rt.frame(0.0);
    let mut bridge = Bridge::default();
    bridge.reconcile(&mut rt);
    assert_eq!(bridge.snapshot(&rt).kind, "field");
    rt.update(|d, tx| {
        d.open_find();
        tx.instant();
    });
    rt.frame(0.0);
    assert!(
        !rt.elements()
            .iter()
            .any(|e| e.key.as_deref() == Some(key.as_str()) && !e.exiting),
        "Find must retire the question field"
    );
    bridge.reconcile(&mut rt);
    let edit = &rt.state.inbox.questions.drafts[id].texts["text"];
    assert!(!edit.focused);
    assert_eq!(edit.text, "preserved");
    rt.update(|d, tx| {
        d.close_find();
        tx.blur();
    });
    rt.frame(0.0);
    bridge.reconcile(&mut rt);
    assert!(!crate::desk::native_input::accessory::editing(&rt.state));
    let _ = client.take_outbound();
    rt.update(|d, tx| d.soft(Soft::Key(Key::Left), tx));
    assert!(client.take_outbound().iter().any(|bytes| matches!(
        FrameKind::decode(bytes).unwrap().0,
        FrameKind::InputKey { .. }
    )));
    assert_eq!(
        rt.state.inbox.questions.drafts[id].text("text"),
        "preserved"
    );
}
