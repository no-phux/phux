use gpui::{Entity, Focusable, TestAppContext, VisualTestContext};
use gpui_kit::component::{Root, command::CommandState};

use super::{InputSlot, command_sections, rank_sections};
use crate::app::{AppOptions, Desk, Modal};

/// An actual desktop with no session to attach and no executable CLI. In
/// particular, a test must not construct navigation content by starting a
/// runtime driver: its real I/O cannot be driven by GPUI's deterministic clock.
fn fixture(cx: &mut TestAppContext) -> (gpui::AnyWindowHandle, Entity<Desk>) {
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

#[gpui::test]
fn empty_actions_show_only_essentials_and_last_row_fits(cx: &mut TestAppContext) {
    let (handle, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    keys(cx, "cmd-shift-p");
    assert!(cx.debug_bounds("palette-result-settings").is_none());
    let last = cx
        .debug_bounds("palette-result-find")
        .expect("last essential action");
    let panel = cx.debug_bounds("palette-panel").unwrap();
    assert!(
        last.bottom() <= panel.bottom(),
        "last action stays inside the panel"
    );
}

#[gpui::test]
fn work_search_discovers_a_preference_and_settings_keep_one_native_hub(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    keys(cx, "cmd-p");
    type_text(cx, "line height");
    keys(cx, "enter");
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.settings_page),
        Some(crate::app::settings_panel::Page::Spacing)
    );
    assert!(cx.debug_bounds("settings-panel").is_some());
    click(cx, "palette-mode-commands");
    assert_eq!(modal(&desk, cx), Modal::Commands);
    keys(cx, "escape");
    assert_desk_focus(&desk, cx);
    keys(cx, "cmd-,");
    type_text(cx, "remote");
    keys(cx, "enter");
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.settings_page),
        Some(crate::app::settings_panel::Page::Connections)
    );
    keys(cx, "escape");
    assert_desk_focus(&desk, cx);
}

#[gpui::test]
fn palette_query_ranks_a_title_before_incidental_metadata_and_dispatches_it(
    cx: &mut TestAppContext,
) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    keys(cx, "cmd-shift-p");
    type_text(cx, "settings");
    assert_eq!(query(&desk, cx), "settings");
    assert_eq!(selected_command(&desk, cx), Some("settings"));
    keys(cx, "enter");
    assert_eq!(modal(&desk, cx), Modal::Settings);
    keys(cx, "escape");
    assert_desk_focus(&desk, cx);
}

#[gpui::test]
fn palette_mode_controls_row_click_and_backdrop_keep_native_focus(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    keys(cx, "cmd-shift-p");
    click(cx, "palette-mode-goto");
    assert_eq!(modal(&desk, cx), Modal::Goto);
    type_text(cx, ">settings");
    assert_eq!(query(&desk, cx), "settings");
    click(cx, "palette-mode-commands");
    assert_eq!(modal(&desk, cx), Modal::Commands);
    click(cx, "palette-result-settings");
    assert_eq!(modal(&desk, cx), Modal::Settings);
    keys(cx, "escape cmd-shift-p");
    cx.simulate_click(
        gpui::point(gpui::px(8.), gpui::px(8.)),
        gpui::Modifiers::none(),
    );
    render(cx);
    assert_eq!(modal(&desk, cx), Modal::None);
    assert_desk_focus(&desk, cx);
}

#[gpui::test]
fn typing_and_confirming_before_the_next_frame_uses_the_current_query(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    let before = sidebar(&desk, cx);
    keys(cx, "cmd-shift-p");
    for character in "sidebar".chars() {
        press(cx, character);
    }
    cx.update(|window, cx| {
        window.dispatch_keystroke(gpui::Keystroke::parse("enter").unwrap(), cx);
    });
    render(cx);
    assert_eq!(sidebar(&desk, cx), !before);
    assert_desk_focus(&desk, cx);
}

