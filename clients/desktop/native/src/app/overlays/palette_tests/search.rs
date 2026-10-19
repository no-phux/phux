//! Typing, ranking, confirming and dismissing in the palette.

use super::*;

#[gpui::test]
fn allocation_settings_are_discoverable_without_a_development_launch(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    keys(cx, "cmd-p");
    type_text(cx, "allocation");
    keys(cx, "enter");
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.settings_page),
        Some(crate::app::settings_panel::Page::Performance)
    );
    assert!(cx.debug_bounds("settings-panel").is_some());
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
    // The row's 6pt kit padding, the list's 4pt inset, the card's 1pt border.
    assert_eq!(
        panel.bottom() - last.bottom(),
        gpui::px(11.),
        "the panel ends the list's inset below its last row"
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
    keys(
        cx,
        if cfg!(target_os = "macos") {
            "cmd-v"
        } else {
            "ctrl-v"
        },
    );
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
            super::super::command_results(desk, cx, "")
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
