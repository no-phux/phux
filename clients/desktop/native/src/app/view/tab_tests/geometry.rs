//! Physical alignment stays independent of labels, focus, and tab placement.

use super::*;
use crate::app::ui::list::{ROW, ROW_INSET};

#[gpui::test]
fn sidebar_tabs_and_background_rows_share_columns_and_keep_actions_inside(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, selectable_work);
    desk.update(cx, |desk, cx| {
        for tab in &mut desk.link.as_mut().unwrap().workspace.tabs {
            tab.title = Some("A deliberately long terminal label ".repeat(12));
        }
        cx.notify();
    });
    run(&desk, "tabs-side", cx);
    for width in [200., 320.] {
        desk.update(cx, |desk, cx| {
            desk.update_prefs(|prefs| prefs.sidebar_width = width, cx);
        });
        draw(cx);
        let background = cx.debug_bounds("pane-label-@4").expect("background label");
        for (row, label, actions) in [
            ("tab-fixture-a", "tab-label-fixture-a", "tab-more-fixture-a"),
            ("tab-fixture-c", "tab-label-fixture-c", "tab-more-fixture-c"),
        ] {
            let row = cx.debug_bounds(row).unwrap();
            let label = cx.debug_bounds(label).unwrap();
            let actions = cx.debug_bounds(actions).unwrap();
            assert_eq!(
                label.origin.x, background.origin.x,
                "selected and idle tabs align"
            );
            assert_eq!(row.size.height, px(ROW), "long text never grows a row");
            assert!(
                (label.center().y - row.center().y).abs() <= px(0.5),
                "text remains centered within pixel-snapping precision"
            );
            assert_eq!(actions.right(), row.right() - px(ROW_INSET));
            assert!(
                label.right() <= actions.origin.x,
                "text never covers actions"
            );
        }
    }
}

#[gpui::test]
fn bottom_tabs_belong_to_the_workspace_and_leave_the_sidebar_footer_anchored(
    cx: &mut TestAppContext,
) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, selectable_work);
    run(&desk, "tabs-side", cx);
    for width in [640., 1000., 1400.] {
        cx.simulate_resize(gpui::size(px(width), px(500.)));
        draw(cx);
        let footer = cx.debug_bounds("sidebar-footer").unwrap();
        run(&desk, "tabs-bottom", cx);
        let main = cx.debug_bounds("main-column").unwrap();
        let strip = cx.debug_bounds("bottom-tab-bar").unwrap();
        let tab = cx.debug_bounds("tab-fixture-a").unwrap();
        let lead = desk.read_with(cx, |desk, _| {
            super::super::panes::lead_inset(desk, &desk.look())
        });
        assert_eq!(strip.origin.x, main.origin.x);
        assert_eq!(strip.right(), main.right());
        assert_eq!(tab.origin.x, main.origin.x + px(lead));
        assert_eq!(cx.debug_bounds("sidebar-footer").unwrap(), footer);
        assert!(tab.right() <= main.right());
        run(&desk, "tabs-side", cx);
    }
}

#[gpui::test]
fn top_tabs_start_on_the_pane_column_beside_the_sidebar(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, selectable_work);
    run(&desk, "tabs-side", cx);
    run(&desk, "tabs-top", cx);
    for width in [200., 320.] {
        desk.update(cx, |desk, cx| {
            desk.update_prefs(|prefs| prefs.sidebar_width = width, cx);
        });
        draw(cx);
        let main = cx.debug_bounds("main-column").unwrap();
        let tab = cx.debug_bounds("tab-fixture-a").unwrap();
        let lead = desk.read_with(cx, |desk, _| {
            super::super::panes::lead_inset(desk, &desk.look())
        });
        assert!(main.origin.x >= px(width as f32), "the sidebar shows");
        assert_eq!(tab.origin.x, main.origin.x + px(lead));
        // The sidebar's rows end LIST_INSET before its edge; the first card
        // starts one gutter after them.
        let gutter = desk.read_with(cx, |desk, _| desk.skin().shape.gutter);
        let rows_end = px(width as f32 - super::super::navigator::LIST_INSET);
        assert_eq!(main.origin.x + px(lead) - rows_end, px(gutter));
    }
}

#[gpui::test]
fn side_panels_never_squeeze_bottom_tab_navigation_away(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, selectable_work);
    run(&desk, "tabs-side", cx);
    run(&desk, "tabs-bottom", cx);
    desk.update(cx, |desk, cx| {
        desk.changes.open = true;
        // Dragged to its narrowest, so the bottom strip still fits at 1000.
        desk.changes.width = crate::app::changes::MIN_WIDTH;
        cx.notify();
    });
    for (width, placement) in [(640., TabPosition::Top), (1000., TabPosition::Bottom)] {
        desk.update(cx, |desk, cx| {
            desk.link.as_mut().unwrap().workspace.active_id = "fixture-a".into();
            cx.notify();
        });
        cx.simulate_resize(gpui::size(px(width), px(500.)));
        draw(cx);
        desk.read_with(cx, |desk, _| {
            assert_eq!(desk.prefs.tab_position, TabPosition::Bottom);
            assert_eq!(desk.tab_position(), placement);
        });
        let tab = cx
            .debug_bounds("tab-fixture-b")
            .expect("tab stays reachable");
        assert!(tab.origin.x >= px(0.) && tab.right() <= px(width));
        cx.simulate_click(tab.center(), Modifiers::none());
        draw(cx);
        desk.read_with(cx, |desk, _| {
            assert_eq!(desk.link.as_ref().unwrap().workspace.active_id, "fixture-b");
        });
    }
}

#[gpui::test]
fn toggling_a_panel_retires_a_drag_even_when_the_bottom_strip_returns(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    // Narrow enough that the open panel leaves the bottom strip no room.
    cx.simulate_resize(gpui::size(px(760.), px(500.)));
    desk.update(cx, selectable_work);
    run(&desk, "tabs-side", cx);
    run(&desk, "tabs-bottom", cx);
    let order = ids(&desk, cx);
    let a = cx.debug_bounds("tab-fixture-a").unwrap();
    cx.simulate_mouse_down(a.center(), MouseButton::Left, Modifiers::none());
    draw(cx);
    assert!(desk.read_with(cx, |desk, _| desk.drag_cursor().is_some()));
    run(&desk, "changes", cx);
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.tab_position(), TabPosition::Top);
        assert!(desk.drag_cursor().is_none());
    });
    run(&desk, "changes", cx);
    let c = cx.debug_bounds("tab-fixture-c").unwrap();
    let target = point(c.right() - px(2.), c.center().y);
    cx.simulate_mouse_move(target, MouseButton::Left, Modifiers::none());
    draw(cx);
    cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::none());
    draw(cx);
    assert_eq!(ids(&desk, cx), order, "the retired drag cannot resume");
}
