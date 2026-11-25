use super::*;

fn no_terminal_input(rt: &Runtime<Desk>) {
    use phux_protocol::wire::frame::FrameKind;
    let client = rt.state.connection.client.as_ref().unwrap();
    for bytes in client.take_outbound() {
        let frame = phux_wire_fixtures::decode_one(&bytes);
        assert!(
            !matches!(
                frame,
                FrameKind::InputKey { .. } | FrameKind::InputPaste { .. }
            ),
            "sidebar navigation leaked input: {frame:?}"
        );
    }
}

#[test]
fn sidebar_focus_reaches_grouped_row_and_inbox_without_changing_panes() {
    for size in [(1100, 720), (640, 360)] {
        for mode in ["normal", "collapsed", "closed"] {
            let mut rt = split_window(size, true, mode);
            if mode == "closed" {
                rt.update(|d, _| d.sidebar_open = false);
                rt.frame(3.0);
            }
            let narrow = rt.state.sidebar_narrow();
            let sidebar_preference = rt.state.sidebar_open;
            let shown = rt.state.connection.shown.as_ref().unwrap().resource.clone();
            let layout = rt.state.connection.layout.clone();
            let remembered = rt.state.connection.tabs.recent.clone();
            let tab_ids: Vec<_> = rt.state.connection.tabs.list.iter().map(|t| t.id).collect();
            let other = SessionKey::new([2; 16]).unwrap();
            rt.update(|d, _| {
                d.sidebar_collapsed.insert(other);
            });
            let _ = rt.state.connection.client.as_ref().unwrap().take_outbound();
            focus_sidebar(&mut rt);
            server::settle(&mut rt);
            let row = crate::ui::element(&rt, &format!("pane-{}", ResourceId::local(2)));
            assert!(row.focused && row.focus_visible, "{size:?}/{mode}: {row:?}");
            assert!(rt.state.sidebar_collapsed.contains(&other));
            if narrow {
                assert_eq!(rt.state.sidebar_open, sidebar_preference);
            }
            assert!(
                !rt.state
                    .sidebar_collapsed
                    .contains(&SessionKey::new([1; 16]).unwrap())
            );
            assert_eq!(rt.state.connection.shown.as_ref().unwrap().resource, shown);
            assert_eq!(rt.state.connection.layout, layout);
            assert_eq!(rt.state.connection.tabs.recent, remembered);
            assert_eq!(
                rt.state
                    .connection
                    .tabs
                    .list
                    .iter()
                    .map(|t| t.id)
                    .collect::<Vec<_>>(),
                tab_ids
            );
            // A second invocation must not fold a revealed sidebar.
            focus_sidebar(&mut rt);
            assert!(rt.state.sidebar_shown());
            previous(&mut rt);
            assert!(crate::ui::element(&rt, "inbox-button").focused);
            rt.key(crate::keys::plain(rune::Key::Text(" ".into())));
            rt.frame(3.0);
            assert!(rt.state.inbox.open);
            no_terminal_input(&rt);
        }
    }
}

#[test]
fn the_palettes_sidebar_focus_outlives_the_palette() {
    for closed in [false, true] {
        let mut rt = split_window((1100, 720), true, "normal");
        rt.update(|d, _| d.sidebar_open = !closed);
        rt.frame(3.0);
        rt.update(|d, _| d.toggle_palette());
        server::settle(&mut rt);
        let action = crate::palette::Target::Action(crate::keys::Shortcut::FocusSidebar);
        rt.update(|d, tx| d.palette_run_target(action, tx));
        server::settle(&mut rt);
        assert!(!rt.state.palette_open);
        let row = crate::ui::element(&rt, &format!("pane-{}", ResourceId::local(2)));
        assert!(row.focused && row.focus_visible, "closed={closed}: {row:?}");
    }
}

fn target(rt: &Runtime<Desk>, id: u32) -> Option<(ResourceId, Option<SessionKey>)> {
    crate::ui::sidebar_focus_target(&rt.state, &ResourceId::local(id))
}

