//! Top, bottom and side tabs: switching, scrolling and reordering.

use super::*;

#[gpui::test]
fn placement_switch_keeps_workspace_and_tab_actions_work_on_both_axes(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, path) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1000.), px(500.)));
    let original = tabs(3);
    for command in ["tabs-top", "tabs-bottom", "tabs-side"] {
        desk.update(cx, |desk, cx| {
            let workspace = &mut desk.link.as_mut().unwrap().workspace;
            workspace.tabs = original.clone();
            workspace.active_id = "fixture-b".into();
            cx.notify();
        });
        run(&desk, command, cx);
        desk.read_with(cx, |desk, _| {
            let workspace = &desk.link.as_ref().unwrap().workspace;
            assert_eq!(
                workspace.tabs, original,
                "placement never edits tab content"
            );
            assert_eq!(workspace.active_id, "fixture-b");
            assert_eq!(
                desk.prefs.sidebar_visible,
                command == "tabs-side",
                "tabs in the sidebar bring the sidebar"
            );
        });
        let a = cx
            .debug_bounds("tab-fixture-a")
            .expect("tab is visible wherever it is placed");
        let c = cx.debug_bounds("tab-fixture-c").unwrap();
        if command == "tabs-side" {
            assert_eq!(a.origin.x, c.origin.x);
            assert!(a.origin.y < c.origin.y);
            assert!(a.origin.y >= px(TITLEBAR));
        } else {
            assert!(a.origin.x < c.origin.x);
            assert_eq!(a.origin.y, c.origin.y);
            if command == "tabs-bottom" {
                let strip = cx.debug_bounds("bottom-tab-bar").unwrap();
                assert!(a.origin.y >= strip.origin.y);
                assert!(a.bottom() <= strip.bottom());
            } else {
                assert!(a.bottom() <= px(TITLEBAR));
            }
        }
        cx.simulate_click(a.center(), Modifiers::none());
        draw(cx);
        assert_eq!(
            desk.read_with(cx, |desk, _| desk
                .link
                .as_ref()
                .unwrap()
                .workspace
                .active_id
                .clone()),
            "fixture-a"
        );
        cx.simulate_mouse_down(a.center(), MouseButton::Left, Modifiers::none());
        draw(cx);
        let target = if command == "tabs-side" {
            point(c.center().x, c.bottom() - px(2.))
        } else {
            point(c.right() - px(2.), c.center().y)
        };
        cx.simulate_mouse_move(target, MouseButton::Left, Modifiers::none());
        draw(cx);
        cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::none());
        draw(cx);
        assert_eq!(ids(&desk, cx), ["fixture-b", "fixture-c", "fixture-a"]);
        assert_eq!(
            desk.read_with(cx, |desk, _| desk
                .link
                .as_ref()
                .unwrap()
                .workspace
                .active_id
                .clone()),
            "fixture-a"
        );
        if command == "tabs-side" {
            // A sidebar tab closes from its actions, as any row does.
            let more = cx.debug_bounds("tab-more-fixture-a").expect("tab actions");
            cx.simulate_click(more.center(), Modifiers::none());
            draw(cx);
            let menu = desk
                .read_with(cx, |desk, _| desk.menu_window())
                .expect("tab menu");
            let menu_cx = &mut VisualTestContext::from_window(menu, cx);
            let close = menu_cx
                .debug_bounds("glass-menu-close-tab")
                .expect("close in the tab menu");
            menu_cx.simulate_click(close.center(), Modifiers::none());
            menu_cx.run_until_parked();
            assert!(
                cx.debug_bounds("tab-close-fixture-c").is_none(),
                "no close glyph on sidebar rows"
            );
        } else {
            let close = cx.debug_bounds("tab-close-fixture-a").unwrap();
            cx.simulate_click(close.center(), Modifiers::none());
        }
        draw(cx);
        assert_eq!(ids(&desk, cx), ["fixture-b", "fixture-c"]);
        assert_eq!(
            desk.read_with(cx, |desk, _| desk
                .link
                .as_ref()
                .unwrap()
                .workspace
                .active_id
                .clone()),
            "fixture-c"
        );
        let c = cx.debug_bounds("tab-fixture-c").unwrap().center();
        cx.simulate_event(gpui::MouseDownEvent {
            position: c,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 2,
            first_mouse: false,
        });
        cx.simulate_event(gpui::MouseUpEvent {
            position: c,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 2,
        });
        draw(cx);
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.modal.clone()),
            Modal::Rename {
                tab_id: "fixture-c".into()
            },
        );
        cx.simulate_keystrokes("escape");
        draw(cx);
    }
    desk.update(cx, |desk, cx| {
        desk.role = desk::WindowRole::Quick;
        cx.notify();
    });
    draw(cx);
    let c = cx.debug_bounds("tab-fixture-c").unwrap();
    assert!(
        c.bottom() <= px(TITLEBAR),
        "Quick retains compact titlebar tabs"
    );
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.prefs.tab_position),
        TabPosition::Side
    );
    desk.update(cx, |desk, cx| {
        desk.role = desk::WindowRole::Main;
        cx.notify();
    });
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
    let saved = serde_json::from_slice(&std::fs::read(path.join("layout.json")).unwrap()).unwrap();
    assert_eq!(
        persist::parse_layout(&saved).unwrap().display.tab_position,
        TabPosition::Side
    );
}

