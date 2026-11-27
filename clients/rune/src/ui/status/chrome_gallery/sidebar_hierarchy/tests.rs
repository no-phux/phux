use super::*;
use crate::ui::tokens::Paint;
use phux_protocol::wire::frame::FrameKind;

pub(super) fn no_terminal_input(rt: &Runtime<Desk>) {
    let client = rt.state.connection.client.as_ref().unwrap();
    for bytes in client.take_outbound() {
        let frame = phux_wire_fixtures::decode_one(&bytes);
        assert!(
            !matches!(
                frame,
                FrameKind::InputKey { .. } | FrameKind::InputPaste { .. }
            ),
            "disclosure sent terminal input: {frame:?}"
        );
    }
}

#[test]
fn local_rows_are_visible_and_arrow_free_including_terminal_descendants() {
    for size in [(1100, 720), (640, 360)] {
        let mut rt = window(size, true, "normal");
        for id in 1..=5 {
            let key = format!("pane-{}", ResourceId::local(id));
            assert!(crate::ui::sidebar::tests::focusable(&rt, &key));
        }
        for key in [
            "disclose-session-2",
            "fold-@1",
            "child-count-@1",
            "identity-layer-1",
        ] {
            assert!(rt.find(key).is_none(), "{key} is obsolete");
        }
        assert!(
            rt.elements()
                .iter()
                .any(|e| e.path.contains("pane-@2/") && e.label.as_deref() == Some("needs you"))
        );
        // The identity leads immediately after row padding, not a blank pointer gutter.
        let row = crate::ui::element(&rt, "pane-@1");
        assert_eq!(row.rect[0], crate::ui::element(&rt, "pane-@2").rect[0]);
        state(&mut rt, "hosts");
        assert!(
            rt.state
                .machines
                .expanded
                .contains(&Some("build-mini".to_owned()))
        );
    }
}

#[test]
fn local_horizontal_keys_keep_focus_without_folding_or_terminal_input() {
    for key in ["select-session-1", "pane-@1"] {
        let mut rt = window((1100, 720), true, "normal");
        super::super::sidebar_focus::focus_sidebar(&mut rt);
        if key == "select-session-1" {
            // Stay in the attached session: cross-session arrows need the
            // real server's Attach reply before selection follows focus.
            rt.key(crate::keys::plain(rune::Key::Up));
            rt.frame(3.0);
        }
        assert!(crate::ui::element(&rt, key).focused, "initial focus: {key}");
        let layout = rt.state.connection.layout.clone();
        let client = rt.state.connection.client.as_ref().unwrap().clone();
        let _ = client.take_outbound();
        for arrow in [
            rune::Key::Left,
            rune::Key::Right,
            rune::Key::Left,
            rune::Key::Right,
        ] {
            rt.key(crate::keys::plain(arrow.clone()));
            rt.frame(3.0);
            assert!(crate::ui::element(&rt, key).focused, "{key}");
            assert_eq!(rt.state.connection.layout, layout);
            for id in 1..=3 {
                assert!(crate::ui::sidebar::tests::focusable(
                    &rt,
                    &format!("pane-@{id}")
                ));
            }
        }
        no_terminal_input(&rt);
    }
}

#[test]
fn a_single_session_keeps_separate_session_and_resource_drag_ownership() {
    for (key, grab) in [
        (
            "select-session-1",
            crate::desk::Grab::Session(SessionKey::new([1; 16]).unwrap()),
        ),
        (
            "pane-@1",
            crate::desk::Grab::Resource(1, SessionKey::new([1; 16]).unwrap()),
        ),
    ] {
        let mut rt = window((1100, 720), true, "normal");
        state(&mut rt, "single");
        server::settle(&mut rt);
        let layout = rt.state.connection.layout.clone();
        let client = rt.state.connection.client.as_ref().unwrap().clone();
        let _ = client.take_outbound();
        let (x, y) = rt.find(key).unwrap();
        let other = if key == "select-session-1" {
            "pane-@1"
        } else {
            "select-session-1"
        };
        let (over_x, over_y) = rt.find(other).unwrap();
        assert!(
            rt.element_at(x, y)
                .unwrap()
                .path()
                .split('/')
                .any(|part| part == key)
        );
        rt.pointer_down(x, y);
        rt.pointer_move(over_x, over_y);
        rt.frame(3.0);
        assert_eq!(
            rt.state.connection.reorder.held.map(|(from, _)| from),
            Some(grab)
        );
        rt.pointer_up(x + 100.0, y + 40.0);
        rt.frame(3.0);
        assert_eq!(rt.state.connection.layout, layout);
        assert!(rt.state.connection.reorder.held.is_none());
        no_terminal_input(&rt);
    }
}

