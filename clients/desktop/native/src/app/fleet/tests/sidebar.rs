use super::*;

#[gpui::test]
fn the_host_filter_opens_at_its_trigger_and_filters_without_navigation(cx: &mut TestAppContext) {
    use gpui::{Modifiers, VisualTestContext};
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = launch(cx);
    let cx = &mut VisualTestContext::from_window(*handle, cx);
    desk.update(cx, |desk, cx| {
        install(desk, connection(1, vec![session(7, "main")]));
        observe(
            &mut desk.fleet,
            Some("mini"),
            connection(2, vec![session(8, "remote")]),
        );
        desk.hosts.push(super::super::host::RemoteHost {
            name: "mini".into(),
            endpoint: "quic://fixture.invalid:1".into(),
        });
        cx.notify();
    });
    draw(cx);
    let trigger = cx.debug_bounds("sidebar-host-filter").unwrap();
    assert_eq!(
        desk.read_with(cx, |desk, _| desk
            .anchors
            .get(crate::app::menus::HOST_FILTER)),
        Some(trigger)
    );
    cx.simulate_click(trigger.center(), Modifiers::none());
    draw(cx);
    let menu = desk.read_with(cx, |desk, _| desk.menu_window()).unwrap();
    {
        let menu_cx = &mut VisualTestContext::from_window(menu, cx);
        draw(menu_cx);
        let local = menu_cx
            .debug_bounds("glass-menu-show-host-This Mac")
            .expect("machine checkboxes open directly, without a View submenu");
        menu_cx.simulate_click(local.center(), Modifiers::none());
        menu_cx.run_until_parked();
    }
    draw(cx);
    assert!(cx.debug_bounds("work-Local-7").is_none());
    assert!(cx.debug_bounds("work-mini-8").is_some());
    desk.read_with(cx, |desk, _| {
        assert!(desk.remote.is_none());
        assert_eq!(desk.session, "main");
    });
}

#[gpui::test]
fn session_names_keep_their_column_when_counts_and_hosts_are_long(cx: &mut TestAppContext) {
    use gpui::{VisualTestContext, px};
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = launch(cx);
    let cx = &mut VisualTestContext::from_window(*handle, cx);
    let host = "remote-with-a-long-machine-name";
    desk.update(cx, |desk, cx| {
        install(
            desk,
            with_pane(connection(1, vec![session(7, "local-session-name")])),
        );
        observe(
            &mut desk.fleet,
            Some(host),
            with_pane(connection(2, vec![session(8, "remote-session-name")])),
        );
        desk.prefs.sidebar_visible = true;
        cx.notify();
    });
    for width in [crate::settings::SIDEBAR_MIN, 248., 320.] {
        desk.update(cx, |desk, cx| {
            desk.update_prefs(|prefs| prefs.sidebar_width = width, cx)
        });
        draw(cx);
        for (row_selector, label_selector, detail_selector, actions_selector) in [
            (
                "work-Local-7",
                "session-label-Local-7",
                "session-detail-Local-7",
                "session-actions-Local-7",
            ),
            (
                "work-remote-with-a-long-machine-name-8",
                "session-label-remote-with-a-long-machine-name-8",
                "session-detail-remote-with-a-long-machine-name-8",
                "session-actions-remote-with-a-long-machine-name-8",
            ),
        ] {
            let row = cx.debug_bounds(row_selector).unwrap();
            let label = cx.debug_bounds(label_selector).unwrap();
            let detail = cx.debug_bounds(detail_selector).unwrap();
            let actions = cx.debug_bounds(actions_selector).unwrap();
            assert!(
                label.size.width >= px(80.),
                "{row_selector}: metadata must not squeeze away the session name at {width}"
            );
            assert_eq!(detail.size.width, label.size.width);
            assert!(
                detail.origin.y >= label.bottom(),
                "counts have a separate stable line"
            );
            assert_eq!(row.size.height, px(44.));
            assert!(actions.right() <= row.right() - px(crate::app::ui::list::ROW_INSET));
            assert!(row.right() <= px(width as f32));
        }
    }
}