#[gpui::test]
fn side_tabs_scroll_and_reorder_using_their_scrolled_positions(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(800.), px(360.)));
    desk.update(cx, |desk, cx| {
        let workspace = &mut desk.link.as_mut().unwrap().workspace;
        workspace.tabs = tabs(12);
        workspace.active_id = "fixture-a".into();
        cx.notify();
    });
    run(&desk, "tabs-side", cx);
    let shelf = cx
        .debug_bounds("sidebar-tab-shelf")
        .expect("fixed tab area");
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: shelf.center(),
        delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-210.))),
        ..Default::default()
    });
    draw(cx);
    let h = cx
        .debug_bounds("tab-fixture-h")
        .expect("scroll reveals later tabs");
    let i = cx.debug_bounds("tab-fixture-i").unwrap();
    assert!(h.origin.y >= px(TITLEBAR));
    assert!(
        i.bottom() < px(360.),
        "later tabs fit after scrolling a short window"
    );
    cx.simulate_mouse_down(h.center(), MouseButton::Left, Modifiers::none());
    draw(cx);
    let target = point(i.center().x, i.bottom() - px(2.));
    cx.simulate_mouse_move(target, MouseButton::Left, Modifiers::none());
    draw(cx);
    cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::none());
    draw(cx);
    let order = ids(&desk, cx);
    assert_eq!(&order[7..9], ["fixture-i", "fixture-h"]);
    assert_eq!(
        desk.read_with(cx, |desk, _| desk
            .link
            .as_ref()
            .unwrap()
            .workspace
            .active_id
            .clone()),
        "fixture-h"
    );
    let h = cx.debug_bounds("tab-fixture-h").unwrap().center();
    cx.simulate_mouse_down(h, MouseButton::Left, Modifiers::none());
    draw(cx);
    run(&desk, "tabs-top", cx);
    cx.simulate_mouse_move(
        point(px(700.), px(20.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_mouse_up(
        point(px(700.), px(20.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    draw(cx);
    assert_eq!(
        ids(&desk, cx),
        order,
        "changing placement cancels the old-axis drag"
    );
}

#[gpui::test]
fn overflowing_top_tabs_scroll_and_reorder_at_their_painted_positions(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(800.), px(360.)));
    desk.update(cx, |desk, cx| {
        let workspace = &mut desk.link.as_mut().unwrap().workspace;
        workspace.tabs = tabs(12);
        workspace.active_id = "fixture-a".into();
        cx.notify();
    });
    run(&desk, "tabs-top", cx);
    let hidden = cx.debug_bounds("tab-fixture-h").unwrap();
    assert!(hidden.origin.x > px(800.));
    cx.simulate_event(gpui::ScrollWheelEvent {
        // Far enough that the seventh tab clears the main menu rail.
        position: point(px(500.), px(TITLEBAR / 2.)),
        delta: gpui::ScrollDelta::Pixels(point(px(-560.), px(0.))),
        ..Default::default()
    });
    draw(cx);
    let g = cx.debug_bounds("tab-fixture-g").unwrap();
    let h = cx.debug_bounds("tab-fixture-h").unwrap();
    let rail = desk.read_with(cx, |desk, _| desk.prefs.sidebar_width as f32);
    assert!(g.origin.x >= px(rail));
    assert!(h.right() < px(800.), "scroll reveals the overflowed tabs");
    cx.simulate_mouse_down(g.center(), MouseButton::Left, Modifiers::none());
    draw(cx);
    let target = point(h.right() - px(2.), h.center().y);
    cx.simulate_mouse_move(target, MouseButton::Left, Modifiers::none());
    draw(cx);
    cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::none());
    draw(cx);
    assert_eq!(&ids(&desk, cx)[6..8], ["fixture-h", "fixture-g"]);
    assert_eq!(
        desk.read_with(cx, |desk, _| desk
            .link
            .as_ref()
            .unwrap()
            .workspace
            .active_id
            .clone()),
        "fixture-g"
    );
}

#[gpui::test]
fn plain_bottom_tabs_scroll_reorder_and_cancel_drag_on_placement_change(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(800.), px(360.)));
    desk.update(cx, |desk, cx| {
        let workspace = &mut desk.link.as_mut().unwrap().workspace;
        workspace.tabs = tabs(12);
        workspace.active_id = "fixture-a".into();
        cx.notify();
    });
    run(&desk, "plain-terminal", cx);
    run(&desk, "tabs-bottom", cx);
    let strip = cx.debug_bounds("bottom-tabs").expect("bottom scroller");
    assert!(cx.debug_bounds("tab-fixture-h").unwrap().right() > px(800.));
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: strip.center(),
        delta: gpui::ScrollDelta::Pixels(point(px(-560.), px(0.))),
        ..Default::default()
    });
    draw(cx);
    let g = cx.debug_bounds("tab-fixture-g").unwrap();
    let h = cx.debug_bounds("tab-fixture-h").unwrap();
    assert!(g.origin.x >= px(0.));
    assert!(h.right() < px(800.));
    cx.simulate_mouse_down(g.center(), MouseButton::Left, Modifiers::none());
    draw(cx);
    let target = point(h.right() - px(2.), h.center().y);
    cx.simulate_mouse_move(target, MouseButton::Left, Modifiers::none());
    draw(cx);
    cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::none());
    draw(cx);
    let order = ids(&desk, cx);
    assert_eq!(&order[6..8], ["fixture-h", "fixture-g"]);
    let g = cx.debug_bounds("tab-fixture-g").unwrap();
    cx.simulate_mouse_down(g.center(), MouseButton::Left, Modifiers::none());
    draw(cx);
    run(&desk, "tabs-top", cx);
    desk.read_with(cx, |desk, _| assert!(desk.drag_cursor().is_none()));
    cx.simulate_mouse_move(
        point(px(790.), px(15.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_mouse_up(
        point(px(790.), px(15.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    draw(cx);
    assert_eq!(ids(&desk, cx), order, "a moved strip invalidates the drag");
}

#[gpui::test]
fn unified_settings_changes_tab_placement_without_changing_work(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    let original = tabs(3);
    desk.update(cx, |desk, cx| {
        let workspace = &mut desk.link.as_mut().unwrap().workspace;
        workspace.tabs = original.clone();
        workspace.active_id = "fixture-b".into();
        cx.notify();
    });
    for (selector, position) in [
        ("tab-position-side", TabPosition::Side),
        ("tab-position-bottom", TabPosition::Bottom),
        ("tab-position-top", TabPosition::Top),
    ] {
        cx.update(|window, cx| desk.update(cx, |desk, cx| desk.execute("settings", window, cx)));
        draw(cx);
        cx.simulate_input("layout");
        draw(cx);
        cx.simulate_keystrokes("enter");
        draw(cx);
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.settings_page),
            Some(crate::app::settings_panel::Page::Layout)
        );
        let choice = cx
            .debug_bounds(selector)
            .expect("tab placement in focused settings page");
        cx.simulate_click(choice.center(), Modifiers::none());
        draw(cx);
        desk.read_with(cx, |desk, _| {
            assert_eq!(desk.prefs.tab_position, position);
            let workspace = &desk.link.as_ref().unwrap().workspace;
            assert_eq!(workspace.tabs, original);
            assert_eq!(workspace.active_id, "fixture-b");
        });
        cx.simulate_keystrokes("escape");
        draw(cx);
    }
}
