//! Which keys reach a focused terminal and which stay with the window or a
//! text field.

use super::*;

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

/// Command-C and Command-V are the terminal's Copy and Paste while a
/// terminal holds the keyboard, so the Edit menu shows them, and stay a text
/// field's own while one does.
#[gpui::test]
fn clipboard_chords_belong_to_the_focused_terminal_or_text_field(cx: &mut TestAppContext) {
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
    let defaults = crate::settings::default_chords()
        .into_iter()
        .filter(|(_, id)| id == "copy" || id == "paste")
        .collect::<Vec<_>>();
    assert!(defaults.contains(&("cmd+c".into(), "copy".into())));
    assert!(defaults.contains(&("cmd+v".into(), "paste".into())));
    desktop.update(cx, |desktop, cx| {
        desktop.install_bindings(defaults, &Default::default(), false, cx);
    });
    cx.update_window(handle, |_, window, cx| {
        fixture.read(cx).terminal.clone().focus(window, cx);
    })
    .unwrap();
    for key in ["cmd-c", "cmd-v"] {
        cx.simulate_keystrokes(handle, key);
        cx.run_until_parked();
    }
    fixture.read_with(cx, |fixture, _| {
        assert_eq!(fixture.commands, ["copy", "paste"]);
        assert!(fixture.keys.is_empty(), "the chords never reach the PTY");
    });

    cx.update_window(handle, |_, window, cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("from the clipboard".into()));
        fixture.read(cx).inputs[0]
            .focus_handle(cx)
            .focus(window, cx);
    })
    .unwrap();
    // Kit binds a field's Paste to Command-V on macOS, Ctrl-V elsewhere.
    let field_paste = if cfg!(target_os = "macos") {
        "cmd-v"
    } else {
        "ctrl-v"
    };
    cx.simulate_keystrokes(handle, field_paste);
    cx.run_until_parked();
    fixture.read_with(cx, |fixture, cx| {
        assert_eq!(fixture.inputs[0].read(cx).value(), "from the clipboard");
        assert_eq!(fixture.commands, ["copy", "paste"], "the field pasted");
    });

    // Unbound, the chords fall through to the terminal surface's own
    // Command-C and Command-V instead of vanishing.
    desktop.update(cx, |desktop, cx| {
        desktop.install_bindings(Vec::new(), &Default::default(), false, cx);
    });
    cx.update_window(handle, |_, window, cx| {
        fixture.read(cx).terminal.clone().focus(window, cx);
    })
    .unwrap();
    for key in ["cmd-c", "cmd-v"] {
        cx.simulate_keystrokes(handle, key);
        cx.run_until_parked();
    }
    fixture.read_with(cx, |fixture, _| {
        assert_eq!(fixture.commands, ["copy", "paste"]);
        assert_eq!(fixture.keys, ["c", "v"]);
    });
}
