//! The chrome around the panes: titlebar, sidebar, paint and controls.

use super::*;

#[gpui::test]
fn pane_actions_stay_open_after_the_pointer_leaves_the_capsule(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, |desk, cx| {
        let workspace = &mut desk.link.as_mut().unwrap().workspace;
        workspace.tabs = tabs(1);
        workspace.active_id = "fixture-a".into();
        desk.chrome_hover = Some("fixture-a".into());
        cx.notify();
    });
    draw(cx);
    let actions = cx
        .debug_bounds("pane-actions-fixture-a")
        .expect("pane actions");
    cx.simulate_click(actions.center(), Modifiers::none());
    draw(cx);
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.pane_menu_open.clone())
            .as_deref(),
        Some("fixture-a")
    );
    cx.simulate_mouse_move(point(px(12.), px(300.)), None, Modifiers::none());
    draw(cx);
    assert!(
        cx.debug_bounds("pane-actions-fixture-a").is_some(),
        "the trigger stays mounted while its menu is open"
    );
    assert!(desk.read_with(cx, |desk, _| desk.menu_open("pane-fixture-a")));
    desk.update(cx, |desk, cx| desk.close_menu(cx));
    draw(cx);
    assert!(desk.read_with(cx, |desk, _| desk.pane_menu_open.is_none()));
    assert!(!desk.read_with(cx, |desk, _| desk.menu_open("pane-fixture-a")));
}

#[test]
fn the_slim_strip_fades_controls_and_shows_tabs_only_when_several() {
    use crate::settings::skin::{GLASS, TERMINAL};
    assert_eq!(controls_fit(GLASS.chrome.titlebar), 1.);
    assert_eq!(controls_fit(TERMINAL.chrome.titlebar), 0.);
    let mid = controls_fit(GLASS.lerp(&TERMINAL, 0.5).chrome.titlebar);
    assert!(mid > 0. && mid < 1.);
    assert_eq!(tab_height(GLASS.chrome.titlebar), 30.);
    assert!(tab_height(TERMINAL.chrome.titlebar) < TERMINAL.chrome.titlebar);
    assert!(tabs_shown(&GLASS, 1));
    assert!(!tabs_shown(&TERMINAL, 1));
    assert!(tabs_shown(&TERMINAL, 2));
    assert_eq!(gloss(&GLASS), 1.);
    assert_eq!(gloss(&TERMINAL), 0.);
    assert_eq!(row_radius(&GLASS), 6.);
    assert_eq!(chip_radius(&GLASS), 4.);
}

#[gpui::test]
fn the_titlebar_toggle_switches_to_a_plain_terminal_and_back(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1000.), px(500.)));
    desk.update(cx, |desk, cx| {
        let workspace = &mut desk.link.as_mut().unwrap().workspace;
        workspace.tabs = tabs(1);
        workspace.active_id = "fixture-a".into();
        cx.notify();
    });
    draw(cx);
    assert!(
        cx.debug_bounds("tab-fixture-a").is_some(),
        "glass shows one tab"
    );
    let toggle = cx
        .debug_bounds("skin-toggle")
        .expect("toggle in the titlebar");
    assert!(toggle.bottom() <= px(TITLEBAR));
    cx.simulate_click(toggle.center(), Modifiers::none());
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert!(desk.prefs.plain);
        assert_eq!(desk.skin().id, "terminal");
    });
    assert!(
        cx.debug_bounds("tab-fixture-a").is_none(),
        "a plain terminal hides a lone tab"
    );
    desk.update(cx, |desk, cx| {
        desk.link.as_mut().unwrap().workspace.tabs = tabs(2);
        cx.notify();
    });
    draw(cx);
    let strip = crate::settings::skin::TERMINAL.chrome.titlebar;
    let tab = cx.debug_bounds("tab-fixture-b").expect("two tabs show");
    assert!(tab.bottom() <= px(strip));
    // The way back is the same button, revealed by hovering the strip.
    let toggle = cx.debug_bounds("skin-toggle").expect("toggle in the strip");
    assert!(toggle.bottom() <= px(strip));
    cx.simulate_mouse_move(toggle.center(), None, Modifiers::none());
    draw(cx);
    cx.simulate_click(toggle.center(), Modifiers::none());
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert!(!desk.prefs.plain);
        assert_eq!(desk.skin().id, "glass");
    });
}