#[gpui::test]
fn switching_hosts_retains_local_inventory_before_its_observer_is_ready(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (desk, _) = fixture(cx);
    desk.update(cx, |desk, cx| {
        install(desk, connection(1, vec![session(7, "local-work")]));
        observe(
            &mut desk.fleet,
            Some("mini"),
            connection(2, vec![session(8, "remote-work")]),
        );
        desk.hosts.push(super::super::host::RemoteHost {
            name: "mini".into(),
            endpoint: "quic://fixture.invalid:1".into(),
        });
        let target = desk
            .work_catalog()
            .into_iter()
            .find(|work| work.destination.host.is_some())
            .unwrap()
            .destination;
        // No local observer exists yet: switching must seed its inventory
        // before releasing the only successful local projection.
        assert!(!desk.fleet.observers.contains_key(&None));
        desk.navigate_work(target, cx);
        assert!(
            desk.work_catalog().iter().any(
                |work| work.destination.host.is_none() && work.destination.name == "local-work"
            )
        );
        desk.remote = None;
        desk.sync_fleet(cx);
        assert!(desk.fleet.observers.contains_key(&None));
    });
}

fn with_pane(mut connection: Connection) -> Connection {
    let session = &connection.topology.as_ref().unwrap().sessions[0];
    let pane = phux_client_runtime::control::PaneDescriptor {
        terminal_id: ResourceId::local(1),
        session_id: session.id,
        session_name: session.name.clone(),
        cols: 80,
        rows: 24,
        title: Some("shell".into()),
        cwd: None,
        is_focused: false,
        key: None,
        revision: None,
        name: None,
        activation: None,
        exec: None,
    };
    connection.topology.as_mut().unwrap().panes.push(pane);
    connection
}

#[gpui::test]
fn session_clicks_never_move_tree_rows_or_change_host_filters(cx: &mut TestAppContext) {
    use gpui::{Modifiers, VisualTestContext, px, size};
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = launch(cx);
    let cx = &mut VisualTestContext::from_window(*handle, cx);
    cx.simulate_resize(size(px(1000.), px(700.)));
    desk.update(cx, |desk, cx| {
        desk.prefs.sidebar_visible = true;
        desk.prefs.sidebar_grouping = crate::settings::SidebarGrouping::Machine;
        install(
            desk,
            with_pane(connection(
                1,
                vec![session(7, "main"), session(9, "sibling")],
            )),
        );
        observe(
            &mut desk.fleet,
            None,
            with_pane(connection(
                1,
                vec![session(7, "main"), session(9, "sibling")],
            )),
        );
        observe(
            &mut desk.fleet,
            Some("mini"),
            with_pane(connection(2, vec![session(8, "main")])),
        );
        desk.hosts.push(super::super::host::RemoteHost {
            name: "mini".into(),
            endpoint: "quic://fixture.invalid:1".into(),
        });
        cx.notify();
    });
    for position in [
        crate::settings::TabPosition::Top,
        crate::settings::TabPosition::Bottom,
        crate::settings::TabPosition::Side,
    ] {
        desk.update(cx, |desk, cx| {
            desk.update_prefs(|prefs| prefs.tab_position = position, cx);
        });
        draw(cx);
        let selectors = [
            "work-Local-7",
            "work-Local-9",
            "work-mini-8",
            "sidebar-footer",
        ];
        let before: Vec<_> = selectors
            .iter()
            .map(|selector| cx.debug_bounds(selector).expect(selector))
            .collect();
        assert!(
            cx.debug_bounds("catalog-pane-mini-@1").is_some(),
            "unselected sessions disclose their panes ({position:?}): {:?}",
            desk.read_with(cx, |desk, _| (
                desk.remote.clone(),
                desk.session.clone(),
                desk.work_catalog()
                    .iter()
                    .map(|work| (
                        work.destination.host.clone(),
                        work.panes.len(),
                        desk.work_is_active(&work.destination)
                    ))
                    .collect::<Vec<_>>()
            ))
        );
        for target in ["work-mini-8", "work-Local-9", "work-Local-7"] {
            let bounds = cx.debug_bounds(target).unwrap();
            cx.simulate_click(bounds.center(), Modifiers::none());
            draw(cx);
            let after: Vec<_> = selectors
                .iter()
                .map(|selector| cx.debug_bounds(selector).expect(selector))
                .collect();
            assert_eq!(
                before, after,
                "{target} changes focus, not tree geometry ({position:?})"
            );
            desk.read_with(cx, |desk, _| {
                assert!(desk.collapsed.is_empty());
                assert!(desk.prefs.sidebar_hidden_hosts.is_empty());
                assert_eq!(desk.work_catalog().len(), 3);
            });
            desk.update(cx, |desk, cx| {
                let topology = if desk.remote.is_some() {
                    with_pane(connection(2, vec![session(8, "main")])).topology
                } else {
                    with_pane(connection(
                        1,
                        vec![session(7, "main"), session(9, "sibling")],
                    ))
                    .topology
                };
                let link = desk.link.as_mut().unwrap();
                link.connection.topology = topology;
                link.connection.status = Status::Attached;
                // Different sessions can restore different numbers of tabs.
                // Neither the pending nor attached projection may move the tree.
                let count = if desk.remote.is_some() { 3 } else { 1 };
                link.workspace.tabs = (0..count)
                    .map(|index| {
                        let id = format!("restored-{index}");
                        crate::workspace::layout::DeskTab {
                            id: id.clone(),
                            title: Some(id.clone()),
                            focused_id: id.clone(),
                            zoomed_id: None,
                            root: crate::workspace::layout::LayoutNode::Leaf {
                                placement: crate::workspace::layout::Placement {
                                    id,
                                    terminal_id: None,
                                    terminal_label: String::new(),
                                    view_id: None,
                                },
                            },
                        }
                    })
                    .collect();
                link.workspace.active_id = "restored-0".into();
                cx.notify();
            });
            draw(cx);
            let attached: Vec<_> = selectors
                .iter()
                .map(|selector| cx.debug_bounds(selector).expect(selector))
                .collect();
            assert_eq!(
                before, attached,
                "{target} attachment preserves tree bounds ({position:?})"
            );
        }
    }
}

