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

#[gpui::test]
fn copy_diagnostics_puts_a_redacted_identity_receipt_on_the_clipboard(cx: &mut TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        Desktop::install(cx)
    });
    let path = std::env::temp_dir().join(format!("phux-diagnostics-window-{}", std::process::id()));
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
                    startup_error: Some("Isolated window fixture".into()),
                },
                cx,
            )
            .unwrap()
    });
    cx.run_until_parked();
    cx.write_to_clipboard(gpui::ClipboardItem::new_string("terminal selection".into()));
    main.update(cx, |_, window, cx| {
        window.dispatch_action(Box::new(Run::new("copy-diagnostics")), cx);
    })
    .unwrap();
    cx.run_until_parked();
    let text = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .expect("receipt on the clipboard");
    let value: serde_json::Value = serde_json::from_str(&text).expect("receipt is JSON");
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    let mut expected = crate::diag::Receipt::FIELDS.to_vec();
    expected.sort_unstable();
    assert_eq!(keys, expected);
    assert_eq!(value["host"], "local");
    assert_eq!(value["appProtocol"], crate::updates::own_protocol());
    assert!(value["appVersion"].as_str().is_some_and(|v| !v.is_empty()));
    assert!(!text.contains("terminal selection"));
}

struct FocusFixture {
    terminal: FocusHandle,
    keys: Vec<String>,
    commands: Vec<String>,
    inputs: [Entity<InputState>; 2],
}

#[gpui::test]
fn main_menu_is_the_same_hub_with_or_without_a_sidebar(cx: &mut TestAppContext) {
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        Desktop::install(cx)
    });
    let path = std::env::temp_dir().join(format!("phux-main-hub-{}", std::process::id()));
    let mut prefs = crate::settings::default_display();
    prefs.ghostty_keys = false;
    std::fs::create_dir_all(&path).unwrap();
    let layout = crate::workspace::persist::save_layout("", &[], &prefs, None);
    std::fs::write(
        path.join("layout.json"),
        serde_json::to_vec(&crate::workspace::persist::layout_value(&layout)).unwrap(),
    )
    .unwrap();
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
                    startup_error: Some("Isolated hub fixture".into()),
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
            cx.notify();
        });
        cx.run_until_parked();
        let bounds = cx
            .debug_bounds("main-menu-rail")
            .expect("persistent hub trigger");
        assert!(bounds.origin.x >= gpui::px(78.));
        assert!(bounds.right() <= gpui::px(240.));
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.modal.clone()),
            crate::app::Modal::Goto
        );
        let settings = cx
            .debug_bounds("palette-mode-settings")
            .expect("shared settings scope");
        cx.simulate_click(settings.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.modal.clone()),
            crate::app::Modal::Settings
        );
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.modal.clone()),
            crate::app::Modal::None
        );
        assert_eq!(
            desktop.read_with(cx, |desktop, _| desktop.windows.len()),
            1,
            "navigation opens no extra native windows"
        );
    }
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
