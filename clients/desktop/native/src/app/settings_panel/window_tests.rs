//! Settings lives in a native utility window; palette searches stay lightweight.

use super::{Page, window::SettingsWindow};
use crate::app::desk::Desk;
use gpui::{Entity, TestAppContext, VisualTestContext};

pub(in crate::app) fn opened(
    desk: &Entity<Desk>,
    cx: &gpui::TestAppContext,
) -> (gpui::AnyWindowHandle, Entity<SettingsWindow>) {
    desk.read_with(cx, |desk, cx| {
        desk.desktop
            .read(cx)
            .settings
            .clone()
            .expect("settings window")
    })
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

#[gpui::test]
fn settings_shortcut_opens_a_separate_reusable_window(cx: &mut TestAppContext) {
    let (owner, desk) = crate::app::overlays::palette_tests::fixture(cx);
    let workspace = &mut VisualTestContext::from_window(owner, cx);
    workspace.simulate_keystrokes("cmd-,");
    draw(workspace);
    let (handle, _) = opened(&desk, workspace);
    assert_ne!(handle, owner);
    assert_eq!(
        desk.read_with(workspace, |desk, _| desk.modal.clone()),
        crate::app::desk::Modal::None
    );
    assert!(workspace.debug_bounds("palette-panel").is_none());
    workspace.simulate_keystrokes("cmd-,");
    draw(workspace);
    assert_eq!(opened(&desk, workspace).0, handle);
    let settings = &mut VisualTestContext::from_window(handle, workspace);
    draw(settings);
    assert!(settings.debug_bounds("settings-sidebar").is_some());
    assert!(settings.debug_bounds("settings-content").is_some());
    settings.simulate_keystrokes("cmd-w");
    settings.run_until_parked();
    assert!(desk.read_with(settings, |desk, cx| {
        desk.desktop.read(cx).settings.is_none()
    }));
}

#[gpui::test]
fn settings_categories_search_clear_and_live_edits(cx: &mut TestAppContext) {
    let (owner, desk) = crate::app::overlays::palette_tests::fixture(cx);
    let workspace = &mut VisualTestContext::from_window(owner, cx);
    workspace.update(|window, cx| desk.update(cx, |desk, cx| desk.execute("settings", window, cx)));
    draw(workspace);
    let (handle, view) = opened(&desk, workspace);
    let settings = &mut VisualTestContext::from_window(handle, workspace);
    draw(settings);
    let sidebar_before = settings.debug_bounds("settings-sidebar").unwrap();
    let category = settings.debug_bounds("settings-category-Layout").unwrap();
    settings.simulate_click(category.center(), gpui::Modifiers::none());
    draw(settings);
    assert_eq!(view.read_with(settings, |view, _| view.page), Page::Layout);
    let choice = settings.debug_bounds("tab-position-bottom").unwrap();
    settings.simulate_click(choice.center(), gpui::Modifiers::none());
    draw(settings);
    assert_eq!(
        desk.read_with(settings, |desk, _| desk.prefs.tab_position),
        crate::settings::TabPosition::Bottom
    );
    assert_eq!(
        settings.debug_bounds("settings-sidebar").unwrap(),
        sidebar_before
    );
    settings.simulate_keystrokes("cmd-f");
    settings.simulate_input("line height");
    draw(settings);
    assert_eq!(view.read_with(settings, |view, _| view.page), Page::Spacing);
    assert!(settings.debug_bounds("settings-category-Spacing").is_some());
    assert!(settings.debug_bounds("settings-category-Updates").is_none());
    settings.simulate_keystrokes("cmd-a");
    settings.simulate_input("no-such-preference-xyz");
    draw(settings);
    assert!(settings.debug_bounds("settings-empty").is_some());
    settings.simulate_keystrokes("escape");
    draw(settings);
    assert!(settings.debug_bounds("settings-empty").is_none());
    assert!(
        settings
            .debug_bounds("settings-category-Appearance")
            .is_some()
    );
}

#[gpui::test]
fn settings_wide_controls_fit_and_palette_shortcut_returns_to_workspace(cx: &mut TestAppContext) {
    let (owner, desk) = crate::app::overlays::palette_tests::fixture(cx);
    let workspace = &mut VisualTestContext::from_window(owner, cx);
    workspace.simulate_keystrokes("cmd-,");
    draw(workspace);
    let (handle, _) = opened(&desk, workspace);
    let settings = &mut VisualTestContext::from_window(handle, workspace);
    for width in [700., 860., 1100.] {
        settings.simulate_resize(gpui::size(gpui::px(width), gpui::px(900.)));
        draw(settings);
        let content = settings.debug_bounds("settings-content").unwrap();
        let theme = settings.debug_bounds("settings-theme").unwrap();
        assert!(
            theme.left() >= content.left() && theme.right() <= content.right(),
            "theme fits at {width}: {theme:?}"
        );
    }
    settings.simulate_keystrokes("cmd-p");
    draw(settings);
    assert_eq!(
        desk.read_with(settings, |desk, _| desk.modal.clone()),
        crate::app::desk::Modal::Goto
    );
    assert!(
        settings.debug_bounds("palette-panel").is_none(),
        "the palette belongs to the workspace"
    );
    let workspace = &mut VisualTestContext::from_window(owner, settings);
    draw(workspace);
    assert!(workspace.debug_bounds("palette-panel").is_some());
}

#[gpui::test]
fn closing_the_owner_closes_its_settings_without_leaking_a_desk(cx: &mut TestAppContext) {
    let (owner, desk) = crate::app::overlays::palette_tests::fixture(cx);
    let workspace = &mut VisualTestContext::from_window(owner, cx);
    workspace.simulate_keystrokes("cmd-,");
    draw(workspace);
    let settings = opened(&desk, workspace).0;
    owner
        .update(workspace, |_, window, _| window.remove_window())
        .unwrap();
    workspace.run_until_parked();
    assert!(settings.update(workspace, |_, _, _| ()).is_err());
    assert!(desk.read_with(workspace, |desk, cx| {
        desk.desktop.read(cx).settings.is_none()
    }));
}

#[gpui::test]
fn native_edit_menu_edits_settings_search_not_the_terminal(cx: &mut TestAppContext) {
    use crate::app::actions::Run;
    let (owner, desk) = crate::app::overlays::palette_tests::fixture(cx);
    let workspace = &mut VisualTestContext::from_window(owner, cx);
    workspace.simulate_keystrokes("cmd-,");
    draw(workspace);
    let (handle, view) = opened(&desk, workspace);
    let settings = &mut VisualTestContext::from_window(handle, workspace);
    settings.dispatch_action(Run::new("find"));
    settings.simulate_input("no-such-setting-xyz");
    draw(settings);
    assert!(settings.debug_bounds("settings-empty").is_some());
    settings.update(|_, cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("line height".into()))
    });
    settings.dispatch_action(Run::new("select-all"));
    settings.dispatch_action(Run::new("paste"));
    draw(settings);
    assert_eq!(view.read_with(settings, |view, _| view.page), Page::Spacing);
    assert!(settings.debug_bounds("settings-empty").is_none());
    settings.dispatch_action(Run::new("select-all"));
    settings.dispatch_action(Run::new("copy"));
    settings.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("line height".into())
        )
    });
    assert_eq!(
        desk.read_with(settings, |desk, _| desk.modal.clone()),
        crate::app::desk::Modal::None
    );
}
