use super::{Desktop, WindowRole};
use crate::app::actions::Run;
use gpui::prelude::*;
use gpui::{AppContext, Context, Entity, FocusHandle, Focusable, TestAppContext, Window, div};
use gpui_kit::component::input::{Input, InputState};

#[gpui::test]
fn opening_a_window_does_not_read_its_leased_desktop(cx: &mut TestAppContext) {
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        Desktop::install(cx)
    });
    let path = std::env::temp_dir().join(format!("phux-update-window-{}", std::process::id()));
    let options = crate::app::AppOptions {
        socket: path.join("isolated.sock"),
        session: String::new(),
        phux: path.join("not-installed-phux"),
        layout_path: path.join("layout.json"),
        fresh: false,
        initial_terminal: None,
        quick: false,
        dev_telemetry: false,
        startup_error: Some("Isolated window fixture".into()),
    };
    desktop.update(cx, |desktop, cx| {
        desktop.launch(options, cx).unwrap();
        assert_eq!(desktop.windows.len(), 1);
        assert_eq!(
            desktop.windows[0].desk.read(cx).updates.entity_id(),
            desktop.updates.entity_id()
        );
        assert!(desktop.updates.read(cx).development);
    });
}

#[gpui::test]
fn project_open_uses_a_durable_window_even_when_a_fresh_view_exists(cx: &mut TestAppContext) {
    // The role check does not need a server. A missing socket still makes
    // `Runtime::connect` spawn its driver, and that driver's ActivityListener
    // wakes GPUI's TestScheduler off the test thread. Hold an embedded client
    // for this fixture only.
    let _dial = crate::connection::DeterministicDial::arm();
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        Desktop::install(cx)
    });
    let path = std::env::temp_dir().join(format!("phux-project-window-{}", std::process::id()));
    let options = crate::app::AppOptions {
        socket: path.join("isolated.sock"),
        session: String::new(),
        phux: path.join("not-installed-phux"),
        layout_path: path.join("layout.json"),
        fresh: false,
        initial_terminal: None,
        quick: false,
        dev_telemetry: false,
        startup_error: Some("Isolated window fixture".into()),
    };
    desktop.update(cx, |desktop, cx| {
        desktop.launch(options, cx).unwrap();
        desktop.open_session("Cedar".into(), None, cx);
        assert_eq!(desktop.windows[1].role, WindowRole::Fresh);
        desktop
            .open_project("Cedar".into(), phux_protocol::ResourceId::local(1), cx)
            .unwrap();
        assert_eq!(desktop.windows.len(), 3);
        assert_eq!(desktop.windows[2].role, WindowRole::Main);
        assert_eq!(desktop.windows[2].session, "Cedar");
    });
}