#[test]
fn a_flat_single_keeps_close_confirmation_without_selecting_or_dragging() {
    let mut rt = window((1100, 720), true, "normal");
    state(&mut rt, "single");
    let key = SessionKey::new([1; 16]).unwrap();
    let shown = rt.state.connection.shown.as_ref().unwrap().resource.clone();
    let layout = rt.state.connection.layout.clone();
    let client = rt.state.connection.client.as_ref().unwrap().clone();
    let _ = client.take_outbound();
    let (x, y) = rt.find("pane-@1").unwrap();
    rt.pointer_move(x, y);
    server::settle(&mut rt);
    press(&mut rt, &format!("session-close-{key}"));
    assert_eq!(
        rt.state.pending,
        Some(crate::desk::Pending::CloseSession(key, "phux".to_owned()))
    );
    assert_eq!(rt.state.connection.shown.as_ref().unwrap().resource, shown);
    assert_eq!(rt.state.connection.layout, layout);
    assert!(rt.state.connection.reorder.held.is_none());
    no_terminal_input(&rt);
}

#[test]
fn real_split_members_are_inline_and_attention_is_not_hidden_by_the_lead() {
    for dark in [true, false] {
        let mut rt = window((1100, 720), dark, "normal");
        state(&mut rt, "split");
        server::settle(&mut rt);
        let t = crate::ui::tokens_for(dark, None);
        let expected = t.roles.text.dim.paint();
        for id in 1..=3 {
            assert!(rt.find(&format!("sidebar-member-@{id}")).is_some());
        }
        let (x, y) = rt.find("pane-@1").unwrap();
        rt.pointer_move(x, y);
        rt.frame(3.0);
        assert!(rt.find("identity-layer-1").is_some());
        assert!(rt.find("identity-layer-2").is_some());
        assert!(
            rt.elements()
                .iter()
                .any(|e| e.path.contains("pane-@1/") && e.label.as_deref() == Some("needs you"))
        );
        assert!(
            rt.elements()
                .iter()
                .any(|e| e.text.as_deref() == Some("needs you"))
        );
        rt.pointer_down(x, y);
        rt.frame(3.0);
        assert!(
            rt.elements()
                .iter()
                .any(|e| e.text.as_deref() == Some("·") && e.text_color == Some(expected))
        );
        rt.pointer_up(x + 200.0, y + 50.0);
    }
}

fn wanted_or_shown(rt: &Runtime<Desk>, id: u32) -> bool {
    rt.state
        .connection
        .wanted
        .as_ref()
        .is_some_and(|(wanted, _)| *wanted == ResourceId::local(id))
        || rt
            .state
            .connection
            .shown
            .as_ref()
            .is_some_and(|s| s.resource == ResourceId::local(id))
}

#[test]
fn inline_member_gestures_own_their_press_and_never_drag_or_type_into_a_terminal() {
    let mut rt = window((1100, 720), true, "normal");
    state(&mut rt, "split");
    let client = rt.state.connection.client.as_ref().unwrap().clone();
    let _ = client.take_outbound();
    let layout = rt.state.connection.layout.clone();
    let (x, y) = rt.find("sidebar-member-@2").unwrap();
    rt.pointer_down(x, y);
    rt.frame(3.0);
    assert!(!wanted_or_shown(&rt, 2), "press-start does not select");
    rt.pointer_move(x + 150.0, y + 80.0);
    rt.frame(3.0);
    assert!(
        rt.state.connection.reorder.held.is_none(),
        "the member is not a drag handle"
    );
    rt.pointer_up(x + 150.0, y + 80.0);
    rt.frame(3.0);
    assert!(!wanted_or_shown(&rt, 2), "cancelled press does not select");
    press(&mut rt, "sidebar-member-@2");
    assert!(
        wanted_or_shown(&rt, 2),
        "the member, not the lead, was requested"
    );
    assert_eq!(rt.state.connection.layout, layout);
    assert!(rt.state.connection.reorder.held.is_none());
    rt.update(|_, tx| assert_eq!(tx.focused(), None));
    no_terminal_input(&rt);
}