#[gpui::test]
fn palette_arrow_navigation_confirms_the_highlighted_command(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, |desk, _| desk.prefs.font_size = 18.);
    keys(cx, "cmd-shift-p");
    type_text(cx, "font size");
    let initial = state(&desk, cx).read_with(cx, |state, _| state.selected_index());
    assert!(initial.is_some());
    keys(cx, "down");
    let next = state(&desk, cx).read_with(cx, |state, _| state.selected_index());
    assert_ne!(initial, next, "Down must move a real selection");
    keys(cx, "up");
    assert_eq!(
        state(&desk, cx).read_with(cx, |state, _| state.selected_index()),
        initial
    );
    keys(cx, "down");
    let expected = match selected_command(&desk, cx).expect("highlighted font command") {
        "font-up" => 19.,
        "font-down" => 17.,
        "font-reset" => crate::settings::default_display().font_size,
        other => panic!("font-size query highlighted unrelated command {other}"),
    };
    keys(cx, "enter");
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.prefs.font_size, expected);
        assert_eq!(desk.modal, Modal::None);
    });
    assert_desk_focus(&desk, cx);
}

#[gpui::test]
fn palette_confirm_transfers_typing_to_the_command_dialog(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    keys(cx, "cmd-shift-p");
    type_text(cx, "new-session");
    assert_eq!(selected_command(&desk, cx), Some("new-session"));
    let palette = state(&desk, cx);
    keys(cx, "enter");
    assert!(desk.read_with(cx, |desk, _| matches!(
        desk.modal,
        Modal::Session { create: true, .. }
    )));
    type_text(cx, "Cedar");
    desk.read_with(cx, |desk, cx| {
        let input = desk.input_entity(InputSlot::SessionName).unwrap();
        assert_eq!(input.read(cx).value(), "Cedar");
        assert_eq!(
            palette.read(cx).query(cx),
            "new-session",
            "typing no longer changes the palette"
        );
    });
    cx.update(|window, cx| {
        let input = desk.read(cx).input_entity(InputSlot::SessionName).unwrap();
        assert!(input.focus_handle(cx).is_focused(window));
    });
    keys(cx, "escape");
    assert_eq!(modal(&desk, cx), Modal::None);
    assert_desk_focus(&desk, cx);
}

#[gpui::test]
fn palette_escape_clears_query_before_dismissing_and_can_reopen_cleanly(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    keys(cx, "cmd-shift-p");
    type_text(cx, "sidebar");
    let before = sidebar(&desk, cx);
    keys(cx, "escape");
    assert_eq!(query(&desk, cx), "");
    assert_eq!(modal(&desk, cx), Modal::Commands);
    cx.update(|window, cx| {
        assert!(
            !desk.read(cx).focus.is_focused(window),
            "clearing keeps typing in the palette"
        );
    });
    keys(cx, "escape");
    assert_eq!(modal(&desk, cx), Modal::None);
    assert_desk_focus(&desk, cx);
    keys(cx, "cmd-shift-p");
    assert_eq!(query(&desk, cx), "");
    type_text(cx, "sidebar");
    keys(cx, "enter");
    assert_eq!(sidebar(&desk, cx), !before);
    assert_desk_focus(&desk, cx);
}

#[gpui::test]
fn goto_leading_marker_hands_the_remaining_query_and_focus_to_commands(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    let before = sidebar(&desk, cx);
    keys(cx, "cmd-p");
    assert_eq!(modal(&desk, cx), Modal::Goto);
    // Pasting delivers the marker and remainder in one real Input edit; typing
    // a bare `>` followed by characters would not prove query preservation.
    cx.update(|_, cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("> sidebar".into()));
    });
    keys(cx, "cmd-v");
    assert_eq!(modal(&desk, cx), Modal::Commands);
    assert_eq!(query(&desk, cx), "sidebar");
    assert_eq!(selected_command(&desk, cx), Some("sidebar"));
    keys(cx, "enter");
    assert_eq!(sidebar(&desk, cx), !before);
    assert_desk_focus(&desk, cx);
}

