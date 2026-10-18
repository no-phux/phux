use gpui::{Entity, Focusable, TestAppContext, VisualTestContext};
use gpui_kit::component::{Root, command::CommandState};

use super::{InputSlot, command_sections, rank_sections};
use crate::app::{AppOptions, Desk, Modal};

/// An actual desktop with no session to attach and no executable CLI. In
/// particular, a test must not construct navigation content by starting a
/// runtime driver: its real I/O cannot be driven by GPUI's deterministic clock.
pub(in crate::app) fn fixture(cx: &mut TestAppContext) -> (gpui::AnyWindowHandle, Entity<Desk>) {
    let path = std::env::temp_dir().join(format!(
        "phux-palette-test-{}-{}",
        std::process::id(),
        cx.test_function_name().unwrap_or("isolated")
    ));
    // Disable user global hotkeys before launch can register their real-time
    // polling task. Changing this after opening the window is too late.
    let mut prefs = crate::settings::default_display();
    prefs.ghostty_keys = false;
    std::fs::create_dir_all(&path).unwrap();
    let layout = crate::workspace::persist::save_layout("", &[], &prefs, None);
    std::fs::write(
        path.join("layout.json"),
        serde_json::to_vec(&crate::workspace::persist::layout_value(&layout)).unwrap(),
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
                fresh: true,
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
    cx.run_until_parked();
    (*handle, desk)
}

fn render(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn keys(cx: &mut VisualTestContext, keystrokes: &str) {
    render(cx);
    for keystroke in keystrokes.split_whitespace() {
        cx.simulate_keystrokes(keystroke);
        render(cx);
    }
}

/// Type `character` without drawing a frame after it.
fn press(cx: &mut VisualTestContext, character: char) {
    cx.update(|window, cx| {
        let key = character.to_string();
        window.dispatch_keystroke(
            gpui::Keystroke {
                modifiers: gpui::Modifiers::default(),
                key: key.clone(),
                key_char: Some(key),
            },
            cx,
        );
    });
}

fn type_text(cx: &mut VisualTestContext, text: &str) {
    render(cx);
    for character in text.chars() {
        press(cx, character);
        render(cx);
    }
}

fn state(desk: &Entity<Desk>, cx: &VisualTestContext) -> Entity<CommandState> {
    desk.read_with(cx, |desk, _| {
        desk.palette_state.clone().expect("open palette")
    })
}

fn query(desk: &Entity<Desk>, cx: &VisualTestContext) -> String {
    state(desk, cx).read_with(cx, |state, cx| state.query(cx).to_string())
}

fn selected_command(desk: &Entity<Desk>, cx: &VisualTestContext) -> Option<&'static str> {
    desk.read_with(cx, |desk, cx| {
        let state = desk.palette_state.as_ref().unwrap().read(cx);
        let selected = state.selected_index()?;
        let sections = rank_sections(command_sections(desk, cx), &state.query(cx));
        sections
            .get(selected.section)?
            .rows
            .get(selected.row)?
            .command
    })
}

fn modal(desk: &Entity<Desk>, cx: &VisualTestContext) -> Modal {
    desk.read_with(cx, |desk, _| desk.modal.clone())
}

fn sidebar(desk: &Entity<Desk>, cx: &VisualTestContext) -> bool {
    desk.read_with(cx, |desk, _| desk.prefs.sidebar_visible)
}

/// Click the element `selector` names and let the next frames land.
fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("no {selector} on screen"));
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    render(cx);
}

fn assert_desk_focus(desk: &Entity<Desk>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        assert!(
            desk.read(cx).focus.is_focused(window),
            "closed palette returns the keyboard to the desk"
        );
    });
}

fn open(desk: &Entity<Desk>, modal: &Modal, cx: &mut VisualTestContext) {
    if *modal == Modal::Path {
        desk.update(cx, |desk, _| desk.picker = Some(path_picker()));
    }
    cx.update(|window, cx| desk.update(cx, |desk, cx| desk.open_modal(modal.clone(), window, cx)));
    render(cx);
    assert_eq!(desk.read_with(cx, |desk, _| desk.modal.clone()), *modal);
}

fn path_picker() -> crate::connection::path::PickerState {
    crate::connection::path::begin_picker(
        crate::connection::path::PathTarget {
            terminal_id: "local:1".into(),
            placement_id: "place-1".into(),
            server_id: "a".into(),
            epoch: "1".into(),
        },
        "/repo",
    )
}

mod capture;
mod modals;
mod navigation;
mod search;
