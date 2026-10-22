//! Exercise the actual window surface and dismissal, not just the view model.

use super::*;
use crate::app::{AppOptions, desk::Desk};
use gpui::{Entity, TestAppContext, VisualTestContext, px};
use gpui_kit::component::Root;

fn fixture(cx: &mut TestAppContext) -> (gpui::AnyWindowHandle, Entity<Desk>) {
    let path = std::env::temp_dir().join(format!(
        "phux-update-card-{}-{}",
        std::process::id(),
        cx.test_function_name().unwrap()
    ));
    std::fs::create_dir_all(&path).unwrap();
    let handle = cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        crate::app::open(
            AppOptions {
                socket: path.join("absent.sock"),
                session: String::new(),
                phux: path.join("not-installed-phux"),
                layout_path: path.join("layout.json"),
                fresh: false,
                initial_terminal: None,
                quick: false,
                dev_telemetry: false,
                startup_error: Some("Update card fixture: no server".into()),
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
    desk.update(cx, |desk, cx| {
        desk.updates.update(cx, |updater, cx| {
            *updater = installed("0.2.0");
            updater.target = Some(release("0.3.0", &updates::own_protocol()));
            updater.runtime = Some(runtime(None));
            cx.notify();
        });
    });
    (*handle, desk)
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.bounds_changed(cx);
            window.draw(cx).clear(cx);
        });
    }
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx.debug_bounds(selector).expect(selector);
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    draw(cx);
}

#[gpui::test]
fn corner_card_can_be_dismissed_and_reopened_without_starting_an_update(cx: &mut TestAppContext) {
    let (window, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(window, cx);
    draw(cx);
    let bounds = cx
        .debug_bounds("update-card")
        .expect("available update card paints");
    assert!(bounds.origin.x > px(0.));
    assert!(bounds.size.width <= px(344.));
    click(cx, "update-dismiss");
    assert!(cx.debug_bounds("update-card").is_none());
    click(cx, "desktop-update-ready");
    assert!(cx.debug_bounds("update-card").is_some());
    desk.read_with(cx, |desk, cx| {
        let updater = desk.updates.read(cx);
        assert!(!updater.busy);
        assert!(updater.staged.is_none());
    });
}

#[gpui::test]
fn busy_card_survives_progress_and_details_opens_the_shared_settings(cx: &mut TestAppContext) {
    let (window, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(window, cx);
    desk.update(cx, |desk, cx| {
        desk.updates.update(cx, |updater, cx| {
            updater.busy = true;
            updater.work = Work::Downloading;
            updater.progress = Some(updates::StageProgress::Downloading(63));
            cx.notify();
        })
    });
    draw(cx);
    assert!(cx.debug_bounds("update-card").is_some());
    assert!(
        cx.debug_bounds("desktop-update-ready").is_some(),
        "busy does not remove the trigger"
    );
    click(cx, "update-details");
    assert!(
        cx.debug_bounds("update-card").is_none(),
        "card does not overlap Settings"
    );
    desk.read_with(cx, |desk, _| {
        assert_eq!(
            desk.settings_page,
            Some(crate::app::settings_panel::Page::Updates)
        )
    });
    desk.update(cx, |desk, cx| {
        desk.updates.update(cx, |updater, cx| {
            updater.card_expanded = false;
            cx.notify();
        })
    });
    // Settings is modal, like every other overlay. Close it before using toolbar controls.
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(
        cx.debug_bounds("update-card").is_none(),
        "closing Settings preserves dismissal"
    );
    click(cx, "desktop-update-ready");
    assert!(cx.debug_bounds("update-card").is_some());
}

#[gpui::test]
fn short_windows_keep_the_card_inside_the_viewport_without_taking_focus(cx: &mut TestAppContext) {
    let (window, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(window, cx);
    cx.simulate_resize(gpui::size(px(420.), px(300.)));
    cx.update(|window, cx| {
        let focus = desk.read(cx).focus.clone();
        window.focus(&focus, cx);
    });
    desk.update(cx, |desk, cx| {
        desk.updates.update(cx, |updater, cx| {
            updater.message = format!(
                "Desktop: Update failed: {}",
                "Checksum mismatch. ".repeat(20)
            );
            cx.notify();
        })
    });
    draw(cx);
    let bounds = cx.debug_bounds("update-card").unwrap();
    assert!(bounds.origin.x >= px(0.));
    assert!(bounds.right() <= px(420.));
    assert!(bounds.bottom() <= px(300.));
    cx.update(|window, cx| {
        assert!(
            desk.read(cx).focus.is_focused(window),
            "passive update cards never take terminal focus"
        )
    });
}
