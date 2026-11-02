//! Direct, live settings remain available in the Cmd-P palette.

use super::*;

#[gpui::test]
fn interface_style_and_pane_glass_are_live_without_overwriting_terminal_typography(
    cx: &mut TestAppContext,
) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, |desk, cx| {
        desk.update_prefs(
            |prefs| {
                prefs.font_size = 17.;
                prefs.terminal_opacity = 0.65;
            },
            cx,
        )
    });
    keys(cx, "cmd-p");
    type_text(cx, "Interface style");
    keys(cx, "enter");
    click(cx, "skin-card-classic");
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.prefs.skin_id, "classic");
        assert!(!desk.prefs.plain);
        assert_eq!(desk.look().card.a, 1.);
        assert_eq!(desk.skin().chrome.sidebar, 1.);
    });
    click(cx, "skin-card-glass");
    keys(cx, "escape");
    keys(cx, "cmd-p");
    type_text(cx, "Glassy panes");
    keys(cx, "enter");
    desk.read_with(cx, |desk, _| {
        assert!(!desk.prefs.pane_glass);
        assert_eq!(desk.look().card.a, 1.);
        assert_eq!(desk.prefs.terminal_opacity, 0.65);
        assert_eq!(desk.prefs.font_size, 17.);
    });
    keys(cx, "enter");
    desk.read_with(cx, |desk, _| {
        assert!(desk.prefs.pane_glass);
        assert!(desk.look().card.a < 0.7);
        assert_eq!(desk.prefs.terminal_opacity, 0.65);
    });
}

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
            "Interface style",
            "Font family",
            "Font size",
            "Animations",
            "Laser cursor",
            "Tab placement",
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
        assert_eq!(
            entries
                .iter()
                .any(|(key, _)| key.title.as_ref() == "Option sends Alt"),
            cfg!(target_os = "macos"),
            "Option sends Alt is searchable only where Option composes"
        );
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
        gpui::size(gpui::px(1199.), gpui::px(899.)),
        gpui::size(gpui::px(360.), gpui::px(480.)),
        gpui::size(gpui::px(361.), gpui::px(481.)),
    ] {
        cx.simulate_resize(size);
        // GPUI snaps both absolute edges to device pixels. An odd physical
        // height cannot have both integral edges and an exact viewport center.
        let half_pixel = cx.update(|window, _| gpui::px(0.5 / window.scale_factor()));
        let center = gpui::point(size.width / 2., size.height / 2.);
        keys(cx, "cmd-p");
        for text in ["", "Horizontal padding"] {
            type_text(cx, text);
            let panel = cx.debug_bounds("palette-panel").unwrap();
            assert!(
                (panel.center().x - center.x).abs() <= half_pixel
                    && (panel.center().y - center.y).abs() <= half_pixel,
                "pixel-snapped panel stays centered: {panel:?} within {size:?}"
            );
            assert!(panel.left() >= gpui::px(16.) && panel.right() <= size.width - gpui::px(16.));
        }
        keys(cx, "enter");
        let editor = cx.debug_bounds("palette-panel").unwrap();
        assert!(
            (editor.center().x - center.x).abs() <= half_pixel
                && (editor.center().y - center.y).abs() <= half_pixel,
            "pixel-snapped editor stays centered: {editor:?} within {size:?}"
        );
        assert!(editor.top() >= gpui::px(16.) && editor.bottom() <= size.height - gpui::px(16.));
        keys(cx, "escape");
    }
}

#[gpui::test]
fn wide_palette_controls_fit_inside_the_exact_editor_at_narrow_widths(cx: &mut TestAppContext) {
    let (handle, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    for width in [360., 480., 760., 1200.] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(700.)));
        for (query, selectors) in [
            ("Theme", vec!["settings-theme"]),
            ("Font family", vec!["settings-font"]),
            (
                "Tab placement",
                vec![
                    "tab-position-top",
                    "tab-position-bottom",
                    "tab-position-side",
                ],
            ),
        ] {
            keys(cx, "cmd-p");
            type_text(cx, query);
            keys(cx, "enter");
            let editor = cx.debug_bounds("palette-setting-editor").unwrap();
            let inset = cx.update(|window, _| window.rem_size());
            for selector in selectors {
                let control = cx.debug_bounds(selector).expect(selector);
                assert!(
                    control.left() >= editor.left() + inset
                        && control.right() <= editor.right() - inset,
                    "{query} control {selector} fits at {width}: {control:?} in {editor:?}"
                );
            }
            keys(cx, "escape");
        }
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