#[gpui::test]
fn empty_palette_results_never_dispatch_the_previous_selection(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    let before = sidebar(&desk, cx);
    keys(cx, "cmd-shift-p");
    type_text(cx, "sidebar");
    assert_eq!(selected_command(&desk, cx), Some("sidebar"));
    type_text(cx, " zqxjv-no-command-987654321");
    assert_eq!(
        state(&desk, cx).read_with(cx, |state, _| state.selected_index()),
        None
    );
    keys(cx, "down up enter");
    desk.read_with(cx, |desk, _| {
        assert_eq!(
            desk.prefs.sidebar_visible, before,
            "Enter must not run the stale sidebar action"
        );
        assert_eq!(
            desk.modal,
            Modal::Commands,
            "empty confirmation keeps the search open"
        );
    });
    // The same control recovers once there are results again.
    keys(cx, "escape");
    type_text(cx, "sidebar");
    keys(cx, "enter");
    assert_eq!(sidebar(&desk, cx), !before);
    assert_desk_focus(&desk, cx);
}

#[gpui::test]
fn performance_utility_opens_stops_and_reopens_from_native_palette(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let desktop = desk.read_with(cx, |desk, _| desk.desktop.clone());
    let cx = &mut VisualTestContext::from_window(handle, cx);
    assert!(!desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
    keys(cx, "cmd-shift-p");
    type_text(cx, "performance-monitor-stop");
    desk.read_with(cx, |desk, cx| {
        assert!(
            command_sections(desk, cx)
                .iter()
                .flat_map(|section| &section.rows)
                .all(|row| row.command != Some("performance-monitor-stop")),
            "an inactive monitor must not offer a stop action"
        );
    });
    keys(cx, "escape");
    type_text(cx, "cpu");
    assert_eq!(selected_command(&desk, cx), Some("performance-monitor"));
    keys(cx, "enter");
    assert!(desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
    // TestPlatform does not transfer native focus between utility windows.
    cx.update(|window, cx| {
        window.activate_window();
        desk.read(cx).focus.clone().focus(window, cx);
    });
    keys(cx, "cmd-shift-p");
    type_text(cx, "performance-monitor-stop");
    assert_eq!(
        selected_command(&desk, cx),
        Some("performance-monitor-stop")
    );
    keys(cx, "enter");
    assert!(!desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
    assert_desk_focus(&desk, cx);
    keys(cx, "cmd-shift-p");
    type_text(cx, "memory");
    keys(cx, "enter");
    assert!(desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
    // Exercise the stop command again rather than leaving a sampler alive.
    cx.update(|window, cx| {
        window.activate_window();
        desk.read(cx).focus.clone().focus(window, cx);
    });
    keys(cx, "cmd-shift-p");
    type_text(cx, "performance-monitor-stop");
    keys(cx, "enter");
    assert!(!desktop.read_with(cx, |desktop, _| desktop.performance_is_open()));
}

#[gpui::test]
fn pending_path_search_does_not_leave_command_mode_loading(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    keys(cx, "cmd-shift-p");
    let palette = state(&desk, cx);
    cx.update(|window, cx| {
        desk.update(cx, |desk, cx| desk.open_modal(Modal::Path, window, cx));
        palette.update(cx, |state, cx| state.set_loading(true, window, cx));
        desk.update(cx, |desk, cx| desk.execute("palette", window, cx));
    });
    render(cx);
    assert_eq!(modal(&desk, cx), Modal::Commands);
    assert!(
        !palette.read_with(cx, |state, _| state.is_loading()),
        "the new mode must not retain an abandoned path request's spinner"
    );
    type_text(cx, "zqxjv-no-command-987654321");
    assert_eq!(palette.read_with(cx, |state, _| state.matched_count()), 0);
}

#[gpui::test]
fn command_palette_starts_small_but_search_and_expansion_keep_every_command(
    cx: &mut TestAppContext,
) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    keys(cx, "cmd-shift-p");
    let count = |desk: &Entity<Desk>, cx: &VisualTestContext| {
        desk.read_with(cx, |desk, cx| {
            super::command_results(desk, cx, "")
                .iter()
                .map(|section| section.rows.len())
                .sum::<usize>()
        })
    };
    assert_eq!(count(&desk, cx), 5);
    assert!(cx.debug_bounds("palette-result-settings").is_none());
    click(cx, "palette-all-commands");
    assert!(count(&desk, cx) > 20);
    type_text(cx, "settings");
    assert_eq!(selected_command(&desk, cx), Some("settings"));
    keys(cx, "escape escape cmd-shift-p");
    assert_eq!(count(&desk, cx), 5, "disclosure resets on reopen");
    type_text(cx, "performance-monitor");
    assert_eq!(selected_command(&desk, cx), Some("performance-monitor"));
}

#[gpui::test]
fn navigation_details_are_explicit_and_preserve_search_focus(cx: &mut TestAppContext) {
    use super::{Row, Section, Target};
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(gpui::px(320.), gpui::px(280.)));
    keys(cx, "cmd-p");
    desk.update(cx, |desk, cx| {
        let revision = super::search_revision(desk);
        desk.goto_search.rebuild(
            revision,
            vec![Section {
                label: None,
                rows: vec![Row {
                    title: "Worker".into(),
                    detail: Some("~/project".into()),
                    keywords: "project".into(),
                    target: Target::Pane(phux_protocol::ResourceId::local(1)),
                    command: None,
                    category: "Terminals",
                    chord: None,
                    agent: None,
                    preview: Some(super::goto_facts::FactsCard {
                        title: "Worker".into(),
                        facts: vec![super::goto_facts::Fact::new("Directory", "~/project")],
                    }),
                }],
            }],
        );
        cx.notify();
    });
    render(cx);
    assert!(cx.debug_bounds("palette-facts").is_none());
    let panel = cx.debug_bounds("palette-panel").unwrap();
    let modes = cx.debug_bounds("palette-mode-settings").unwrap();
    let filter = cx.debug_bounds("palette-filter").unwrap();
    let details_bounds = cx.debug_bounds("palette-details").unwrap();
    for bounds in [modes, filter, details_bounds] {
        assert!(
            bounds.left() >= panel.left() && bounds.right() <= panel.right(),
            "header controls stay inside the panel"
        );
    }
    assert!(
        modes.bottom() <= filter.top(),
        "compact controls occupy their own row"
    );
    assert!(
        filter.right() <= details_bounds.left(),
        "filter and details do not overlap"
    );
    click(cx, "palette-details");
    assert!(cx.debug_bounds("palette-facts").is_some());
    let panel = cx.debug_bounds("palette-panel").unwrap();
    assert!(panel.origin.x >= gpui::px(0.) && panel.right() <= gpui::px(320.));
    assert!(
        panel.bottom() <= gpui::px(280.),
        "details stay on a small screen"
    );
    type_text(cx, "project");
    assert_eq!(
        query(&desk, cx),
        "project",
        "details keep the search keyboard"
    );
    keys(cx, "escape escape cmd-p");
    assert!(!desk.read_with(cx, |desk, _| desk.palette_view.details));
}

#[gpui::test]
fn a_remapped_chord_is_the_hint_the_palette_shows(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, |desk, cx| {
        desk.install_ghostty_for_test("keybind = cmd+alt+z=toggle_split_zoom\n", cx)
    });
    render(cx);
    let remapped = crate::settings::display_chord("cmd+alt+z");
    let default = crate::settings::display_chord("cmd+shift+enter");
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.shown_chord("zoom")),
        Some(remapped.clone())
    );
    keys(cx, "cmd-shift-p");
    type_text(cx, "zoom pane");
    assert_eq!(selected_command(&desk, cx), Some("zoom"));
    // Debug selectors are looked up by `'static` name.
    let selector = |chord: &str| -> &'static str {
        Box::leak(format!("palette-chord:{chord}").into_boxed_str())
    };
    assert!(
        cx.debug_bounds(selector(&remapped)).is_some(),
        "the highlighted row shows the installed chord"
    );
    assert!(
        cx.debug_bounds(selector(&default)).is_none(),
        "the built-in default is not shown once remapped"
    );
}

