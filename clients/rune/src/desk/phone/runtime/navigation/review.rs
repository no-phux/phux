//! Acceptance paths that cross the navigator and its underlying terminal.
use super::*;

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
    rt.key(crate::keys::plain(Key::Text("review".into())));
    settle(&mut rt);
    assert_eq!(rt.state.phone.as_ref().unwrap().query.text, "review");
    assert!(rt.find("phone-session-pane-@2").is_some());
    assert!(rt.find("phone-session-pane-@1").is_none());
    rt.key(crate::keys::plain(Key::Escape));
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
        assert_eq!(
            rt.state.connection.wanted.as_ref().unwrap().0,
            ResourceId::local(2)
        );
        assert!(rt.state.phone_page().is_none());
        no_terminal_input(&client);
    }
}

#[test]
fn connections_keyboard_activation_requests_native_setup_without_terminal_input() {
    for key in [Key::Enter, Key::Text(" ".into())] {
        let (client, mut rt) = fixture();
        rt.update(|d, tx| {
            d.native = Some(crate::mobile::Platform::test());
            d.open_phone_sessions();
            tx.focus("phone-session-connections", true);
        });
        settle(&mut rt);
        let _ = client.take_outbound();
        rt.key(crate::keys::plain(key));
        settle(&mut rt);
        assert!(rt.state.native.as_ref().unwrap().connection_requested);
        // UIKit's sheet comes up over Sessions, which waits under it.
        assert_eq!(rt.state.phone_page(), Some(Page::Sessions));
        assert!(!rt.state.phone.as_ref().unwrap().query.focused);
        no_terminal_input(&client);
    }
}

#[test]
fn native_session_search_retains_its_editor_across_large_text_layout_changes() {
    use crate::desk::native_input::{Action, Bridge, Operation};
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((320.0, 340.0), 2.0);
    rt.update(|d, _| {
        d.native = Some(crate::mobile::Platform::test());
        d.open_phone_sessions();
    });
    settle(&mut rt);
    tap(&mut rt, "phone-session-query", 0.1);
    let _ = client.take_outbound();
    let mut bridge = Bridge::default();
    for (size, text) in [
        ("UICTContentSizeCategoryAccessibilityXXXL", "review"),
        ("UICTContentSizeCategoryL", "build"),
    ] {
        rt.update(|d, _| {
            d.set_accessibility(false, size);
        });
        settle(&mut rt);
        let snap = bridge.snapshot(&rt);
        assert_eq!(snap.kind, "field");
        assert!(
            bridge
                .apply(
                    &mut rt,
                    Operation {
                        token: snap.token,
                        revision: snap.revision,
                        action: Action::Replace {
                            expected: snap.text,
                            text: text.into(),
                            anchor: text.len(),
                            focus: text.len(),
                        },
                    },
                )
                .unwrap()
        );
        rt.settle_all();
        settle(&mut rt);
        assert_eq!(bridge.snapshot(&rt).text, text);
        assert!(rt.find("phone-session-connections").is_none());
        let editor = crate::ui::element(&rt, "phone-session-query");
        assert!(editor.visible[3] >= editor.rect[3] - 0.5, "{editor:?}");
        no_terminal_input(&client);
    }
    rt.update(|d, tx| d.blur_phone_search(tx));
    settle(&mut rt);
    assert_eq!(rt.state.phone.as_ref().unwrap().query.text, "build");
    assert!(rt.find("phone-session-connections").is_some());
    no_terminal_input(&client);
}
