use super::*;

#[gpui::test]
fn agent_inventory_is_host_scoped_deduplicated_and_stable_across_states(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (desk, _) = fixture(cx);
    desk.update(cx, |desk, _| {
        install(
            desk,
            with_agent(
                connection(1, vec![session(7, "main")]),
                9,
                AgentState::Working,
            ),
        );
        observe(
            &mut desk.fleet,
            None,
            with_agent(connection(1, vec![session(7, "main")]), 9, AgentState::Idle),
        );
        observe(
            &mut desk.fleet,
            Some("mini"),
            with_agent(
                connection(2, vec![session(7, "main")]),
                9,
                AgentState::Blocked,
            ),
        );
        let before = desk.agent_catalog();
        assert_eq!(before.len(), 2);
        assert_ne!(before[0].destination, before[1].destination);
        assert_eq!(
            before[0].info.state,
            AgentState::Working,
            "active link wins over its observer"
        );
        desk.link
            .as_mut()
            .unwrap()
            .connection
            .agents
            .get_mut(&ResourceId::local(9))
            .unwrap()
            .state = AgentState::Done;
        let after = desk.agent_catalog();
        assert!(
            before.iter().zip(&after).all(|(a, b)| a.same_target(b)),
            "state changes never rearrange the grid"
        );
        desk.fleet
            .observers
            .get_mut(&Some("mini".into()))
            .unwrap()
            .connection
            .as_mut()
            .unwrap()
            .status = Status::Failed;
        assert_eq!(
            desk.agent_catalog().len(),
            1,
            "offline hosts never advertise stale agents as live"
        );
        desk.link
            .as_mut()
            .unwrap()
            .connection
            .topology
            .as_mut()
            .unwrap()
            .panes
            .clear();
        desk.fleet.observers.remove(&None);
        assert!(
            desk.agent_catalog().is_empty(),
            "orphaned agent metadata is not a terminal"
        );
    });
}

#[gpui::test]
fn fleet_is_opt_in_and_the_empty_view_dismisses_with_escape(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = launch(cx);
    let cx = &mut gpui::VisualTestContext::from_window(handle.into(), cx);
    cx.update(|_, cx| cx.set_reduce_motion(true));
    draw(cx);
    assert!(!shown(cx, "agent-fleet"));
    desk.update_in(cx, |desk, window, cx| {
        desk.execute("agent-fleet", window, cx)
    });
    draw(cx);
    assert!(!shown(cx, "agent-fleet-panel"));
    let (_, settings) = super::super::super::settings_panel::window_tests::opened(&desk, cx);
    assert_eq!(
        settings.read_with(cx, |settings, _| settings.page),
        super::super::super::settings_panel::Page::Layout
    );
    desk.update_in(cx, |desk, window, cx| {
        desk.close_modal(window, cx);
        desk.update_prefs(|prefs| prefs.agent_fleet = true, cx);
    });
    draw(cx);
    click_on(cx, "agent-fleet");
    assert!(shown(cx, "agent-fleet-panel"));
    assert!(shown(cx, "fleet-empty"));
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!shown(cx, "agent-fleet-panel"));
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.modal, super::super::super::desk::Modal::None)
    });
}

