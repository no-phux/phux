//! Pane chrome that shows a tab's shape: tab maps, the zoom stack and the capsule.

use super::*;

/// One tab split right: `fixture-a` beside `fixture-b`, focused on the first.
fn split_tab() -> DeskTab {
    let leaf = |id: &str| {
        Box::new(LayoutNode::Leaf {
            placement: Placement {
                id: id.into(),
                terminal_id: None,
                terminal_label: String::new(),
                view_id: None,
            },
        })
    };
    DeskTab {
        id: "split".into(),
        title: Some("Split".into()),
        root: LayoutNode::Split {
            id: "split-root".into(),
            axis: Axis::Row,
            ratio: 0.5,
            first: leaf("fixture-a"),
            second: leaf("fixture-b"),
        },
        focused_id: "fixture-a".into(),
        zoomed_id: None,
    }
}

#[gpui::test]
fn a_split_tab_maps_its_panes_and_a_zoom_stacks_the_rest_behind(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1000.), px(500.)));
    desk.update(cx, |desk, cx| {
        let workspace = &mut desk.link.as_mut().unwrap().workspace;
        let mut lone = tabs(1).remove(0);
        lone.id = "lone".into();
        workspace.tabs = vec![split_tab(), lone];
        workspace.active_id = "split".into();
        cx.notify();
    });
    draw(cx);
    let map = cx
        .debug_bounds("tab-map-split")
        .expect("a split tab maps its panes");
    let tab = cx.debug_bounds("tab-split").unwrap();
    assert!(tab.contains(&map.center()), "the map sits on its tab");
    assert!(
        cx.debug_bounds("tab-map-lone").is_none(),
        "one pane needs no map"
    );
    assert!(
        cx.debug_bounds("zoom-stack").is_none(),
        "nothing hides unzoomed"
    );

    let body = cx.debug_bounds("workspace-body").unwrap();
    run(&desk, "zoom", cx);
    let stack = cx
        .debug_bounds("zoom-stack")
        .expect("the hidden pane peeks out");
    assert!(stack.size.height > px(0.) && stack.origin.y >= body.origin.y);
    assert!(
        cx.debug_bounds("tab-map-split").is_some(),
        "a zoomed tab keeps its map"
    );

    cx.simulate_click(stack.center(), Modifiers::none());
    draw(cx);
    assert!(
        desk.read_with(cx, |desk, _| desk.active_tab().unwrap().zoomed_id.is_none()),
        "the stack restores the split"
    );
    assert!(cx.debug_bounds("zoom-stack").is_none());
}

#[gpui::test]
fn the_pane_capsule_groups_its_actions(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, |desk, cx| {
        let workspace = &mut desk.link.as_mut().unwrap().workspace;
        workspace.tabs = vec![split_tab()];
        workspace.active_id = "split".into();
        desk.chrome_hover = Some("fixture-a".into());
        cx.notify();
    });
    draw(cx);
    let capsule = cx
        .debug_bounds("pane-controls-fixture-a")
        .expect("the capsule shows on hover");
    let actions = cx.debug_bounds("pane-actions-fixture-a").unwrap();
    assert!(capsule.contains(&actions.center()));
    // No agent runs here, so nothing offers a conversation.
    assert!(
        cx.debug_bounds("conversation-setup-fixture-a").is_none()
            && cx.debug_bounds("pane-conversation-fixture-a").is_none()
    );
}

#[gpui::test]
fn conversation_switch_lives_in_the_hover_capsule_not_the_resting_corner(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    let mut connection = crate::workspace::view_tests::attached();
    let mut workspace = crate::workspace::view_tests::opened(&mut connection);
    let original = workspace.focused().unwrap().clone();
    crate::app::conversation::preview::seed(&mut connection, original.terminal_id.clone().unwrap());
    workspace.tabs = tabs(1);
    workspace.active_id = "fixture-a".into();
    let LayoutNode::Leaf { placement } = &mut workspace.tabs[0].root else {
        unreachable!()
    };
    placement.terminal_id = original.terminal_id;
    placement.view_id = original.view_id;
    desk.update(cx, |desk, cx| {
        desk.link = Some(crate::app::desk::Link {
            connection,
            workspace,
        });
        desk.prefs.native_agents = false;
        assert!(desk.conversation_eligible("fixture-a"));
        assert!(!desk.conversation_visible("fixture-a"));
        cx.notify();
    });
    draw(cx);
    let pane = cx.debug_bounds("pane-card-fixture-a").unwrap();
    assert!(cx.debug_bounds("pane-corner-fixture-a").is_none());
    assert!(cx.debug_bounds("pane-conversation-fixture-a").is_none());
    desk.update(cx, |desk, cx| {
        desk.chrome_hover = Some("fixture-a".into());
        cx.notify();
    });
    draw(cx);
    assert!(cx.debug_bounds("pane-controls-fixture-a").is_some());
    let switch = cx.debug_bounds("pane-conversation-fixture-a").unwrap();
    assert_eq!(cx.debug_bounds("pane-card-fixture-a"), Some(pane));
    cx.simulate_click(
        point(switch.right() - switch.size.width / 4., switch.center().y),
        Modifiers::none(),
    );
    draw(cx);
    assert!(desk.read_with(cx, |desk, _| desk.conversation_visible("fixture-a")));
    cx.simulate_click(
        point(switch.left() + switch.size.width / 4., switch.center().y),
        Modifiers::none(),
    );
    draw(cx);
    assert!(!desk.read_with(cx, |desk, _| desk.conversation_visible("fixture-a")));
    cx.simulate_mouse_move(pane.center(), None, Modifiers::none());
    desk.update(cx, |desk, cx| {
        desk.chrome_hover = None;
        desk.controls_hover = None;
        cx.notify();
    });
    draw(cx);
    assert!(cx.debug_bounds("pane-corner-fixture-a").is_none());
    assert!(cx.debug_bounds("pane-conversation-fixture-a").is_none());
}
