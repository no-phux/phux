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
