use super::{Desktop, WindowRole};
use crate::app::actions::{PerformanceMonitor, Run, StopPerformanceMonitor};
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
    let main = cx
        .add_empty_window()
        .update(|window, _| window.window_handle());
    assert!(desktop.read_with(cx, |desktop, _| desktop.performance.is_none()));
    cx.update(|cx| cx.dispatch_action(&PerformanceMonitor));
    cx.run_until_parked();
    let first = desktop.read_with(cx, |desktop, _| desktop.performance.unwrap());
    cx.update(|cx| cx.dispatch_action(&PerformanceMonitor));
    cx.run_until_parked();
    assert_eq!(
        desktop.read_with(cx, |desktop, _| desktop.performance.unwrap().window_id()),
        first.window_id(),
    );
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    cx.run_until_parked();
    cx.update(|cx| cx.dispatch_action(&StopPerformanceMonitor));
    cx.run_until_parked();
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(2));
    cx.run_until_parked();
    assert!(desktop.read_with(cx, |desktop, _| desktop.performance.is_none()));
    assert!(
        cx.update_window(first, |_, _, _| ()).is_err(),
        "stop closes the sampler's owning window"
    );
    // The test platform does not transfer native focus when a window closes.
    main.update(cx, |_, window, _| window.activate_window())
        .unwrap();
    cx.update(|cx| cx.dispatch_action(&PerformanceMonitor));
    cx.run_until_parked();
    let reopened = desktop.read_with(cx, |desktop, _| desktop.performance.unwrap());
    assert_ne!(reopened.window_id(), first.window_id());
    cx.update_window(reopened, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert!(desktop.read_with(cx, |desktop, _| desktop.performance.is_none()));
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
    // Kit's popup uses a platform timer; allow its real wake just as the
    // runtime-backed window journeys do.
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
    // Ten command entries, ignoring separators: Settings is the last one.
    cx.simulate_keystrokes("down down down down down down down down down down enter");
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
    for (input, escape) in [(false, true), (true, true), (false, false), (true, false)] {
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
        cx.update(|window, cx| {
            let target = if input {
                fixture.read(cx).inputs[0].focus_handle(cx)
            } else {
                fixture.read(cx).terminal.clone()
            };
            assert!(
                !target.is_focused(window),
                "popup actually took focus before dismissal"
            );
        });
        if escape {
            cx.simulate_keystrokes("escape");
        } else {
            cx.simulate_click(
                gpui::point(gpui::px(700.), gpui::px(500.)),
                gpui::Modifiers::none(),
            );
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
        assert_eq!(fixture.inputs[0].read(cx).value(), "xx");
        assert!(
            !fixture.keys.is_empty(),
            "terminal still receives keys after Escape"
        );
    });
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
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
