use super::*;
use crate::ui::tokens::Paint;
use phux_protocol::wire::frame::FrameKind;

fn no_terminal_input(rt: &Runtime<Desk>) {
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
fn disclosure_targets_are_consistent_and_do_not_select_or_drag_their_parent() {
    for size in [(1100, 720), (640, 360)] {
        let mut rt = window(size, true, "normal");
        let client = rt.state.connection.client.as_ref().unwrap().clone();
        let shown = rt.state.connection.shown.as_ref().unwrap().resource.clone();
        let layout = rt.state.connection.layout.clone();
        let selected = client.selected_session();
        let _ = client.take_outbound();
        for key in ["disclose-session-2", "fold-@1"] {
            let target = crate::ui::element(&rt, key);
            let min = phux_design::tokens::SPACE.pointer_min;
            assert_eq!((target.rect[2], target.rect[3]), (min, min));
            assert!(
                !target.focusable,
                "a disclosure is not another list Tab stop"
            );
            press(&mut rt, key);
            assert_eq!(rt.state.connection.shown.as_ref().unwrap().resource, shown);
            assert_eq!(rt.state.connection.layout, layout);
            assert_eq!(client.selected_session(), selected);
            assert!(rt.state.connection.reorder.held.is_none());
        }
        assert!(
            rt.state
                .sidebar_collapsed
                .contains(&SessionKey::new([2; 16]).unwrap())
        );
        assert!(rt.state.sidebar_unfolded.contains(&ResourceId::local(1)));
        no_terminal_input(&rt);
    }
}

#[test]
fn tree_keys_keep_focus_and_do_not_change_the_terminal_arrangement() {
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
            if key == "select-session-1" {
                assert_eq!(
                    rt.state
                        .sidebar_collapsed
                        .contains(&SessionKey::new([1; 16]).unwrap()),
                    arrow == rune::Key::Left,
                );
            } else {
                assert_eq!(
                    rt.state.sidebar_unfolded.contains(&ResourceId::local(1)),
                    arrow == rune::Key::Right,
                );
            }
        }
        assert!(
            !rt.state
                .sidebar_collapsed
                .contains(&SessionKey::new([1; 16]).unwrap())
        );
        if key == "pane-@1" {
            assert!(rt.state.sidebar_unfolded.contains(&ResourceId::local(1)));
        }
        no_terminal_input(&rt);
    }
}

#[test]
fn dragging_out_of_a_disclosure_cancels_without_selecting_an_unselected_parent() {
    for key in ["disclose-session-2", "fold-@1"] {
        let mut rt = window((1100, 720), true, "normal");
        rt.update(|d, _| {
            d.connection.shown = Some(crate::desk::Shown {
                resource: ResourceId::local(5),
                view: None,
            });
            d.connection.layout = Some(LayoutNode::Leaf(ResourceId::local(5)));
        });
        server::settle(&mut rt);
        let layout = rt.state.connection.layout.clone();
        let client = rt.state.connection.client.as_ref().unwrap().clone();
        let selected = client.selected_session();
        let _ = client.take_outbound();
        let (x, y) = rt.find(key).unwrap();
        assert!(
            rt.element_at(x, y)
                .unwrap()
                .path()
                .split('/')
                .any(|part| part == key)
        );
        rt.pointer_down(x, y);
        rt.pointer_move(x + 100.0, y + 40.0);
        rt.frame(3.0);
        assert!(rt.state.connection.reorder.held.is_none());
        rt.pointer_up(x + 100.0, y + 40.0);
        rt.frame(3.0);
        assert!(rt.state.sidebar_collapsed.is_empty());
        assert!(rt.state.sidebar_unfolded.is_empty());
        press(&mut rt, key);
        assert_eq!(
            rt.state.connection.shown.as_ref().unwrap().resource,
            ResourceId::local(5)
        );
        assert_eq!(rt.state.connection.layout, layout);
        assert!(rt.state.connection.wanted.is_none());
        assert!(rt.state.connection.reorder.held.is_none());
        assert_eq!(client.selected_session(), selected);
        no_terminal_input(&rt);
    }
}

#[test]
fn child_counts_remain_legible_on_live_hover_and_press() {
    for dark in [true, false] {
        let mut rt = window((1100, 720), dark, "normal");
        rt.update(|d, _| {
            d.connection.shown = Some(crate::desk::Shown {
                resource: ResourceId::local(5),
                view: None,
            });
        });
        server::settle(&mut rt);
        let t = crate::ui::tokens_for(dark, None);
        let expected = t.roles.text.dim.paint();
        let (x, y) = rt.find("pane-@1").unwrap();
        rt.pointer_move(x, y);
        rt.frame(3.0);
        assert_eq!(
            crate::ui::element(&rt, "child-count-@1").text_color,
            Some(expected)
        );
        rt.pointer_down(x, y);
        rt.frame(3.0);
        assert_eq!(
            crate::ui::element(&rt, "child-count-@1").text_color,
            Some(expected)
        );
        rt.pointer_up(x + 200.0, y + 50.0);
    }
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
