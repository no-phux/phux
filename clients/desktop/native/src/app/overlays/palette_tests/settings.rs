//! Direct, live settings remain available in the Cmd-P palette.

use super::*;

#[gpui::test]
fn switch_results_toggle_both_ways_keep_search_and_save(cx: &mut TestAppContext) {
    let path = std::env::temp_dir()
        .join(format!(
            "phux-palette-test-{}-{}",
            std::process::id(),
            cx.test_function_name().unwrap()
        ))
        .join("layout.json");
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, |desk, _| desk.persist_writes = true);
    let before = desk.read_with(cx, |desk, _| desk.prefs.notifications);
    keys(cx, "cmd-p");
    type_text(cx, "Agent notifications");
    keys(cx, "enter");
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.prefs.notifications),
        !before
    );
    assert_eq!(modal(&desk, cx), Modal::Goto);
    assert_eq!(query(&desk, cx), "Agent notifications");
    keys(cx, "enter");
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.prefs.notifications),
        before
    );
    click(
        cx,
        "palette-setting:Layout:Notifications:Agent notifications",
    );
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.prefs.notifications),
        !before
    );
    cx.update(|_, cx| desk.update(cx, |desk, cx| desk.prepare_close(cx)));
    let store = crate::workspace::persist::FileLayoutStore::new(path, None);
    let saved = crate::workspace::persist::load_layout(&store.read())
        .initial
        .unwrap();
    assert_eq!(
        saved.display.notifications, !before,
        "palette toggles use the persistent preference writer"
    );
}

#[gpui::test]
fn command_p_edits_the_exact_choice_and_back_restores_query(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    keys(cx, "cmd-p");
    type_text(cx, "Tab placement");
    keys(cx, "enter");
    assert!(cx.debug_bounds("palette-setting-editor").is_some());
    assert_eq!(modal(&desk, cx), Modal::Goto);
    for (selector, position) in [
        ("tab-position-bottom", crate::settings::TabPosition::Bottom),
        ("tab-position-top", crate::settings::TabPosition::Top),
    ] {
        click(cx, selector);
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.prefs.tab_position),
            position
        );
    }
    click(cx, "palette-settings-back");
    assert_eq!(query(&desk, cx), "Tab placement");
    assert!(cx.debug_bounds("palette-setting-editor").is_none());
    keys(cx, "escape");
    type_text(cx, "Font family");
    keys(cx, "enter");
    assert_eq!(
        desk.read_with(cx, |desk, _| desk
            .palette_view
            .edit
            .as_ref()
            .map(|key| key.title.to_string())),
        Some("Font family".into())
    );
    assert!(cx.debug_bounds("palette-setting-editor").is_some());
}

#[gpui::test]
fn settings_catalog_covers_controls_without_shortcut_or_status_dead_ends(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    keys(cx, "cmd-p");
    type_text(cx, "font");
    let handle = desk.downgrade();
    desk.read_with(cx, |desk, cx| {
        let entries = crate::app::settings_panel::entries(desk, &handle, cx);
        for title in [
            "Theme",
            "Skin",
            "Font family",
            "Font size",
            "Animations",
            "Laser cursor",
            "Tab placement",
            "Option sends Alt",
            "Line height",
            "Horizontal padding",
            "Unfocused panes",
            "Check automatically",
            "Update phux with Desktop",
            "Desktop",
            "phux",
            "Machines",
            "Count Rust allocations",
        ] {
            assert!(
                entries.iter().any(|(key, _)| key.title.as_ref() == title),
                "{title} is directly searchable"
            );
        }
        for title in ["Status", "Ghostty config", "Remote machines"] {
            assert!(
                !entries.iter().any(|(key, _)| key.title.as_ref() == title),
                "{title} is information, not an editable setting"
            );
        }
        let mut keys = Vec::new();
        for (key, _) in entries {
            assert!(
                !keys.contains(&key),
                "each control has an unambiguous identity: {key:?}"
            );
            keys.push(key);
        }
    });
}

#[gpui::test]
fn palette_and_exact_editor_stay_centered_after_filtering_and_resize(cx: &mut TestAppContext) {
    let (handle, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    for size in [
        gpui::size(gpui::px(1200.), gpui::px(900.)),
        gpui::size(gpui::px(360.), gpui::px(480.)),
    ] {
        cx.simulate_resize(size);
        keys(cx, "cmd-p");
        for text in ["", "Horizontal padding"] {
            type_text(cx, text);
            let panel = cx.debug_bounds("palette-panel").unwrap();
            assert_eq!(
                panel.center(),
                gpui::point(size.width / 2., size.height / 2.)
            );
            assert!(panel.left() >= gpui::px(16.) && panel.right() <= size.width - gpui::px(16.));
        }
        keys(cx, "enter");
        let editor = cx.debug_bounds("palette-panel").unwrap();
        assert_eq!(
            editor.center(),
            gpui::point(size.width / 2., size.height / 2.)
        );
        assert!(editor.top() >= gpui::px(16.) && editor.bottom() <= size.height - gpui::px(16.));
        keys(cx, "escape");
    }
}

#[gpui::test]
fn numeric_setting_is_keyboard_editable(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    let before = desk.read_with(cx, |desk, _| desk.prefs.font_size);
    keys(cx, "cmd-p");
    type_text(cx, "Font size");
    keys(cx, "enter tab up");
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.prefs.font_size),
        before + 1.
    );
    keys(cx, "down");
    assert_eq!(desk.read_with(cx, |desk, _| desk.prefs.font_size), before);
    keys(cx, "cmd-a");
    type_text(cx, "999");
    keys(cx, "tab");
    assert_eq!(desk.read_with(cx, |desk, _| desk.prefs.font_size), 28.);
}
