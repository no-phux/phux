//! Selecting tabs and terminals by click and modifier-click.

use super::*;

#[gpui::test]
fn modifier_click_selects_tab_subset_without_activation_or_drag_on_both_axes(
    cx: &mut TestAppContext,
) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1000.), px(500.)));
    let modifier = Modifiers {
        platform: true,
        ..Modifiers::none()
    };
    for command in ["tabs-top", "tabs-bottom", "tabs-side"] {
        desk.update(cx, selectable_work);
        run(&desk, command, cx);
        for selector in ["tab-fixture-a", "tab-fixture-b"] {
            let bounds = cx.debug_bounds(selector).unwrap();
            cx.simulate_mouse_down(bounds.center(), MouseButton::Left, modifier);
            desk.read_with(cx, |desk, _| {
                assert!(
                    desk.drag_cursor().is_none(),
                    "selection must not arm tab reorder"
                );
                assert_eq!(desk.link.as_ref().unwrap().workspace.active_id, "fixture-c");
            });
            cx.simulate_mouse_up(bounds.center(), MouseButton::Left, modifier);
            draw(cx);
        }
        desk.read_with(cx, |desk, _| {
            assert_eq!(
                desk.organization.selected,
                [resource_key(1), resource_key(2)]
            );
            assert_eq!(desk.modal, Modal::None);
        });
        assert_eq!(ids(&desk, cx), ["fixture-a", "fixture-b", "fixture-c"]);
        run(&desk, "move-resources", cx);
        desk.read_with(cx, |desk, _| {
            assert_eq!(desk.modal, Modal::MoveResources);
            assert_eq!(
                desk.organization.selected,
                [resource_key(1), resource_key(2)],
                "Move receives exactly A+B, not C"
            );
        });
        cx.simulate_keystrokes("escape");
        draw(cx);
        let a = cx.debug_bounds("tab-fixture-a").unwrap();
        cx.simulate_click(a.center(), modifier);
        draw(cx);
        desk.read_with(cx, |desk, _| {
            assert_eq!(desk.organization.selected, [resource_key(2)])
        });
        cx.simulate_click(a.center(), Modifiers::none());
        draw(cx);
        desk.read_with(cx, |desk, _| {
            assert_eq!(desk.link.as_ref().unwrap().workspace.active_id, "fixture-a");
            assert_eq!(
                desk.organization.selected,
                [resource_key(2)],
                "ordinary activation preserves selection"
            );
        });
    }
}

#[gpui::test]
fn tab_and_background_terminal_share_selection_and_busy_guard(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, selectable_work);
    run(&desk, "tabs-side", cx);
    let modifier = Modifiers {
        platform: true,
        ..Modifiers::none()
    };
    let background = cx
        .debug_bounds("pane-@4")
        .expect("unplaced terminal stays selectable");
    cx.simulate_click(background.center(), modifier);
    draw(cx);
    let a = cx.debug_bounds("tab-fixture-a").unwrap();
    cx.simulate_click(a.center(), modifier);
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert_eq!(
            desk.organization.selected,
            [resource_key(4), resource_key(1)]
        );
        assert_eq!(desk.link.as_ref().unwrap().workspace.active_id, "fixture-c");
    });
    desk.update(cx, |desk, _| desk.organization.planning = true);
    cx.simulate_click(a.center(), modifier);
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert_eq!(
            desk.organization.selected,
            [resource_key(4), resource_key(1)]
        );
        assert!(desk.drag_cursor().is_none());
    });
    desk.update(cx, |desk, _| desk.organization.planning = false);
}

#[gpui::test]
fn split_tab_selects_only_its_focused_terminal_and_keyless_tabs_still_activate(
    cx: &mut TestAppContext,
) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, selectable_work);
    desk.update(cx, |desk, cx| {
        let workspace = &mut desk.link.as_mut().unwrap().workspace;
        let LayoutNode::Leaf { placement } = workspace.tabs.remove(1).root else {
            unreachable!()
        };
        let tab = &mut workspace.tabs[0];
        layout::split_at(
            &mut tab.root,
            "fixture-a",
            placement,
            layout::Axis::Row,
            false,
        );
        tab.focused_id = "fixture-b".into();
        cx.notify();
    });
    run(&desk, "tabs-top", cx);
    let modifier = Modifiers {
        platform: true,
        ..Modifiers::none()
    };
    let a = cx.debug_bounds("tab-fixture-a").unwrap().center();
    cx.simulate_event(gpui::MouseDownEvent {
        position: a,
        modifiers: modifier,
        button: MouseButton::Left,
        click_count: 2,
        first_mouse: false,
    });
    // Selection is decided on press, even if Command is released before up.
    cx.simulate_event(gpui::MouseUpEvent {
        position: a,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 2,
    });
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert_eq!(
            desk.organization.selected,
            [resource_key(2)],
            "not every terminal in the split tab"
        );
        assert_eq!(
            desk.modal,
            Modal::None,
            "modifier double-click never renames"
        );
        assert_eq!(desk.link.as_ref().unwrap().workspace.active_id, "fixture-c");
    });
    desk.update(cx, |desk, cx| {
        let link = desk.link.as_mut().unwrap();
        link.workspace.tabs[0].focused_id = "fixture-a".into();
        link.connection.topology.as_mut().unwrap().panes[0].key = None;
        cx.notify();
    });
    draw(cx);
    cx.simulate_mouse_down(a, MouseButton::Left, modifier);
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.organization.selected, [resource_key(2)]);
        assert_eq!(desk.link.as_ref().unwrap().workspace.active_id, "fixture-a");
        assert!(
            desk.drag_cursor().is_some(),
            "keyless tabs keep normal activation/reorder"
        );
    });
    cx.simulate_mouse_up(a, MouseButton::Left, modifier);
    draw(cx);
}