#[gpui::test]
fn plain_terminal_tabs_toggle_top_bottom_and_persist(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, path) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1000.), px(500.)));
    desk.update(cx, |desk, cx| {
        let workspace = &mut desk.link.as_mut().unwrap().workspace;
        workspace.tabs = tabs(2);
        workspace.active_id = "fixture-a".into();
        cx.notify();
    });
    run(&desk, "plain-terminal", cx);
    run(&desk, "tabs-bottom", cx);
    let bottom = cx.debug_bounds("bottom-tab-bar").expect("bottom strip");
    assert_eq!(bottom.bottom(), px(500.));
    assert!(cx.debug_bounds("top-tabs").is_none());
    let a = cx.debug_bounds("tab-fixture-a").expect("plain bottom tab");
    assert!(a.origin.y >= bottom.origin.y);
    assert!(a.bottom() <= bottom.bottom());
    desk.read_with(cx, |desk, _| {
        assert!(desk.prefs.plain);
        assert!(!desk.sidebar_shown());
        assert_eq!(desk.tab_position(), TabPosition::Bottom);
    });
    run(&desk, "tabs-top", cx);
    assert!(cx.debug_bounds("bottom-tab-bar").is_none());
    let a = cx.debug_bounds("tab-fixture-a").expect("plain top tab");
    assert!(a.bottom() <= px(crate::settings::skin::TERMINAL.chrome.titlebar));
    run(&desk, "tabs-bottom", cx);
    desk.update(cx, |desk, cx| {
        desk.role = desk::WindowRole::Quick;
        cx.notify();
    });
    draw(cx);
    assert!(cx.debug_bounds("bottom-tab-bar").is_none());
    assert!(cx.debug_bounds("top-tabs").is_some());
    desk.update(cx, |desk, cx| {
        desk.role = desk::WindowRole::Main;
        desk.link.as_mut().unwrap().workspace.tabs = tabs(1);
        cx.notify();
    });
    draw(cx);
    assert!(
        cx.debug_bounds("bottom-tab-bar").is_none(),
        "plain hides a lone tab"
    );
    run(&desk, "plain-terminal", cx);
    assert!(
        cx.debug_bounds("bottom-tab-bar").is_some(),
        "glass shows a lone tab"
    );
    run(&desk, "plain-terminal", cx);
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
    let saved = serde_json::from_slice(&std::fs::read(path.join("layout.json")).unwrap()).unwrap();
    let display = persist::parse_layout(&saved).unwrap().display;
    assert!(display.plain);
    assert_eq!(display.tab_position, TabPosition::Bottom);
}

#[gpui::test]
fn the_paint_is_built_once_until_preferences_change(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (_, desk, _) = fixture(cx);
    desk.update(cx, |desk, cx| {
        let first = desk.paint();
        assert!(std::rc::Rc::ptr_eq(&first, &desk.paint()));
        assert!(std::rc::Rc::ptr_eq(&first.look, &desk.look()));
        assert_eq!(first.palette, desk.palette());
        desk.apply_prefs(|prefs| prefs.terminal_opacity = 0.5, cx);
        let second = desk.paint();
        assert!(!std::rc::Rc::ptr_eq(&first, &second));
        assert!(std::rc::Rc::ptr_eq(&second, &desk.paint()));
        desk.sync_bindings(cx);
        assert!(!std::rc::Rc::ptr_eq(&second, &desk.paint()));
    });
}

#[gpui::test]
fn a_solid_window_goes_opaque_live_and_returns_to_blur(cx: &mut TestAppContext) {
    use crate::settings::WindowBackground;
    use crate::settings::skin::Backdrop;
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    draw(cx);
    let background = |desk: &Desk| desk.paint().terminal.background.expect("background").a;
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.skin().material.backdrop, Backdrop::Glass);
        assert!(background(desk) < 1.);
        assert!(desk.look().sheet.a < 1.);
    });
    desk.update(cx, |desk, cx| {
        desk.update_prefs(
            |prefs| prefs.window_background = WindowBackground::Solid,
            cx,
        );
    });
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.skin().material.backdrop, Backdrop::Opaque);
        assert_eq!(desk.skin().id, "glass", "the skin keeps its shape");
        assert_eq!(background(desk), 1.);
        assert_eq!(desk.look().sheet.a, 1.);
    });
    desk.update(cx, |desk, cx| {
        desk.update_prefs(|prefs| prefs.window_background = WindowBackground::Blur, cx);
    });
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.skin().material.backdrop, Backdrop::Glass);
        assert!(background(desk) < 1.);
    });
}

#[gpui::test]
fn shown_chords_follow_the_keymap(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (_, desk, _) = fixture(cx);
    desk.read_with(cx, |desk, _| {
        let shown =
            crate::settings::shown_chords(desk.keymap(), &crate::settings::default_chords());
        assert!(!shown.is_empty());
        for (command, chord) in shown {
            assert_eq!(desk.bound_chord(&command), Some(chord.as_str()));
            assert_eq!(
                desk.shown_chord(&command),
                Some(crate::settings::display_chord(&chord))
            );
        }
        assert_eq!(desk.shown_chord("no-such-command"), None);
    });
}

