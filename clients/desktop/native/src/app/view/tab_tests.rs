use super::*;
use crate::app::{AppOptions, desk::Link};
use crate::connection::{Connection, DeterministicDial};
use crate::workspace::{Workspace, WorkspaceOptions, persist};
use gpui::{AnyWindowHandle, Entity, Modifiers, TestAppContext, VisualTestContext};
use gpui_kit::component::Root;

fn fixture(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Desk>, PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "phux-tab-layout-{}-{}",
        std::process::id(),
        cx.test_function_name().unwrap_or("isolated")
    ));
    let mut prefs = crate::settings::default_display();
    prefs.ghostty_keys = false;
    prefs.sidebar_visible = false;
    std::fs::create_dir_all(&path).unwrap();
    let layout = persist::save_layout("", &[], &prefs, None);
    std::fs::write(
        path.join("layout.json"),
        serde_json::to_vec(&persist::layout_value(&layout)).unwrap(),
    )
    .unwrap();
    let handle = cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        crate::app::open(
            AppOptions {
                socket: path.join("never-connected.sock"),
                session: String::new(),
                phux: path.join("not-installed-phux"),
                layout_path: path.join("layout.json"),
                fresh: false,
                initial_terminal: None,
                quick: false,
                dev_telemetry: false,
                startup_error: None,
            },
            cx,
        )
        .unwrap()
    });
    let desk = (*handle)
        .update(cx, |root, _, cx| {
            root.downcast::<Root>()
                .unwrap()
                .read(cx)
                .view()
                .clone()
                .downcast::<Desk>()
                .unwrap()
        })
        .unwrap();
    let connection =
        Connection::connect(path.join("never-connected.sock"), "fixture".into()).unwrap();
    desk.update(cx, |desk, cx| {
        desk.link = Some(Link {
            connection,
            workspace: Workspace::new("fixture".into(), WorkspaceOptions::default()),
        });
        cx.notify();
    });
    (*handle, desk, path)
}

fn tabs(count: usize) -> Vec<DeskTab> {
    (0..count)
        .map(|index| {
            let id = format!("fixture-{}", char::from(b'a' + index as u8));
            DeskTab {
                id: id.clone(),
                title: Some(format!("Workspace {}", index + 1)),
                root: LayoutNode::Leaf {
                    placement: Placement {
                        id: id.clone(),
                        terminal_id: None,
                        terminal_label: String::new(),
                        view_id: None,
                    },
                },
                focused_id: id,
                zoomed_id: None,
            }
        })
        .collect()
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn run(desk: &Entity<Desk>, command: &str, cx: &mut VisualTestContext) {
    cx.update(|window, cx| desk.update(cx, |desk, cx| desk.execute(command, window, cx)));
    draw(cx);
}

fn ids(desk: &Entity<Desk>, cx: &VisualTestContext) -> Vec<String> {
    desk.read_with(cx, |desk, _| {
        desk.link
            .as_ref()
            .unwrap()
            .workspace
            .tabs
            .iter()
            .map(|tab| tab.id.clone())
            .collect()
    })
}

#[gpui::test]
fn placement_switch_keeps_workspace_and_tab_actions_work_on_both_axes(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, path) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1000.), px(500.)));
    let original = tabs(3);
    for command in ["tabs-top", "tabs-side"] {
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
            assert!(!desk.prefs.sidebar_visible);
        });
        let a = cx
            .debug_bounds("tab-fixture-a")
            .expect("tab is visible without a sidebar");
        let c = cx.debug_bounds("tab-fixture-c").unwrap();
        if command == "tabs-side" {
            assert_eq!(a.origin.x, c.origin.x);
            assert!(a.origin.y < c.origin.y);
            assert!(a.origin.y >= px(TITLEBAR));
        } else {
            assert!(a.origin.x < c.origin.x);
            assert_eq!(a.origin.y, c.origin.y);
            assert!(a.bottom() <= px(TITLEBAR));
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
        let close = cx.debug_bounds("tab-close-fixture-a").unwrap();
        cx.simulate_click(close.center(), Modifiers::none());
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
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: point(px(100.), px(150.)),
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
        position: point(px(300.), px(TITLEBAR / 2.)),
        delta: gpui::ScrollDelta::Pixels(point(px(-420.), px(0.))),
        ..Default::default()
    });
    draw(cx);
    let g = cx.debug_bounds("tab-fixture-g").unwrap();
    let h = cx.debug_bounds("tab-fixture-h").unwrap();
    assert!(g.origin.x >= px(MENU_RAIL));
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
