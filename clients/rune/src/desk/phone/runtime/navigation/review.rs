//! Acceptance paths that cross the navigator and its underlying terminal.
use super::*;

#[test]
fn session_confirmation_keeps_its_key_when_selection_and_names_change() {
    let (client, mut rt) = fixture();
    let key = phux_protocol::ids::SessionKey::new([1; 16]).unwrap();
    inventory(&mut rt);
    rt.update(|d, _| {
        d.topology.as_mut().unwrap().sessions[0].key = Some(key);
        d.close_session();
    });
    settle(&mut rt);
    let question = Some(crate::desk::Pending::CloseSession(key, "budget".into()));
    assert_eq!(rt.state.pending, question);
    rt.update(|d, _| {
        d.topology.as_mut().unwrap().sessions[0].name = "renamed".into();
        client.attach_session(phux_protocol::wire::frame::AttachTarget::ByName(
            "empty".into(),
        ));
    });
    settle(&mut rt);
    assert_eq!(client.selected_session(), Some(2));
    assert_eq!(rt.state.pending, question);
    tap(&mut rt, "confirm-cancel", 0.1);
    assert!(rt.state.pending.is_none());
}

#[test]
fn closing_from_actions_confirms_the_exact_pane_and_cancel_sends_no_close() {
    let (client, mut rt) = fixture();
    inventory(&mut rt);
    rt.update(|d, _| {
        d.settings.confirm_close = true;
        d.open_phone_actions();
    });
    settle(&mut rt);
    let _ = client.take_outbound();
    assert!(rt.accessibility_action(
        "phone-action-CloseTerminal",
        rune::accesskit::Action::Click,
        None,
    ));
    settle(&mut rt);
    assert!(rt.state.phone_page().is_none());
    assert_eq!(
        rt.state.pending,
        Some(crate::desk::Pending::Close(ResourceId::local(1)))
    );
    for key in ["confirm-cancel", "confirm-ok"] {
        let rect = rt.presented(key).unwrap().0;
        assert!(rect[2] >= 44.0 && rect[3] >= 44.0, "{key}: {rect:?}");
    }
    tap(&mut rt, "confirm-cancel", 0.1);
    assert!(rt.state.pending.is_none());
    for bytes in client.take_outbound() {
        let frame = FrameKind::decode(&bytes).unwrap().0;
        assert!(
            !matches!(
                frame,
                FrameKind::Command {
                    command: phux_protocol::wire::frame::Command::KillResource { .. }
                        | phux_protocol::wire::frame::Command::KillResources { .. },
                    ..
                } | FrameKind::InputKey { .. }
                    | FrameKind::InputPaste { .. }
            ),
            "cancel must neither close nor type: {frame:?}"
        );
    }
}

#[test]
fn hardware_search_and_escape_release_focus_without_typing_behind_navigation() {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_system_reduced_motion(true);
    rt.update(|d, _| d.open_phone_sessions());
    settle(&mut rt);
    tap(&mut rt, "phone-session-query", 0.1);
    let _ = client.take_outbound();
    rt.key(rune::KeyEvent {
        key: Key::Text("review".into()),
        mods: rune::Mods::default(),
    });
    settle(&mut rt);
    assert_eq!(rt.state.phone.as_ref().unwrap().query.text, "review");
    assert!(rt.find("phone-session-pane-@2").is_some());
    assert!(rt.find("phone-session-pane-@1").is_none());
    rt.key(rune::KeyEvent {
        key: Key::Escape,
        mods: rune::Mods::default(),
    });
    settle(&mut rt);
    assert!(rt.state.phone_page().is_none());
    assert!(!rt.state.phone.as_ref().unwrap().query.focused);
    assert!(!rt.has_focus());
    no_terminal_input(&client);
}

#[test]
fn desktop_pointer_and_touch_select_the_same_resource_without_terminal_input() {
    for pointer in [false, true] {
        let (client, mut rt) = fixture();
        detailed_inventory(&mut rt);
        rt.update(|d, _| d.open_phone_sessions());
        settle(&mut rt);
        let _ = client.take_outbound();
        let key = "phone-session-pane-@2";
        if pointer {
            let (x, y) = rt.find(key).unwrap();
            rt.pointer_down(x, y);
            rt.pointer_up(x, y);
            settle(&mut rt);
        } else {
            tap(&mut rt, key, 0.1);
        }
        assert_eq!(rt.state.wanted.as_ref().unwrap().0, ResourceId::local(2));
        assert!(rt.state.phone_page().is_none());
        no_terminal_input(&client);
    }
}
