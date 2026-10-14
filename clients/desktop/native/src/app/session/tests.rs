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
        startup_error: Some("Isolated window fixture".into()),
    };
    desktop.update(cx, |desktop, cx| {
        desktop.launch(options, cx).unwrap();
        desktop.open_session("Cedar".into(), cx);
        assert_eq!(desktop.windows[1].role, WindowRole::Fresh);
        desktop
            .open_project("Cedar".into(), phux_protocol::ResourceId::local(1), cx)
            .unwrap();
        assert_eq!(desktop.windows.len(), 3);
        assert_eq!(desktop.windows[2].role, WindowRole::Main);
        assert_eq!(desktop.windows[2].session, "Cedar");
    });
}

struct FocusFixture {
    terminal: FocusHandle,
    keys: Vec<String>,
    commands: Vec<String>,
    inputs: [Entity<InputState>; 2],
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
