use super::*;
use gpui::{KeyUpEvent, PlatformInput, TestAppContext, VisualTestContext};

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

fn keys(cx: &mut VisualTestContext, keys: &'static str) {
    for key in keys.split_whitespace() {
        cx.update(|window, cx| {
            let stroke = Keystroke::parse(key).unwrap();
            window.dispatch_keystroke(stroke.clone(), cx);
            window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke: stroke }), cx);
        });
        draw(cx);
    }
}

#[gpui::test]
fn leader_is_additive_cancellable_and_rebinds_live(cx: &mut TestAppContext) {
    let (owner, desk) = crate::app::overlays::palette_tests::fixture(cx);
    let cx = &mut VisualTestContext::from_window(owner, cx);
    desk.update(cx, |desk, cx| {
        desk.update_prefs(
            |prefs| {
                prefs.leader_key = Some("ctrl+a".into());
                prefs.sidebar_visible = true;
            },
            cx,
        )
    });
    draw(cx);
    keys(cx, "ctrl-a b");
    assert!(!desk.read_with(cx, |desk, _| desk.prefs.sidebar_visible));
    keys(cx, "cmd-b");
    assert!(desk.read_with(cx, |desk, _| desk.prefs.sidebar_visible));
    keys(cx, "ctrl-a escape b");
    assert!(desk.read_with(cx, |desk, _| desk.prefs.sidebar_visible));
    keys(cx, "ctrl-a f b");
    assert!(desk.read_with(cx, |desk, _| desk.prefs.sidebar_visible));
    keys(cx, "ctrl-a cmd-b b");
    assert!(!desk.read_with(cx, |desk, _| desk.prefs.sidebar_visible));

    desk.update(cx, |desk, cx| {
        desk.update_prefs(|prefs| prefs.leader_key = Some("ctrl+space".into()), cx)
    });
    keys(cx, "ctrl-a b");
    assert!(!desk.read_with(cx, |desk, _| desk.prefs.sidebar_visible));
    keys(cx, "ctrl-space b");
    assert!(desk.read_with(cx, |desk, _| desk.prefs.sidebar_visible));
    desk.update(cx, |desk, cx| {
        desk.update_prefs(|prefs| prefs.leader_key = None, cx)
    });
    keys(cx, "ctrl-space b");
    assert!(desk.read_with(cx, |desk, _| desk.prefs.sidebar_visible));
}

#[gpui::test]
fn leader_does_not_capture_palette_typing_or_other_windows(cx: &mut TestAppContext) {
    let (owner, desk) = crate::app::overlays::palette_tests::fixture(cx);
    let cx = &mut VisualTestContext::from_window(owner, cx);
    desk.update(cx, |desk, cx| {
        desk.update_prefs(|prefs| prefs.leader_key = Some("ctrl+a".into()), cx)
    });
    draw(cx);
    keys(cx, "ctrl-a space");
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.modal.clone()),
        Modal::Commands
    );
    keys(cx, "ctrl-a b");
    let query = desk.read_with(cx, |desk, cx| {
        desk.palette_state.as_ref().unwrap().read(cx).query(cx)
    });
    assert_eq!(query.as_ref(), "b");
    keys(cx, "escape");
    keys(cx, "ctrl-a cmd-,");
    let (settings, _) = crate::app::settings_panel::window_tests::opened(&desk, cx);
    let settings = &mut VisualTestContext::from_window(settings, cx);
    keys(settings, "ctrl-a b");
    assert!(desk.read_with(settings, |desk, _| desk.prefs.sidebar_visible));
}

#[gpui::test]
fn held_leader_never_becomes_a_second_press(cx: &mut TestAppContext) {
    let (owner, desk) = crate::app::overlays::palette_tests::fixture(cx);
    let cx = &mut VisualTestContext::from_window(owner, cx);
    desk.update(cx, |desk, cx| {
        desk.update_prefs(|prefs| prefs.leader_key = Some("ctrl+a".into()), cx)
    });
    draw(cx);
    cx.simulate_keystrokes("ctrl-a ctrl-a ctrl-a");
    keys(cx, "b");
    assert!(!desk.read_with(cx, |desk, _| desk.prefs.sidebar_visible));
}

#[gpui::test]
fn a_conflicting_command_chord_is_only_a_leader_while_enabled(cx: &mut TestAppContext) {
    let (owner, desk) = crate::app::overlays::palette_tests::fixture(cx);
    let cx = &mut VisualTestContext::from_window(owner, cx);
    desk.update(cx, |desk, cx| {
        desk.update_prefs(|prefs| prefs.leader_key = Some("cmd+b".into()), cx)
    });
    draw(cx);
    keys(cx, "cmd-b escape");
    assert!(desk.read_with(cx, |desk, _| desk.prefs.sidebar_visible));
    keys(cx, "cmd-b b");
    assert!(!desk.read_with(cx, |desk, _| desk.prefs.sidebar_visible));
    desk.update(cx, |desk, cx| {
        desk.update_prefs(|prefs| prefs.leader_key = None, cx)
    });
    keys(cx, "cmd-b");
    assert!(desk.read_with(cx, |desk, _| desk.prefs.sidebar_visible));
}

#[gpui::test]
fn releasing_shift_before_a_symbol_leader_does_not_leave_it_held(cx: &mut TestAppContext) {
    use gpui::InputEvent as _;
    let (owner, desk) = crate::app::overlays::palette_tests::fixture(cx);
    let cx = &mut VisualTestContext::from_window(owner, cx);
    desk.update(cx, |desk, cx| {
        desk.update_prefs(|prefs| prefs.leader_key = Some("ctrl+shift+/".into()), cx)
    });
    draw(cx);
    for shown in [false, true] {
        cx.update(|window, cx| {
            let mut stroke = Keystroke::parse("ctrl-shift-/").unwrap();
            stroke.key = "?".into();
            window.dispatch_event(
                gpui::KeyDownEvent {
                    keystroke: stroke,
                    is_held: false,
                    prefer_character_input: false,
                }
                .to_platform_input(),
                cx,
            );
            window.dispatch_event(
                KeyUpEvent {
                    keystroke: Keystroke::parse("ctrl-/").unwrap(),
                }
                .to_platform_input(),
                cx,
            );
        });
        keys(cx, "b");
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.prefs.sidebar_visible),
            shown
        );
    }
}
