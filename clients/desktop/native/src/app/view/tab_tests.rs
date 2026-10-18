use std::path::PathBuf;

use super::tabs::tab_height;
use super::titlebar::{MENU_RAIL, controls_fit};
use super::*;
use crate::app::desk::Modal;
use crate::app::ui::list::row_radius;
use crate::app::{AppOptions, desk::Link};
use crate::connection::{Connection, DeterministicDial};
use crate::workspace::layout::{DeskTab, LayoutNode, Placement};
use crate::workspace::{Workspace, WorkspaceOptions, persist};
use gpui::{AnyWindowHandle, Entity, Modifiers, TestAppContext, VisualTestContext};
use gpui::{MouseButton, point};
use gpui_kit::component::Root;

/// The default skin's titlebar, which these layouts paint.
const TITLEBAR: f32 = crate::settings::skin::GLASS.chrome.titlebar;

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
        cx.update(|window, cx| {
            window.bounds_changed(cx);
            window.draw(cx).clear(cx);
        });
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

fn resource_key(byte: u8) -> phux_protocol::ids::ResourceKey {
    phux_protocol::ids::ResourceKey::new([byte; 16]).unwrap()
}

/// Three tab-backed terminals and a fourth that is only in the navigator.
fn selectable_work(desk: &mut Desk, cx: &mut gpui::Context<Desk>) {
    use phux_client_runtime::control::{PaneDescriptor, SessionDescriptor, Status, Topology};
    use phux_protocol::ResourceId;

    desk.session = "fixture".into();
    let link = desk.link.as_mut().unwrap();
    link.workspace.tabs = tabs(3);
    link.workspace.active_id = "fixture-c".into();
    for (index, tab) in link.workspace.tabs.iter_mut().enumerate() {
        let LayoutNode::Leaf { placement } = &mut tab.root else {
            unreachable!()
        };
        placement.terminal_id = Some(ResourceId::local(index as u32 + 1));
    }
    link.connection.status = Status::Attached;
    link.connection.topology = Some(Topology {
        sessions: vec![SessionDescriptor {
            id: 1,
            name: "fixture".into(),
            attached_client_count: 1,
            keep_empty: true,
            created_at_unix_secs: 0,
            key: None,
            lifetime: None,
            revision: None,
            members: 4,
            arrangement: None,
            awaiting_organization: false,
        }],
        panes: (1..=4)
            .map(|index| PaneDescriptor {
                terminal_id: ResourceId::local(index),
                session_id: 1,
                session_name: "fixture".into(),
                cols: 80,
                rows: 24,
                title: None,
                cwd: None,
                is_focused: index == 3,
                key: Some(resource_key(index as u8)),
                revision: None,
                name: None,
                activation: None,
                exec: None,
            })
            .collect(),
        agent_sessions: Vec::new(),
        focused_session: 1,
        focused_pane: ResourceId::local(3),
        authority: None,
        inbox: None,
        handles: Default::default(),
    });
    desk.organization.selected.clear();
    cx.notify();
}

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
    for command in ["tabs-top", "tabs-side"] {
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
    cx.simulate_event(gpui::MouseUpEvent {
        position: a,
        modifiers: modifier,
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
        // Far enough that the seventh tab clears the main menu rail.
        position: point(px(500.), px(TITLEBAR / 2.)),
        delta: gpui::ScrollDelta::Pixels(point(px(-560.), px(0.))),
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
fn sidebar_tabs_live_under_their_session_and_return_to_the_titlebar_when_it_hides(
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
    let session = cx
        .debug_bounds("session-current")
        .expect("this window's session heads its tabs");
    let tab = cx
        .debug_bounds("tab-fixture-a")
        .expect("tab in the sidebar");
    assert!(tab.right() <= px(width), "the tab is inside the sidebar");
    assert!(tab.origin.y >= session.bottom(), "under its session");
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