#[gpui::test]
fn performance_monitor_starts_stops_and_reopens_live_without_a_launch_flag(
    cx: &mut TestAppContext,
) {
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        Desktop::install(cx)
    });
    let path = std::env::temp_dir().join(format!("phux-performance-window-{}", std::process::id()));
    let mut prefs = crate::settings::default_display();
    prefs.ghostty_keys = false;
    std::fs::create_dir_all(&path).unwrap();
    let layout = crate::workspace::persist::save_layout("", &[], &prefs, None);
    std::fs::write(
        path.join("layout.json"),
        serde_json::to_vec(&crate::workspace::persist::layout_value(&layout)).unwrap(),
    )
    .unwrap();
    let main = desktop.update(cx, |desktop, cx| {
        desktop
            .launch(
                crate::app::AppOptions {
                    socket: path.join("isolated.sock"),
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
    cx.run_until_parked();
    let run = |id: &str, cx: &mut TestAppContext| {
        main.update(cx, |_, window, cx| {
            window.activate_window();
            window.dispatch_action(Box::new(Run::new(id)), cx);
        })
        .unwrap();
        cx.run_until_parked();
    };
    assert!(!desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
    run("performance-monitor-stop", cx);
    assert!(!desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
    run("performance-monitor", cx);
    assert!(desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
    let first = desktop.read_with(cx, |desktop, _| desktop.performance.unwrap());
    run("performance-monitor", cx);
    assert_eq!(
        desktop.read_with(cx, |desktop, _| desktop.performance.unwrap().window_id()),
        first.window_id(),
    );
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    cx.run_until_parked();
    run("performance-monitor-stop", cx);
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(2));
    cx.run_until_parked();
    assert!(!desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
    assert!(
        cx.update_window(first, |_, _, _| ()).is_err(),
        "stop closes the sampler's owning window"
    );
    run("performance-monitor", cx);
    let reopened = desktop.read_with(cx, |desktop, _| desktop.performance.unwrap());
    assert_ne!(reopened.window_id(), first.window_id());
    cx.update_window(reopened, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert!(!desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
    run("performance-monitor", cx);
    assert!(desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
    run("performance-monitor-stop", cx);
    assert!(!desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
}

struct FocusFixture {
    terminal: FocusHandle,
    keys: Vec<String>,
    commands: Vec<String>,
    inputs: [Entity<InputState>; 2],
    menu_desk: Option<Entity<crate::app::Desk>>,
}

#[gpui::test]
fn main_menu_stays_left_with_a_hidden_sidebar_and_opens_settings(cx: &mut TestAppContext) {
    // The runtime-backed fixture may wake outside the test scheduler.
    cx.executor().allow_parking();
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        Desktop::install(cx)
    });
    let path = std::env::temp_dir().join(format!("phux-main-menu-{}", std::process::id()));
    let handle = desktop.update(cx, |desktop, cx| {
        desktop
            .launch(
                crate::app::AppOptions {
                    socket: path.join("isolated.sock"),
                    session: String::new(),
                    phux: path.join("not-installed-phux"),
                    layout_path: path.join("layout.json"),
                    fresh: false,
                    initial_terminal: None,
                    quick: false,
                    startup_error: Some("Isolated menu fixture".into()),
                    dev_telemetry: false,
                },
                cx,
            )
            .unwrap()
    });
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    let cx = &mut gpui::VisualTestContext::from_window(*handle, cx);
    for visible in [true, false] {
        desk.update(cx, |desk, cx| {
            desk.prefs.sidebar_visible = visible;
            desk.remote = Some("a-host-name-longer-than-the-sidebar".into());
            cx.notify();
        });
        cx.run_until_parked();
        let bounds = cx
            .debug_bounds("main-menu-rail")
            .expect("persistent main menu");
        assert!(bounds.origin.x >= gpui::px(78.), "clears traffic lights");
        assert!(
            bounds.right() <= gpui::px(240.),
            "menu stays in the left rail"
        );
        assert!(bounds.size.width > gpui::px(40.));
    }
    let bounds = cx.debug_bounds("main-menu-rail").unwrap();
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    let menu = desk.read_with(cx, |desk, _| desk.main_menu.unwrap());
    let menu_cx = &mut gpui::VisualTestContext::from_window(menu.into(), cx);
    menu_cx.run_until_parked();
    let settings = menu_cx
        .debug_bounds("glass-menu-settings")
        .expect("Settings command");
    menu_cx.simulate_click(settings.center(), gpui::Modifiers::none());
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.modal.clone()),
        crate::app::Modal::Settings
    );
    check_menu_focus(desk, cx);
}

fn check_menu_focus(desk: Entity<crate::app::Desk>, cx: &mut gpui::VisualTestContext) {
    desk.update(cx, |desk, window_cx| {
        desk.modal = crate::app::Modal::None;
        window_cx.notify();
    });
    // Mount the real main menu alongside terminal/find-like focus targets.
    // This checks popup cancellation without needing a daemon for key routing.
    let (focus_window, fixture) = cx.update(|_, cx| {
        gpui_kit::open_window(Default::default(), cx, |window, cx| {
            cx.new(|cx| FocusFixture {
                terminal: cx.focus_handle(),
                keys: Vec::new(),
                commands: Vec::new(),
                inputs: std::array::from_fn(|_| cx.new(|cx| InputState::new(window, cx))),
                menu_desk: Some(desk.clone()),
            })
        })
        .unwrap()
    });
    let cx = &mut gpui::VisualTestContext::from_window(focus_window, cx);
    for (input, escape, submenu) in [
        (false, true, None),
        (true, true, None),
        (false, false, None),
        (true, false, None),
        (false, true, Some("panes")),
        (true, true, Some("hosts")),
    ] {
        cx.update(|window, cx| {
            fixture.read(cx).terminal.clone().focus(window, cx);
            if input {
                fixture.read(cx).inputs[0]
                    .focus_handle(cx)
                    .focus(window, cx);
            }
        });
        cx.run_until_parked();
        let bounds = cx.debug_bounds("main-menu-rail").unwrap();
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        let menu = desk.read_with(cx, |desk, _| {
            desk.main_menu.expect("menu reopened for focus journey")
        });
        {
            let menu_cx = &mut gpui::VisualTestContext::from_window(menu.into(), cx);
            menu_cx.update(|window, cx| {
                window.activate_window();
                assert!(window.focused(cx).is_some(), "popup receives its own focus");
            });
            menu_cx.run_until_parked();
            if let Some(page) = submenu {
                if page == "panes" {
                    let pane = menu_cx
                        .debug_bounds("glass-menu-panes")
                        .expect("Pane layout");
                    menu_cx.simulate_mouse_move(pane.center(), None, gpui::Modifiers::none());
                }
                menu_cx.simulate_keystrokes("right");
                let first = if page == "panes" {
                    "glass-menu-split-right"
                } else {
                    "glass-menu-local"
                };
                assert!(menu_cx.debug_bounds(first).is_some());
                assert!(
                    menu_cx.debug_bounds("glass-menu-settings").is_none(),
                    "child page replaced command rows"
                );
            }
            if escape {
                menu_cx.simulate_keystrokes("escape");
            } else {
                // Native outside clicks arrive through window resignation.
                menu_cx.deactivate_window();
            }
        }
        cx.update(|window, cx| {
            let target = if input {
                fixture.read(cx).inputs[0].focus_handle(cx)
            } else {
                fixture.read(cx).terminal.clone()
            };
            assert!(
                target.is_focused(window),
                "menu cancellation restores the original input target"
            );
        });
        cx.simulate_input("x");
    }
    fixture.read_with(cx, |fixture, cx| {
        assert_eq!(fixture.inputs[0].read(cx).value(), "xxx");
        assert!(
            !fixture.keys.is_empty(),
            "terminal still receives keys after Escape"
        );
    });
    let bounds = cx.debug_bounds("main-menu-rail").unwrap();
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    let moved_menu = desk.read_with(cx, |desk, _| desk.main_menu.unwrap());
    cx.simulate_resize(gpui::size(gpui::px(900.), gpui::px(600.)));
    assert!(
        moved_menu.read_with(cx, |_, _| ()).is_err(),
        "parent geometry changes dismiss the panel"
    );
    let bounds = cx.debug_bounds("main-menu-rail").unwrap();
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    let closing_menu = desk.read_with(cx, |desk, _| desk.main_menu.unwrap());
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
    assert!(
        closing_menu.read_with(cx, |_, _| ()).is_err(),
        "closing a desk leaves no orphan menu"
    );
}

impl gpui::Render for FocusFixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("Desk")
            .on_action(cx.listener(|this, action: &Run, _, _| {
                this.commands.push(action.id.clone());
            }))
            .size_full()
            .flex()
            .flex_col()
            .when_some(self.menu_desk.clone(), |root, desk| {
                root.child(
                    div()
                        .h(gpui::px(32.))
                        .w(gpui::px(192.))
                        .flex()
                        .child(desk.update(cx, |desk, cx| super::super::view::main_menu(desk, cx))),
                )
            })
            .child(
                div()
                    .id("terminal")
                    .track_focus(&self.terminal)
                    .key_context("PhuxTerminal")
                    .child("Terminal")
                    .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                        this.keys.push(event.keystroke.key.clone());
                        cx.stop_propagation();
                    })),
            )
            .child(Input::new(&self.inputs[0]).id("first"))
            .child(Input::new(&self.inputs[1]).id("second"))
    }
}

#[gpui::test]
fn tab_keeps_terminal_input_but_navigates_text_fields(cx: &mut TestAppContext) {
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        Desktop::install(cx)
    });
    let (handle, fixture) = cx.update(|cx| {
        gpui_kit::open_window(Default::default(), cx, |window, cx| {
            cx.new(|cx| FocusFixture {
                terminal: cx.focus_handle(),
                keys: Vec::new(),
                commands: Vec::new(),
                inputs: std::array::from_fn(|_| cx.new(|cx| InputState::new(window, cx))),
                menu_desk: None,
            })
        })
        .unwrap()
    });
    cx.update_window(handle, |_, window, cx| {
        fixture.read(cx).terminal.clone().focus(window, cx);
    })
    .unwrap();
    for key in ["tab", "shift-tab"] {
        cx.simulate_keystrokes(handle, key);
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            assert!(fixture.read(cx).terminal.is_focused(window));
        })
        .unwrap();
    }
    assert_eq!(
        fixture.read_with(cx, |fixture, _| fixture.keys.clone()),
        ["tab", "tab"]
    );
    cx.update_window(handle, |_, window, cx| {
        fixture.read(cx).inputs[0]
            .focus_handle(cx)
            .focus(window, cx);
    })
    .unwrap();
    for (key, destination) in [("tab", 1), ("shift-tab", 0)] {
        cx.simulate_keystrokes(handle, key);
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            assert!(
                fixture.read(cx).inputs[destination]
                    .focus_handle(cx)
                    .is_focused(window)
            );
        })
        .unwrap();
        cx.simulate_input(handle, "x");
        cx.run_until_parked();
    }
    fixture.read_with(cx, |fixture, cx| {
        for input in &fixture.inputs {
            assert_eq!(input.read(cx).value(), "x");
        }
    });

    // Deliberate user shortcuts win; removing them restores terminal input.
    desktop.update(cx, |desktop, cx| {
        desktop.install_bindings(
            vec![
                ("tab".into(), "next_tab".into()),
                ("shift+tab".into(), "previous_tab".into()),
            ],
            &Default::default(),
            false,
            cx,
        );
    });
    cx.update_window(handle, |_, window, cx| {
        fixture.read(cx).terminal.clone().focus(window, cx);
    })
    .unwrap();
    for key in ["tab", "shift-tab"] {
        cx.simulate_keystrokes(handle, key);
        cx.run_until_parked();
    }
    fixture.read_with(cx, |fixture, _| {
        assert_eq!(fixture.commands, ["next_tab", "previous_tab"]);
        assert_eq!(fixture.keys, ["tab", "tab"]);
    });
    desktop.update(cx, |desktop, cx| {
        desktop.install_bindings(Vec::new(), &Default::default(), false, cx);
    });
    for key in ["tab", "shift-tab"] {
        cx.simulate_keystrokes(handle, key);
        cx.run_until_parked();
    }
    fixture.read_with(cx, |fixture, _| {
        assert_eq!(fixture.commands, ["next_tab", "previous_tab"]);
        assert_eq!(fixture.keys, ["tab", "tab", "tab", "tab"]);
    });
}