#[gpui::test]
fn only_the_caret_changes_disclosure_and_equal_names_on_other_hosts_stay_open(
    cx: &mut TestAppContext,
) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = launch(cx);
    let cx = &mut gpui::VisualTestContext::from_window(*handle, cx);
    desk.update(cx, |desk, cx| {
        install(desk, with_pane(connection(1, vec![session(7, "main")])));
        observe(
            &mut desk.fleet,
            Some("mini"),
            with_pane(connection(2, vec![session(8, "main")])),
        );
        desk.hosts.push(super::super::host::RemoteHost {
            name: "mini".into(),
            endpoint: "quic://fixture.invalid:1".into(),
        });
        cx.notify();
    });
    draw(cx);
    let local = cx.debug_bounds("work-Local-7").unwrap();
    let caret = cx.debug_bounds("disclose-work-mini-8").unwrap();
    cx.simulate_click(caret.center(), gpui::Modifiers::none());
    draw(cx);
    assert!(cx.debug_bounds("catalog-pane-mini-@1").is_none());
    assert!(
        cx.debug_bounds("pane-@1").is_some(),
        "same-named local session stays expanded"
    );
    assert_eq!(cx.debug_bounds("work-Local-7").unwrap(), local);
    let collapsed = desk.read_with(cx, |desk, _| {
        assert!(desk.remote.is_none());
        desk.collapsed.clone()
    });
    let remote = cx.debug_bounds("work-mini-8").unwrap();
    cx.simulate_click(remote.center(), gpui::Modifiers::none());
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.remote.as_deref(), Some("mini"));
        assert_eq!(desk.collapsed, collapsed);
    });
    let caret = cx.debug_bounds("disclose-work-mini-8").unwrap();
    cx.simulate_click(caret.center(), gpui::Modifiers::none());
    draw(cx);
    assert!(cx.debug_bounds("catalog-pane-mini-@1").is_some());
}

#[gpui::test]
fn hiding_the_active_host_is_an_explicit_filter_not_a_navigation(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = launch(cx);
    let cx = &mut gpui::VisualTestContext::from_window(*handle, cx);
    desk.update(cx, |desk, cx| {
        install(desk, connection(1, vec![session(7, "main")]));
        observe(
            &mut desk.fleet,
            Some("mini"),
            connection(2, vec![session(8, "main")]),
        );
        desk.update_prefs(|prefs| prefs.set_sidebar_host_visible(None, false), cx);
    });
    draw(cx);
    assert!(cx.debug_bounds("work-Local-7").is_none());
    assert!(cx.debug_bounds("work-mini-8").is_some());
    desk.read_with(cx, |desk, _| {
        assert!(desk.remote.is_none());
        assert_eq!(desk.session, "main");
        assert_eq!(desk.work_catalog().len(), 2);
    });
    let reset = cx.debug_bounds("sidebar-host-filter-reset").unwrap();
    cx.simulate_click(reset.center(), gpui::Modifiers::none());
    draw(cx);
    assert!(cx.debug_bounds("work-Local-7").is_some());
}