#[gpui::test]
fn destination_pickers_drop_the_global_strip_and_keep_every_route(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    for modal in [Modal::WindowSession, Modal::MoveResources] {
        open(&desk, &modal, cx);
        assert!(cx.debug_bounds("palette-panel").is_some());
        for global in [
            "palette-mode-goto",
            "palette-mode-commands",
            "palette-mode-settings",
            "palette-filter",
        ] {
            assert!(
                cx.debug_bounds(global).is_none(),
                "{modal:?} is scoped to its destination, not the {global} hub control"
            );
        }
        keys(cx, "escape");
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.modal.clone()),
            Modal::None
        );
    }
    // The navigator still reaches every route.
    keys(cx, "cmd-p");
    for route in [
        "palette-mode-goto",
        "palette-mode-commands",
        "palette-mode-settings",
        "palette-filter",
    ] {
        assert!(cx.debug_bounds(route).is_some(), "{route} stays reachable");
    }
}

/// The element that carries `modal`'s card.
fn surface(modal: &Modal) -> &'static str {
    match modal {
        Modal::NativeLaunch => "native-launch-panel",
        Modal::Commands
        | Modal::Goto
        | Modal::WindowSession
        | Modal::Windows
        | Modal::MoveResources
        | Modal::Settings => "palette-panel",
        _ => "modal-sheet",
    }
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

