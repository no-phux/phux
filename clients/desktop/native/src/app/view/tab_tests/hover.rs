use super::super::tabs::hover::{pane_context, row_limit};
use super::*;
use crate::app::pane_identity::Harness;
use crate::connection::{AgentAttention, AgentInfo, AgentState};
use phux_protocol::ResourceId;
use std::time::Duration;

fn dwell(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(600));
    draw(cx);
}

#[gpui::test]
fn live_previews_suppress_open_and_pending_tab_context_in_every_position(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1000.), px(600.)));
    desk.update(cx, selectable_work);
    draw(cx);
    for position in ["tabs-top", "tabs-bottom", "tabs-side"] {
        run(&desk, position, cx);
        let tab = cx.debug_bounds("tab-fixture-c").unwrap();
        cx.simulate_mouse_move(tab.center(), None, Modifiers::none());
        dwell(cx);
        assert!(cx.debug_bounds("tab-context").is_some());
        cx.simulate_click(tab.center(), Modifiers::none());
        dwell(cx);
        assert!(cx.debug_bounds("tab-peek").is_some());
        assert!(cx.debug_bounds("tab-context").is_none());
        cx.simulate_keystrokes("escape");
        draw(cx);
        cx.simulate_mouse_move(point(px(500.), px(300.)), None, Modifiers::none());
        draw(cx);
        cx.simulate_mouse_move(tab.center(), None, Modifiers::none());
        // Open the shelf before the tooltip delay elapses.
        cx.simulate_click(tab.center(), Modifiers::none());
        dwell(cx);
        assert!(cx.debug_bounds("tab-context").is_none());
        assert!(cx.debug_bounds("tab-peek").is_some());
        cx.simulate_keystrokes("escape");
        draw(cx);
    }
}

// The shared resize helper re-reads the platform mouse position (0,0 in
// TestPlatform), so it would erase the simulated hover before tooltip prepaint.
fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }
}

#[gpui::test]
fn tab_hover_is_delayed_read_only_and_dismisses_in_every_position(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1000.), px(600.)));
    cx.update(|window, cx| window.bounds_changed(cx));
    desk.update(cx, selectable_work);
    draw(cx);
    for position in ["tabs-top", "tabs-bottom", "tabs-side"] {
        run(&desk, position, cx);
        let before = cx.debug_bounds("workspace-body").unwrap();
        let active = desk.read_with(cx, |desk, _| desk.active_tab_id());
        let tab = cx.debug_bounds("tab-fixture-a").unwrap();
        cx.simulate_mouse_move(tab.center(), None, Modifiers::none());
        draw(cx);
        assert!(cx.debug_bounds("tab-context").is_none(), "not instant");
        dwell(cx);
        let card = cx.debug_bounds("tab-context").expect("hover card");
        assert!(cx.debug_bounds("tab-context-pane-fixture-a").is_some());
        assert!(cx.debug_bounds("tab-context-pane-fixture-c").is_none());
        assert_eq!(cx.debug_bounds("workspace-body").unwrap(), before);
        assert_eq!(desk.read_with(cx, |desk, _| desk.active_tab_id()), active);
        desk.read_with(cx, |desk, _| {
            assert!(!desk.tab_peek.open);
            assert!(desk.organization.selected.is_empty());
        });
        assert!(card.left() >= px(0.) && card.right() <= px(1000.));
        assert!(card.top() >= px(0.) && card.bottom() <= px(600.));
        cx.simulate_mouse_move(before.center(), None, Modifiers::none());
        draw(cx);
        assert!(cx.debug_bounds("tab-context").is_none());
    }
}

#[gpui::test]
fn leaving_before_the_delay_cancels_tab_context_and_click_still_navigates(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, selectable_work);
    draw(cx);
    let tab = cx.debug_bounds("tab-fixture-a").unwrap();
    cx.simulate_mouse_move(tab.center(), None, Modifiers::none());
    draw(cx);
    cx.simulate_mouse_move(point(px(450.), px(300.)), None, Modifiers::none());
    dwell(cx);
    assert!(cx.debug_bounds("tab-context").is_none());
    cx.simulate_mouse_move(tab.center(), None, Modifiers::none());
    dwell(cx);
    assert!(cx.debug_bounds("tab-context").is_some());
    cx.simulate_click(tab.center(), Modifiers::none());
    draw(cx);
    assert!(cx.debug_bounds("tab-context").is_none());
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.active_tab_id())
            .as_deref(),
        Some("fixture-a")
    );
}

