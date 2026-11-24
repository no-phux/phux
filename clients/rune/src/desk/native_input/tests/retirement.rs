//! Real mounted/unmounted ACP focus, rather than only mutating a draft flag.
use super::*;
use crate::desk::phone::Soft;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::FrameKind;

#[test]
fn retained_field_text_does_not_reauthorize_a_native_edit_from_the_old_runtime() {
    let (client, mut rt, mut bridge) = field("palette-query");
    assert!(replace(&mut bridge, &mut rt, "keep this draft", 15, 15).unwrap());
    let old = bridge.snapshot(&rt);
    rt.update(|desk, _| {
        desk.replace_connection(crate::desk::link::Replacement::Retry);
        desk.connection.client = Some(client.clone());
    });
    rt.frame(0.0);
    let stale = Action::Text {
        text: "stale".into(),
    };
    let accepted = submit(&mut bridge, &mut rt, &old, stale).unwrap();
    assert!(!accepted);
    assert_eq!(rt.state.palette_query.text, "keep this draft");
}

#[test]
fn unmounted_acp_focus_does_not_capture_later_terminal_accessory_input() {
    let (client, mut rt) = crate::test_support::input_fixture();
    native(&mut rt.state);
    rt.state.connection.topology = client.with_control(|control| control.topology().cloned());
    let id = "acp-question:retained";
    let question = acp_question(id, "Editor", "Draft");
    let terminal = rt.state.connection.shown.as_ref().unwrap().resource.clone();
    let view = acp_view("active", &question);
    rt.state
        .inbox
        .questions
        .views
        .insert(terminal.clone(), (ResourceId::local(55), view));
    rt.state.inbox.questions.reconcile();
    let owner = rt
        .state
        .inbox
        .questions
        .owner(&terminal, id)
        .unwrap()
        .clone();
    rt.state
        .inbox
        .questions
        .text_mut(&owner, "text")
        .replace_text("preserved");
    rt.set_viewport((393.0, 852.0), 2.0);
    // Settle stage/camera/pane measurement with the actual motion clock.
    for _ in 0..120 {
        rt.update(|_, tx| tx.instant());
        rt.frame(1.0 / 60.0);
    }
    let key = owner.field_key("text");
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
    let edit = &rt.state.inbox.questions.draft(&owner).unwrap().texts["text"];
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
        rt.state.inbox.questions.draft(&owner).unwrap().text("text"),
        "preserved"
    );
}