#[gpui::test]
fn every_modal_shares_one_placement_and_dismissal(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    let modals = [
        Modal::Commands,
        Modal::Rename { tab_id: "t".into() },
        Modal::Session {
            create: true,
            current: String::new(),
            target: None,
        },
        Modal::Session {
            create: false,
            current: "work".into(),
            target: None,
        },
        Modal::AddHost,
        Modal::RemoveHost {
            name: "mini".into(),
        },
        Modal::KillSession {
            name: "work".into(),
            session: 1,
        },
        Modal::Notifications,
        Modal::NativeLaunch,
        Modal::Path,
        Modal::Goto,
        Modal::WindowSession,
        Modal::Windows,
        Modal::MoveResources,
        Modal::Terminate {
            terminal: phux_protocol::ResourceId::local(1),
            title: "shell".into(),
        },
        Modal::Paste {
            placement_id: "place-1".into(),
            terminal_id: phux_protocol::ResourceId::local(1),
            title: "shell".into(),
            paste: crate::input::HeldPaste {
                text: "one\ntwo".into(),
            },
        },
    ]
    .into_iter()
    .map(|modal| (modal, None))
    .chain([
        // The settings form inside the palette, and the settings page, which
        // is the one card not built on `sheet`.
        (Modal::Settings, None),
        (
            Modal::Settings,
            Some(crate::app::settings_panel::Page::Appearance),
        ),
    ]);
    let top = cx.update(|window, _| super::sheet_top(window.viewport_size()));
    for (modal, page) in modals {
        let modal = &modal;
        let selector = if page.is_some() {
            "settings-panel"
        } else {
            surface(modal)
        };
        let open = |cx: &mut VisualTestContext| {
            open(&desk, modal, cx);
            if let Some(page) = page {
                // Choose the page the way the settings palette does.
                cx.update(|window, cx| {
                    desk.update(cx, |desk, cx| {
                        desk.pick(super::Target::Setting(page), window, cx)
                    })
                });
                render(cx);
            }
        };
        // Placement: every card starts at the same offset under the titlebar.
        open(cx);
        let card = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{modal:?} {page:?} renders its card"));
        assert_eq!(
            card.top(),
            gpui::px(top),
            "{modal:?} sits at the shared top"
        );
        // A click inside the card never dismisses it.
        cx.simulate_click(
            card.origin + gpui::point(gpui::px(4.), gpui::px(4.)),
            gpui::Modifiers::none(),
        );
        render(cx);
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.modal.clone()),
            *modal,
            "a click inside {modal:?} keeps it open"
        );
        // Escape dismisses and hands the keyboard back.
        keys(cx, "escape");
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.modal.clone()),
            Modal::None,
            "Escape closes {modal:?}"
        );
        assert_desk_focus(&desk, cx);
        // A click outside dismisses the same way.
        open(cx);
        cx.simulate_click(
            gpui::point(gpui::px(8.), gpui::px(8.)),
            gpui::Modifiers::none(),
        );
        render(cx);
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.modal.clone()),
            Modal::None,
            "an outside click closes {modal:?}"
        );
        assert_desk_focus(&desk, cx);
    }
}

