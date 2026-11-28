//! Clearing is a field edit, not a navigation or terminal-keyboard action.
use super::*;
use crate::ui::element;

fn search(
    size: (f32, f32),
    native: bool,
    category: &str,
    keyboard: bool,
) -> (std::sync::Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport(size, 2.0);
    rt.set_system_reduced_motion(true);
    rt.update(|d, _| {
        if native {
            d.native = Some(crate::mobile::Platform::test());
        }
        d.set_accessibility(false, category);
        d.phone.as_mut().unwrap().keyboard = keyboard;
        d.open_phone_sessions();
    });
    settle(&mut rt);
    assert!(rt.find("phone-session-clear").is_none());
    tap(&mut rt, "phone-session-query", 0.1);
    rt.key(crate::keys::plain(Key::Text("nothing matches".into())));
    rt.settle_all();
    settle(&mut rt);
    assert!(rt.find("phone-session-empty").is_some());
    (client, rt)
}

#[test]
fn session_search_clear_fits_and_keeps_editing_without_changing_the_terminal() {
    for (size, native) in [
        ((320.0, 568.0), false),
        ((393.0, 852.0), false),
        ((320.0, 340.0), true),
        ((393.0, 480.0), true),
    ] {
        for category in [
            "UICTContentSizeCategoryL",
            "UICTContentSizeCategoryAccessibilityXXXL",
        ] {
            for keyboard in [false, true] {
                let (client, mut rt) = search(size, native, category, keyboard);
                let shown = rt.state.connection.shown.as_ref().unwrap().resource.clone();
                let session = client.selected_session();
                let clear = element(&rt, "phone-session-clear");
                let field = element(&rt, "phone-session-query");
                assert_eq!(clear.label.as_deref(), Some("Clear search"));
                assert!(clear.rect[2] >= 44.0 && clear.rect[3] >= 44.0, "{clear:?}");
                assert!(clear.visible[2] >= clear.rect[2] - 0.5, "{clear:?}");
                assert!(clear.visible[3] >= clear.rect[3] - 0.5, "{clear:?}");
                assert!(field.rect[2] >= 44.0, "{field:?}");
                assert!(field.rect[0] + field.rect[2] <= clear.rect[0], "{field:?}");
                tap(&mut rt, "phone-session-clear", 0.3);
                let phone = rt.state.phone.as_ref().unwrap();
                assert!(phone.query.text.is_empty() && phone.query.focused);
                assert_eq!(phone.keyboard, keyboard);
                assert!(rt.state.phone_keyboard_visible());
                assert!(element(&rt, "phone-session-query").focused);
                assert!(rt.find("phone-session-clear").is_none());
                assert!(rt.find("phone-session-empty").is_none());
                assert!(rt.find("phone-session-1").is_some());
                assert_eq!(rt.state.phone_page(), Some(Page::Sessions));
                assert_eq!(rt.state.connection.shown.as_ref().unwrap().resource, shown);
                assert_eq!(client.selected_session(), session);
                rt.update(|d, tx| d.soft(Soft::Char('x'), tx));
                settle(&mut rt);
                assert_eq!(rt.state.phone.as_ref().unwrap().query.text, "x");
                no_terminal_input(&client);
            }
        }
    }
}

#[test]
fn session_search_clear_returns_keyboard_and_accessibility_activation_to_the_editor() {
    for key in [None, Some(Key::Enter), Some(Key::Text(" ".into()))] {
        let (client, mut rt) = search((393.0, 480.0), true, "UICTContentSizeCategoryL", false);
        if let Some(key) = key {
            rt.key(crate::keys::plain(Key::Tab));
            settle(&mut rt);
            assert!(element(&rt, "phone-session-clear").focused);
            rt.key(crate::keys::plain(key));
        } else {
            assert!(rt.accessibility_action(
                "phone-session-clear",
                rune::accesskit::Action::Click,
                None
            ));
        }
        settle(&mut rt);
        assert!(element(&rt, "phone-session-query").focused);
        rt.key(crate::keys::plain(Key::Text("review".into())));
        settle(&mut rt);
        assert_eq!(rt.state.phone.as_ref().unwrap().query.text, "review");
        assert!(rt.find("phone-session-pane-@2").is_some());
        no_terminal_input(&client);
    }
}