#[gpui::test]
fn tab_context_reuses_safe_pane_labels_directories_harnesses_and_states(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (_, desk, _) = fixture(cx);
    desk.update(cx, |desk, cx| {
        selectable_work(desk, cx);
        let terminal = ResourceId::local(1);
        let connection = &mut desk.link.as_mut().unwrap().connection;
        let pane = &mut connection.topology.as_mut().unwrap().panes[0];
        pane.name = Some("review\n\u{202e} changes".into());
        pane.cwd = Some("/Users/person/work\n\u{2066}/phux".into());
        for (state, word) in [
            (AgentState::Working, "working"),
            (AgentState::Blocked, "needs you"),
            (AgentState::Done, "finished"),
            (AgentState::Idle, "idle"),
            (AgentState::Unknown, ""),
        ] {
            desk.link.as_mut().unwrap().connection.agents.insert(
                terminal.clone(),
                AgentInfo {
                    name: "reviewer".into(),
                    kind: Some("opencode-v2".into()),
                    state,
                    attention: AgentAttention::Low,
                    changed_at_ms: 0,
                },
            );
            let placement =
                layout::focused_placement(&desk.link.as_ref().unwrap().workspace.tabs[0]).unwrap();
            let context = pane_context(desk, placement);
            assert_eq!(context.title, "review changes");
            assert_eq!(
                context.directory.as_deref(),
                Some("/Users/person/work/phux")
            );
            assert_eq!(context.harness, Some(Harness::OpenCode));
            assert_eq!(context.state, Some(state));
            assert_eq!(context.status, word);
        }
        desk.link.as_mut().unwrap().connection.agents.clear();
        let placement =
            layout::focused_placement(&desk.link.as_ref().unwrap().workspace.tabs[0]).unwrap();
        let context = pane_context(desk, placement);
        assert_eq!(context.harness, None);
        assert_eq!(context.state, None);
        assert_eq!(context.status, "");
        let empty = tabs(1);
        let missing = pane_context(desk, layout::focused_placement(&empty[0]).unwrap());
        assert_eq!(missing.title, "shell");
        assert_eq!(missing.directory, None);
    });
}

#[gpui::test]
fn zoomed_tab_context_includes_hidden_panes_and_bounds_large_lists(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(800.), px(600.)));
    cx.update(|window, cx| window.bounds_changed(cx));
    desk.update(cx, |desk, cx| {
        selectable_work(desk, cx);
        let tab = &mut desk.link.as_mut().unwrap().workspace.tabs[0];
        for index in 0..12 {
            let placement = Placement {
                id: format!("extra-{index}"),
                terminal_id: None,
                terminal_label: String::new(),
                view_id: None,
            };
            layout::split_at(&mut tab.root, "fixture-a", placement, Axis::Row, false);
        }
        tab.zoomed_id = Some("fixture-a".into());
        cx.notify();
    });
    draw(cx);
    let tab = cx.debug_bounds("tab-fixture-a").unwrap();
    cx.simulate_mouse_move(tab.center(), None, Modifiers::none());
    dwell(cx);
    assert!(cx.debug_bounds("tab-context-more").is_some());
    let placements = desk.read_with(cx, |desk, _| {
        layout::placements(&desk.link.as_ref().unwrap().workspace.tabs[0].root)
            .into_iter()
            .map(|placement| placement.id.clone())
            .collect::<Vec<_>>()
    });
    for (index, id) in placements.iter().enumerate() {
        // GPUI's test selector API accepts only static strings (13 per fixture).
        let selector = Box::leak(format!("tab-context-pane-{id}").into_boxed_str());
        assert_eq!(cx.debug_bounds(selector).is_some(), index < row_limit(600.));
    }
    let card = cx.debug_bounds("tab-context").unwrap();
    assert!(card.size.height <= px(500.));
    assert_eq!(row_limit(600.), 8);
    assert_eq!(row_limit(300.), 3);
    assert_eq!(row_limit(0.), 1);

    cx.simulate_mouse_move(point(px(700.), px(300.)), None, Modifiers::none());
    cx.simulate_resize(gpui::size(px(400.), px(300.)));
    cx.update(|window, cx| window.bounds_changed(cx));
    desk.update(cx, |desk, cx| {
        let link = desk.link.as_mut().unwrap();
        link.workspace.tabs[0].title = Some("long workspace title ".repeat(30));
        link.connection.topology.as_mut().unwrap().panes[0].cwd = Some("/long-folder".repeat(50));
        link.connection.agents.insert(
            ResourceId::local(1),
            AgentInfo {
                name: "reviewer".into(),
                kind: Some("codex".into()),
                state: AgentState::Blocked,
                attention: AgentAttention::Low,
                changed_at_ms: 0,
            },
        );
        cx.notify();
    });
    draw(cx);
    let tab = cx.debug_bounds("tab-fixture-a").unwrap();
    cx.simulate_mouse_move(tab.center(), None, Modifiers::none());
    dwell(cx);
    let card = cx.debug_bounds("tab-context").unwrap();
    assert!(card.left() >= px(0.) && card.right() <= px(400.));
    assert!(card.top() >= px(0.) && card.bottom() <= px(300.));
    assert!(
        cx.debug_bounds("tab-context-harness").is_some(),
        "blocked agents retain their harness mark"
    );
}