#[test]
fn sidebar_focus_keeps_the_shown_child_row_without_unfolding_its_parent() {
    for grouped in [false, true] {
        for mode in ["normal", "collapsed"] {
            let mut rt = split_window((1100, 720), true, mode);
            rt.update(|d, _| {
                d.connection
                    .topology
                    .as_mut()
                    .unwrap()
                    .panes
                    .iter_mut()
                    .find(|p| p.terminal_id == ResourceId::local(1))
                    .unwrap()
                    .parent = Some(ResourceId::local(3));
                if !grouped {
                    d.connection.tabs.by_session.get_mut(&1).unwrap()[0].resources =
                        vec![ResourceId::local(1)];
                }
                d.sidebar_unfolded.clear();
            });
            rt.frame(3.0);
            let layout = rt.state.connection.layout.clone();
            let _ = rt.state.connection.client.as_ref().unwrap().take_outbound();
            focus_sidebar(&mut rt);
            server::settle(&mut rt);
            let lead = ResourceId::local(if grouped { 2 } else { 1 });
            let row = crate::ui::element(&rt, &format!("pane-{lead}"));
            assert!(row.focused && row.focus_visible);
            assert!(
                !crate::ui::element(&rt, "pane-@3").focused,
                "not its parent"
            );
            assert!(
                rt.state.sidebar_unfolded.is_empty(),
                "shown children reveal themselves"
            );
            assert_eq!(
                rt.state.connection.shown.as_ref().unwrap().resource,
                ResourceId::local(1)
            );
            assert_eq!(rt.state.connection.layout, layout);
            previous(&mut rt);
            assert!(crate::ui::element(&rt, "inbox-button").focused);
            rt.key(crate::keys::plain(rune::Key::Text(" ".into())));
            rt.frame(3.0);
            assert!(rt.state.inbox.open);
            no_terminal_input(&rt);
        }
    }
}

#[test]
fn sidebar_focus_uses_available_members_and_agent_precedence() {
    let mut rt = split_window((1100, 720), true, "normal");
    let key = SessionKey::new([1; 16]);
    rt.update(|d, _| {
        // @1 is first, but only the later @2 has an agent.
        d.activities.apply(
            &Event::AgentMetadata {
                terminal_id: ResourceId::local(1),
                value: None,
            },
            &[],
        );
        d.connection.tabs.by_session.get_mut(&1).unwrap()[0].resources =
            [1, 2].map(ResourceId::local).to_vec();
    });
    assert_eq!(target(&rt, 1), Some((ResourceId::local(2), key)));
    rt.update(|d, _| {
        d.activities.apply(
            &Event::AgentMetadata {
                terminal_id: ResourceId::local(2),
                value: None,
            },
            &[],
        );
    });
    assert_eq!(target(&rt, 2), Some((ResourceId::local(1), key)));
    rt.update(|d, _| {
        d.connection.tabs.by_session.get_mut(&1).unwrap()[0].resources =
            [99, 2, 1].map(ResourceId::local).to_vec();
    });
    assert_eq!(target(&rt, 1), Some((ResourceId::local(2), key)));
    assert_eq!(
        target(&rt, 5),
        Some((ResourceId::local(5), key)),
        "unplaced row"
    );
    assert_eq!(target(&rt, 99), None, "no fabricated row");
    rt.update(|d, _| {
        d.connection.topology.as_mut().unwrap().sessions[0].key = None;
    });
    assert_eq!(target(&rt, 1), Some((ResourceId::local(2), None)));
    rt.update(|d, _| d.connection.tabs.by_session.clear());
    assert_eq!(target(&rt, 1), Some((ResourceId::local(1), None)));
    rt.update(|d, _| d.connection.topology = None);
    assert_eq!(target(&rt, 1), None);
    assert_eq!(
        crate::ui::sidebar_focus_target(&rt.state, &ResourceId::satellite("remote", 1)),
        Some((ResourceId::satellite("remote", 1), None)),
        "federated rows keep their flat identity"
    );
}