#[gpui::test]
fn text_dialogs_own_typing_until_dismissed(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    for (modal, slot) in [
        (Modal::Rename { tab_id: "t".into() }, InputSlot::Rename),
        (
            Modal::Session {
                create: true,
                current: String::new(),
                target: None,
            },
            InputSlot::SessionName,
        ),
        (Modal::AddHost, InputSlot::HostTarget),
    ] {
        open(&desk, &modal, cx);
        type_text(cx, "cedar");
        desk.read_with(cx, |desk, cx| {
            let input = desk.input_entity(slot).expect("dialog field");
            assert_eq!(input.read(cx).value(), "cedar", "{modal:?} owns typing");
        });
        keys(cx, "escape");
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.modal.clone()),
            Modal::None
        );
        assert_desk_focus(&desk, cx);
    }
}

#[gpui::test]
fn path_loading_and_failure_read_in_the_results_not_the_footer(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    open(&desk, &Modal::Path, cx);
    // Debug selectors are `'static`; the notice and status carry their text.
    let selector = |slot: &str, text: &str| -> &'static str {
        Box::leak(format!("{slot}: {text}").into_boxed_str())
    };
    for (picker, text) in [
        (
            crate::connection::path::start_query(path_picker(), 7, "/repo", ""),
            "Searching host paths…",
        ),
        (
            crate::connection::path::PickerState {
                failure: Some("permission denied".into()),
                ..path_picker()
            },
            "permission denied",
        ),
        (
            crate::connection::path::PickerState {
                message: "Host index is warming; search again shortly.".into(),
                ..path_picker()
            },
            "Host index is warming; search again shortly.",
        ),
        (path_picker(), "No paths."),
    ] {
        desk.update(cx, |desk, cx| {
            desk.picker = Some(picker);
            cx.notify();
        });
        render(cx);
        let notice = cx
            .debug_bounds("path-results-notice")
            .expect("the state fills the results");
        let footer = cx.debug_bounds("path-footer").expect("footer");
        let panel = cx.debug_bounds("modal-sheet").expect("path panel");
        assert!(
            notice.bottom() <= footer.top(),
            "notice sits above the footer"
        );
        assert!(notice.top() >= panel.top() && footer.bottom() <= panel.bottom());
        let shown = cx
            .debug_bounds(selector("path-notice", text))
            .unwrap_or_else(|| panic!("{text:?} reads in the results"));
        assert!(shown.top() >= notice.top() && shown.bottom() <= notice.bottom());
        assert!(
            cx.debug_bounds(selector("path-status", text)).is_none(),
            "{text:?} is not repeated in the footer"
        );
    }
    // Listed rows keep paging status in the footer.
    desk.update(cx, |desk, cx| {
        desk.picker = Some(crate::connection::path::PickerState {
            rows: vec![crate::connection::path::PathRow {
                path: "/repo/a".into(),
                kind: "file".into(),
            }],
            message: "Results truncated; narrow the search.".into(),
            ..path_picker()
        });
        cx.notify();
    });
    render(cx);
    let status = cx
        .debug_bounds("path-status: Results truncated; narrow the search.")
        .expect("truncation reads in the footer beside listed rows");
    let footer = cx.debug_bounds("path-footer").expect("footer");
    assert!(status.top() >= footer.top() && status.bottom() <= footer.bottom());
    assert!(cx.debug_bounds("path-results-notice").is_none());
}

#[gpui::test]
fn machines_are_reachable_from_the_palette_sidebar_and_titlebar(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    let connections = Some(crate::app::settings_panel::Page::Connections);

    keys(cx, "cmd-shift-p");
    type_text(cx, "add machine");
    assert_eq!(selected_command(&desk, cx), Some("add-host"));
    keys(cx, "enter");
    assert_eq!(modal(&desk, cx), Modal::AddHost);
    keys(cx, "escape");

    keys(cx, "cmd-shift-p");
    type_text(cx, "machines…");
    assert_eq!(selected_command(&desk, cx), Some("machines"));
    keys(cx, "enter");
    assert_eq!(modal(&desk, cx), Modal::Settings);
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.settings_page),
        connections
    );
    keys(cx, "escape");
    assert_eq!(modal(&desk, cx), Modal::None);

    click(cx, "sidebar-machines");
    assert_eq!(modal(&desk, cx), Modal::Settings);
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.settings_page),
        connections
    );
    keys(cx, "escape");

    click(cx, "workspace-machines");
    assert_eq!(modal(&desk, cx), Modal::Settings);
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.settings_page),
        connections
    );
}