#[gpui::test]
fn sidebar_tabs_live_in_a_fixed_shelf_and_return_to_the_titlebar_when_it_hides(
    cx: &mut TestAppContext,
) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1000.), px(500.)));
    desk.update(cx, |desk, cx| {
        let workspace = &mut desk.link.as_mut().unwrap().workspace;
        workspace.tabs = tabs(2);
        workspace.active_id = "fixture-a".into();
        cx.notify();
    });
    run(&desk, "tabs-side", cx);
    let width = desk.read_with(cx, |desk, _| desk.prefs.sidebar_width as f32);
    let shelf = cx
        .debug_bounds("sidebar-tab-shelf")
        .expect("tabs keep their own fixed area");
    let tab = cx
        .debug_bounds("tab-fixture-a")
        .expect("tab in the sidebar");
    assert!(tab.right() <= px(width), "the tab is inside the sidebar");
    assert!(tab.origin.y >= shelf.origin.y, "inside the shelf");
    assert_eq!(shelf.size.height, px(160.));
    assert!(
        cx.debug_bounds("top-tabs").is_none(),
        "one place for tabs, not two"
    );
    run(&desk, "sidebar", cx);
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.prefs.tab_position, TabPosition::Side);
        assert_eq!(desk.tab_position(), TabPosition::Top);
    });
    let tab = cx
        .debug_bounds("tab-fixture-a")
        .expect("tabs never disappear with the sidebar");
    assert!(tab.bottom() <= px(TITLEBAR));
}

#[gpui::test]
fn sidebar_groupings_are_views_that_bring_the_sidebar(cx: &mut TestAppContext) {
    use crate::settings::SidebarGrouping;
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    for (command, grouping) in [
        ("group-machines", SidebarGrouping::Machine),
        ("group-none", SidebarGrouping::None),
        ("group-projects", SidebarGrouping::Project),
    ] {
        run(&desk, command, cx);
        desk.read_with(cx, |desk, _| {
            assert_eq!(desk.prefs.sidebar_grouping, grouping);
            assert!(desk.prefs.sidebar_visible);
        });
        assert!(cx.debug_bounds("sidebar-view").is_some());
    }
}

#[gpui::test]
fn switching_tabs_never_resizes_the_chrome(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1000.), px(500.)));
    desk.update(cx, |desk, cx| {
        desk.prefs.sidebar_visible = true;
        let wide = "a terminal label far wider than the window it sits in ".repeat(8);
        let leaf = |id: &str| LayoutNode::Leaf {
            placement: Placement {
                id: id.into(),
                terminal_id: None,
                terminal_label: wide.clone(),
                view_id: None,
            },
        };
        let mut tabs = tabs(2);
        tabs[1].title = Some(wide.clone());
        tabs[1].root = LayoutNode::Split {
            id: "wide-split".into(),
            axis: Axis::Row,
            ratio: 0.5,
            first: Box::new(leaf("fixture-b")),
            second: Box::new(leaf("fixture-b2")),
        };
        let workspace = &mut desk.link.as_mut().unwrap().workspace;
        workspace.tabs = tabs;
        workspace.active_id = "fixture-a".into();
        cx.notify();
    });
    draw(cx);
    let width = px(desk.read_with(cx, |desk, _| desk.prefs.sidebar_width as f32));
    for tab in ["fixture-a", "fixture-b", "fixture-a"] {
        desk.update(cx, |desk, cx| {
            desk.link.as_mut().unwrap().workspace.active_id = tab.into();
            cx.notify();
        });
        draw(cx);
        let main = cx.debug_bounds("main-column").expect("the panes column");
        assert_eq!(main.origin.x, width, "{tab} keeps the sidebar's width");
        assert_eq!(
            main.origin.y,
            px(TITLEBAR),
            "{tab} keeps the titlebar's height"
        );
        assert_eq!(main.size.width, px(1000.) - width, "{tab} fills the rest");
    }
}

#[gpui::test]
fn the_find_count_never_moves_the_find_field(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1000.), px(500.)));
    cx.update(|window, cx| {
        desk.update(cx, |desk, cx| {
            let workspace = &mut desk.link.as_mut().unwrap().workspace;
            workspace.tabs = tabs(1);
            workspace.active_id = "fixture-a".into();
            desk.find = Some(crate::app::desk::FindState {
                placement_id: "fixture-a".into(),
                ..Default::default()
            });
            desk.find_input = Some(crate::app::overlays::find_input("", window, cx));
            cx.notify();
        })
    });
    draw(cx);
    let field = cx.debug_bounds("find-input").expect("find field");
    let slot = cx
        .debug_bounds("find-status")
        .expect("an empty count keeps its slot");
    for (query, matches, index) in [("zz", 0, 0), ("a", 12, 8), ("a", 12, 9), ("a", 4096, 4095)] {
        desk.update(cx, |desk, cx| {
            let find = desk.find.as_mut().unwrap();
            find.query = query.into();
            find.matches = vec![(0, 0); matches];
            find.index = index;
            cx.notify();
        });
        draw(cx);
        let status = desk.read_with(cx, |desk, _| desk.find_status());
        assert_eq!(
            cx.debug_bounds("find-input"),
            Some(field),
            "{status:?} moves the find field"
        );
        assert_eq!(cx.debug_bounds("find-status"), Some(slot), "{status:?}");
    }
}
