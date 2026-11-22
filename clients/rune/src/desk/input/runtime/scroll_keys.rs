//! Upstream focused-scroller keys must not steal the app's input ownership.

use super::*;
use phux_protocol::ids::ResourceId;
use phux_protocol::input::key::PhysicalKey;
use phux_protocol::wire::frame::FrameKind;

fn settle(rt: &mut rune::Runtime<crate::desk::Desk>) {
    rt.frame(1.0);
    rt.frame(1.0);
}

#[test]
fn clicking_desktop_chrome_returns_typing_to_the_terminal() {
    let (client, mut rt) = crate::test_support::input_fixture();
    rt.update(|d, _| {
        d.connection.tabs.list = vec![
            crate::desk::Tab {
                id: [1; 16],
                name: "Shell".into(),
                resources: vec![ResourceId::local(1)],
                active: true,
                locked: false,
            },
            crate::desk::Tab {
                id: [2; 16],
                name: "Other tab".into(),
                resources: vec![ResourceId::local(2)],
                active: false,
                locked: false,
            },
        ];
    });
    settle(&mut rt);
    for control in ["tab-0", "tab-new", "sidebar-toggle"] {
        let (x, y) = rt.find(control).expect(control);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        settle(&mut rt);
        rt.update(|_, tx| assert_eq!(tx.focused(), None, "{control} kept focus"));
        let _ = client.take_outbound();
        for key in [Key::Text(" ".into()), Key::Down] {
            rt.key(crate::keys::plain(key));
            settle(&mut rt);
        }
        let events: Vec<_> = client
            .take_outbound()
            .iter()
            .filter_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
                FrameKind::InputKey { terminal_id, event } => Some((terminal_id, event.key)),
                _ => None,
            })
            .collect();
        assert_eq!(
            events,
            [
                (ResourceId::local(1), PhysicalKey::Space),
                (ResourceId::local(1), PhysicalKey::ArrowDown),
            ],
            "{control}: both keys should reach the terminal exactly once"
        );
        assert!(!rt.state.settings_open);
    }
}

#[test]
fn intentionally_focused_chrome_does_not_share_keys_with_the_terminal() {
    let (client, mut rt) = crate::test_support::input_fixture();
    settle(&mut rt);
    assert!(rt.accessibility_action("sidebar-toggle", rune::accesskit::Action::Focus, None));
    settle(&mut rt);
    let _ = client.take_outbound();

    rt.key(crate::keys::plain(Key::Text("never type this".into())));
    settle(&mut rt);
    rt.update(|d, tx| {
        assert_eq!(tx.focused(), Some("sidebar-toggle"));
        assert!(!d.runtime_ime(&rune::Ime::Commit("IME should not leak either".into()), tx));
    });
    assert!(client.take_outbound().is_empty());

    let sidebar = rt.state.sidebar_shown();
    rt.key(crate::keys::plain(Key::Text(" ".into())));
    settle(&mut rt);
    assert_ne!(
        rt.state.sidebar_shown(),
        sidebar,
        "keyboard activation works"
    );
    rt.update(|_, tx| assert_eq!(tx.focused(), Some("sidebar-toggle")));

    rt.key(crate::keys::plain(Key::Escape));
    settle(&mut rt);
    rt.update(|_, tx| assert_eq!(tx.focused(), None, "Escape returns to the terminal"));
    assert!(client.take_outbound().is_empty());
}

#[test]
fn scroller_keys_still_reach_the_terminal_in_both_forms() {
    for (phone, size) in [(false, (1100.0, 720.0)), (true, (393.0, 852.0))] {
        let (client, mut rt) = crate::test_support::input_fixture();
        rt.state.phone = phone.then(crate::desk::phone::Phone::default);
        rt.set_viewport(size, 2.0);
        settle(&mut rt);
        let (x, y) = rt.find("pane-@1").expect("real terminal");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        settle(&mut rt);
        let _ = client.take_outbound();
        for (key, expected, text, shift) in [
            (Key::Up, PhysicalKey::ArrowUp, None, false),
            (Key::Down, PhysicalKey::ArrowDown, None, false),
            (Key::Text(" ".into()), PhysicalKey::Space, Some(" "), false),
            (Key::Text(" ".into()), PhysicalKey::Space, Some(" "), true),
            (Key::PageUp, PhysicalKey::PageUp, None, false),
            (Key::PageDown, PhysicalKey::PageDown, None, false),
        ] {
            rt.key(KeyEvent {
                key,
                mods: Mods {
                    shift,
                    ..Mods::default()
                },
            });
            settle(&mut rt);
            let events: Vec<_> = client
                .take_outbound()
                .iter()
                .filter_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
                    FrameKind::InputKey { terminal_id, event } => Some((terminal_id, event)),
                    _ => None,
                })
                .collect();
            assert_eq!(events.len(), 1, "one terminal key, phone={phone}");
            assert_eq!(events[0].0, ResourceId::local(1));
            assert_eq!(events[0].1.key, expected);
            assert_eq!(events[0].1.text.as_deref(), text);
        }
    }
}

#[test]
fn scroller_keys_in_settings_edit_the_field_without_typing_behind_it() {
    for (phone, size) in [(false, (640.0, 360.0)), (true, (393.0, 852.0))] {
        let (client, mut rt) = crate::test_support::input_fixture();
        rt.state.phone = phone.then(crate::desk::phone::Phone::default);
        rt.set_viewport(size, 2.0);
        rt.update(|d, tx| {
            tx.instant();
            d.settings_open = true;
            d.settings_ui.query = rune::TextEdit::new("a");
        });
        settle(&mut rt);
        assert!(rt.accessibility_action("settings-query", rune::accesskit::Action::Focus, None));
        settle(&mut rt);
        let _ = client.take_outbound();
        for (key, shift) in [
            (Key::Up, false),
            (Key::Down, false),
            (Key::Text(" ".into()), false),
            (Key::Text(" ".into()), true),
            (Key::PageUp, false),
            (Key::PageDown, false),
        ] {
            rt.key(KeyEvent {
                key,
                mods: Mods {
                    shift,
                    ..Mods::default()
                },
            });
            settle(&mut rt);
        }
        assert_eq!(rt.state.settings_ui.query.text, "a  ");
        assert!(rt.state.settings_ui.query.focused);
        for bytes in client.take_outbound() {
            assert!(
                !matches!(
                    FrameKind::decode(&bytes).unwrap().0,
                    FrameKind::InputKey { .. } | FrameKind::InputPaste { .. }
                ),
                "settings keys reached the terminal, phone={phone}"
            );
        }
    }
}