#[test]
fn split_keyboard_navigation_keeps_the_row_without_terminal_input() {
    let mut rt = window((1100, 720), true, "normal");
    state(&mut rt, "split");
    super::super::sidebar_focus::focus_sidebar(&mut rt);
    let client = rt.state.connection.client.as_ref().unwrap().clone();
    let _ = client.take_outbound();
    let layout = rt.state.connection.layout.clone();
    rt.key(crate::keys::plain(rune::Key::Right));
    rt.frame(3.0);
    assert!(wanted_or_shown(&rt, 2));
    assert!(
        crate::ui::element(&rt, "pane-@1").focused,
        "lead identity stays put"
    );
    assert_eq!(rt.state.connection.layout, layout);
    no_terminal_input(&rt);
}

#[test]
fn machine_header_keeps_native_clearance_and_room_for_every_action() {
    for size in [(1100, 720), (640, 360)] {
        for mode in ["normal", "large", "forced"] {
            let mut rt = window(size, true, mode);
            rt.update(|d, _| {
                d.connection.keep_awake = crate::desk::keep_awake::Awake::parse(Some(
                    br#"{"state":"on","working":2,"mode":"while-agents-work"}"#,
                ));
            });
            server::settle(&mut rt);
            let band = crate::ui::element(&rt, "sidebar-native-titlebar");
            let controls = [
                "machine-switch",
                "sidebar-search",
                "keep-awake",
                "inbox-button",
            ]
            .map(|key| crate::ui::element(&rt, key));
            let min = phux_design::tokens::SPACE.pointer_min;
            assert!(controls[0].rect[2] >= min * 4.0, "{size:?}/{mode}");
            for control in &controls {
                assert!(control.rect[1] >= band.rect[1] + band.rect[3] - 0.01);
                assert!(
                    control.rect[2] >= min && control.rect[3] >= min,
                    "{control:?}"
                );
                assert!((control.rect[2] - control.visible[2]).abs() < 0.01);
                assert!((control.rect[3] - control.visible[3]).abs() < 0.01);
            }
            for pair in controls.windows(2) {
                assert!(pair[0].rect[0] + pair[0].rect[2] <= pair[1].rect[0] + 0.01);
            }
            assert_eq!(
                crate::ui::element(&rt, "machine-switch").label.as_deref(),
                Some(format!("Machine: {}", crate::machines::LOCAL).as_str()),
            );
        }
    }
}

#[test]
fn long_current_machine_names_do_not_cover_header_actions() {
    let host = "build-mini-with-a-very-long-project-and-team-name.example.tailnet";
    for size in [(1100, 720), (640, 360)] {
        for mode in ["normal", "large", "forced"] {
            let mut rt = window(size, true, mode);
            rt.update(|d, _| {
                d.machines.current = Some(host.to_owned());
                d.connection.keep_awake = crate::desk::keep_awake::Awake::parse(Some(
                    br#"{"state":"on","working":2,"mode":"while-agents-work"}"#,
                ));
            });
            server::settle(&mut rt);
            let machine = crate::ui::element(&rt, "machine-switch");
            let name = crate::ui::element(&rt, "machine-switch-name");
            assert_eq!(machine.label, Some(format!("Machine: {host}")));
            assert!(name.rect[2] >= phux_design::tokens::SPACE.pointer_min);
            assert!(name.rect[0] >= machine.rect[0]);
            assert!(name.rect[0] + name.rect[2] <= machine.rect[0] + machine.rect[2]);
            assert!((name.rect[2] - name.visible[2]).abs() < 0.01);
            let search = crate::ui::element(&rt, "sidebar-search");
            assert!(machine.rect[0] + machine.rect[2] <= search.rect[0]);
            for key in ["sidebar-search", "keep-awake", "inbox-button"] {
                let control = crate::ui::element(&rt, key);
                assert!((control.rect[2] - control.visible[2]).abs() < 0.01);
                assert!((control.rect[3] - control.visible[3]).abs() < 0.01);
            }
        }
    }
}