#[gpui::test]
fn fleet_keyboard_navigation_survives_a_narrow_resize(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = launch(cx);
    let cx = &mut gpui::VisualTestContext::from_window(handle.into(), cx);
    cx.update(|_, cx| cx.set_reduce_motion(true));
    desk.update_in(cx, |desk, window, cx| {
        desk.prefs.agent_fleet = true;
        install(desk, connection(1, vec![session(7, "main")]));
        let remote = with_agent(
            with_agent(
                connection(2, vec![session(8, "remote")]),
                9,
                AgentState::Working,
            ),
            10,
            AgentState::Idle,
        );
        let remote = with_agent(remote, 11, AgentState::Done);
        observe(&mut desk.fleet, Some("mini"), remote);
        desk.execute("agent-fleet", window, cx);
    });
    draw(cx);
    cx.simulate_keystrokes("right");
    draw(cx);
    cx.simulate_keystrokes("left");
    draw(cx);
    cx.simulate_resize(gpui::size(gpui::px(360.), gpui::px(480.)));
    draw(cx);
    let panel = cx.debug_bounds("agent-fleet-panel").unwrap();
    assert!(panel.right() <= gpui::px(360.));
    assert!(panel.bottom() <= gpui::px(480.));
    let first = cx.debug_bounds("fleet-agent-remote-mini-@9").unwrap();
    let second = cx.debug_bounds("fleet-agent-remote-mini-@10").unwrap();
    assert!(
        second.top() > first.top(),
        "a phone-width window has one column"
    );
    cx.simulate_keystrokes("down");
    draw(cx);
    cx.simulate_keystrokes("enter");
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert!(desk.fleet_view.pending.is_some());
        assert!(
            desk.link.as_ref().unwrap().workspace.awaiting_output(&[
                phux_client_runtime::control::Event::TerminalChanged {
                    terminal_id: ResourceId::local(10),
                }
            ]),
            "the warm socket immediately queues the selected terminal"
        );
        assert_eq!(desk.remote.as_deref(), Some("mini"));
    });
}

#[gpui::test]
fn fleet_click_revalidates_the_agent_and_pins_the_remote_incarnation(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = launch(cx);
    let cx = &mut gpui::VisualTestContext::from_window(handle.into(), cx);
    cx.update(|_, cx| cx.set_reduce_motion(true));
    desk.update(cx, |desk, _| {
        desk.prefs.agent_fleet = true;
        install(desk, connection(1, vec![session(7, "main")]));
        observe(
            &mut desk.fleet,
            Some("mini"),
            with_agent(
                connection(2, vec![session(8, "remote")]),
                9,
                AgentState::Blocked,
            ),
        );
    });
    let target = desk.read_with(cx, |desk, _| desk.agent_catalog()[0].clone());
    desk.update_in(cx, |desk, window, cx| {
        let mut recycled = target.clone();
        recycled.boot = Some("another-server".into());
        desk.open_fleet_agent(recycled, window, cx);
        assert!(desk.remote.is_none());
        assert!(desk.fleet_view.pending.is_none());
        desk.execute("agent-fleet", window, cx);
    });
    draw(cx);
    click_on(cx, "fleet-agent-remote-mini-@9");
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.remote.as_deref(), Some("mini"));
        assert_eq!(desk.session, "remote");
        assert_eq!(desk.modal, super::super::super::desk::Modal::None);
        assert!(desk.fleet_view.pending.is_some());
        assert!(
            desk.link.as_ref().unwrap().workspace.awaiting_output(&[
                phux_client_runtime::control::Event::TerminalChanged {
                    terminal_id: target.terminal.clone(),
                }
            ]),
            "the warm socket immediately queues the validated agent"
        );
    });
    desk.update(cx, |desk, _| {
        let mut changed = with_agent(
            connection(2, vec![session(8, "remote")]),
            9,
            AgentState::Working,
        );
        // The authority changed during the dial: never reveal a recycled id.
        changed.topology.as_mut().unwrap().authority = Some(authority(3));
        install(desk, changed);
        // An actual reconnect still waits for its fresh incarnation before reveal.
        desk.fleet_view.pending = Some(target.clone());
        desk.settle_fleet_terminal();
        assert!(desk.fleet_view.pending.is_none());
        assert!(desk.link.as_ref().unwrap().workspace.tabs.is_empty());

        let mut attached = with_agent(
            connection(2, vec![session(8, "remote")]),
            9,
            AgentState::Working,
        );
        attached.agents.clear(); // ATTACHED precedes the independent badge reply.
        install(desk, attached);
        desk.fleet_view.pending = Some(target.clone());
        desk.settle_fleet_terminal();
        assert!(desk.fleet_view.pending.is_some());
        assert_eq!(
            desk.link.as_ref().unwrap().workspace.pending(),
            1,
            "a valid topology reveals the terminal without waiting for its badge"
        );
    });
}