#[test]
fn session_search_clear_reopens_a_blurred_filter_and_resets_the_old_scroll() {
    let (client, mut rt) = search((320.0, 340.0), true, "UICTContentSizeCategoryL", false);
    rt.key(rune::KeyEvent {
        key: Key::Command('a'),
        mods: rune::Mods {
            cmd: true,
            ..rune::Mods::default()
        },
    });
    rt.key(crate::keys::plain(Key::Text("phux".into())));
    rt.key(crate::keys::plain(Key::Enter));
    settle(&mut rt);
    assert!(!rt.state.phone_keyboard_visible());
    rt.update(|_, tx| tx.scroll_to_instant("phone-session-list", 0.0, 150.0));
    rt.settle_all();
    settle(&mut rt);
    assert!(
        element(&rt, "phone-session-1").rect[1] < element(&rt, "phone-session-list").rect[1] - 1.0,
        "prove a genuinely nonzero old offset before clearing"
    );
    tap(&mut rt, "phone-session-clear", 0.3);
    rt.settle_all();
    settle(&mut rt);
    assert!(element(&rt, "phone-session-query").focused);
    assert!(rt.state.phone_keyboard_visible());
    assert!(!rt.state.phone.as_ref().unwrap().keyboard);
    let list = element(&rt, "phone-session-list");
    // The unfiltered list leads with Needs you again, at the top.
    let first = element(&rt, "phone-needs-you");
    let space = phux_design::tokens::SPACE;
    assert!(first.rect[1] >= list.rect[1]);
    assert!(first.rect[1] - list.rect[1] < space.s2 + 1.0);
    no_terminal_input(&client);
}

#[test]
fn session_search_clear_cancels_native_composition_and_refuses_an_old_edit() {
    use crate::desk::native_input::{Action, Bridge, Operation};
    let (client, mut rt) = search((320.0, 340.0), true, "UICTContentSizeCategoryL", false);
    let mut bridge = Bridge::default();
    let before = bridge.snapshot(&rt);
    assert_eq!(before.kind, "field");
    assert!(
        bridge
            .apply(
                &mut rt,
                Operation {
                    token: before.token,
                    revision: before.revision,
                    action: Action::Preedit {
                        text: "é".into(),
                        anchor: 1,
                        focus: 1
                    },
                }
            )
            .unwrap()
    );
    assert!(rt.state.phone.as_ref().unwrap().query.is_composing());
    let stale = bridge.snapshot(&rt);
    tap(&mut rt, "phone-session-clear", 0.3);
    assert!(!rt.state.phone.as_ref().unwrap().query.is_composing());
    let cleared = bridge.snapshot(&rt);
    assert_eq!(cleared.kind, "field");
    assert_eq!(cleared.token, stale.token);
    assert!(cleared.wanted && cleared.text.is_empty());
    assert!(
        !bridge
            .apply(
                &mut rt,
                Operation {
                    token: stale.token,
                    revision: stale.revision,
                    action: Action::Replace {
                        expected: stale.text,
                        text: "stale".into(),
                        anchor: 5,
                        focus: 5,
                    },
                }
            )
            .unwrap()
    );
    assert!(
        bridge
            .apply(
                &mut rt,
                Operation {
                    token: cleared.token,
                    revision: cleared.revision,
                    action: Action::Replace {
                        expected: cleared.text.clone(),
                        text: "review".into(),
                        anchor: 6,
                        focus: 6,
                    },
                }
            )
            .unwrap()
    );
    assert_eq!(rt.state.phone.as_ref().unwrap().query.text, "review");
    assert!(!rt.state.phone.as_ref().unwrap().keyboard);
    no_terminal_input(&client);
}

#[test]
fn session_search_clear_cancellation_and_covering_modal_preserve_the_filter() {
    let (client, mut rt) = search((393.0, 852.0), false, "UICTContentSizeCategoryL", false);
    let (x, y) = rt.find("phone-session-clear").unwrap();
    rt.touch(1, TouchPhase::Started, x, y, 0.3);
    settle(&mut rt);
    rt.touch(1, TouchPhase::Cancelled, x, y, 0.4);
    settle(&mut rt);
    assert_eq!(
        rt.state.phone.as_ref().unwrap().query.text,
        "nothing matches"
    );
    rt.update(|d, _| d.inbox.open = true);
    settle(&mut rt);
    assert!(!rt.accessibility_action("phone-session-clear", rune::accesskit::Action::Click, None));
    assert_eq!(
        rt.state.phone.as_ref().unwrap().query.text,
        "nothing matches"
    );
    no_terminal_input(&client);
}
