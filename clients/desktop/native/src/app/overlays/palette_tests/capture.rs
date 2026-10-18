use super::*;

#[gpui::test]
fn capture_prefix_takes_typing_and_retains_a_refused_draft(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    keys(cx, "cmd-shift-p");
    type_text(cx, "!fix the sidebar");
    assert_eq!(modal(&desk, cx), Modal::Capture);
    assert!(cx.debug_bounds("capture-panel").is_some());
    keys(cx, "enter");
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.capture.draft, "fix the sidebar");
        assert!(desk.capture.error.is_some());
    });
    keys(cx, "escape cmd-shift-p");
    click(cx, "palette-mode-capture");
    desk.read_with(cx, |desk, cx| {
        let (input, _) = desk.capture.editor.as_ref().unwrap();
        assert_eq!(input.read(cx).value().as_str(), "fix the sidebar");
    });
    keys(cx, "shift-enter");
    assert_eq!(modal(&desk, cx), Modal::Capture);
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.capture.draft, "fix the sidebar\n")
    });
}

#[gpui::test]
fn goto_also_accepts_capture_prefix_and_escape_restores_focus(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    keys(cx, "cmd-p");
    type_text(cx, "!");
    assert_eq!(modal(&desk, cx), Modal::Capture);
    type_text(cx, "next thought");
    keys(cx, "escape");
    assert_desk_focus(&desk, cx);
    desk.read_with(cx, |desk, _| assert_eq!(desk.capture.draft, "next thought"));
}

#[gpui::test]
fn capture_shortcut_opens_focused_and_accepts_a_burst_without_mouse_or_extra_frames(
    cx: &mut TestAppContext,
) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_keystrokes("cmd-shift-space");
    for ch in "fix this now".chars() {
        press(cx, ch);
    }
    cx.run_until_parked();
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.modal, Modal::Capture);
        assert_eq!(desk.capture.draft, "fix this now");
    });
    cx.update(|window, cx| {
        let desk = desk.read(cx);
        assert!(
            desk.capture
                .editor
                .as_ref()
                .unwrap()
                .0
                .focus_handle(cx)
                .is_focused(window)
        );
    });
}

#[gpui::test]
fn capture_prefix_routes_native_text_before_another_paint(cx: &mut TestAppContext) {
    use gpui::PlatformWindow as _;
    for chord in ["cmd-shift-p", "cmd-p"] {
        let (handle, desk) = fixture(cx);
        let cx = &mut VisualTestContext::from_window(handle, cx);
        keys(cx, chord);
        press(cx, '!');
        cx.run_until_parked();
        assert_eq!(modal(&desk, cx), Modal::Capture);
        // AppKit/IME inserts into the installed text handler directly. Unlike
        // dispatch_keystroke, this does not draw a dirty window first.
        let mut platform = cx.test_window(handle);
        let mut handler = platform.take_input_handler().expect("native text input");
        handler.replace_text_in_range(None, "東京 thought");
        platform.set_input_handler(handler);
        cx.run_until_parked();
        desk.read_with(cx, |desk, _| assert_eq!(desk.capture.draft, "東京 thought"));
    }
}

#[gpui::test]
fn capture_composer_edits_multiline_and_preserves_undo_across_dismissal(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    keys(cx, "cmd-shift-p");
    type_text(cx, "!first line");
    keys(cx, "shift-enter");
    type_text(cx, "second line");
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.capture.draft, "first line\nsecond line");
        assert!(desk.capture.error.is_none(), "a newline never submits");
    });
    let input = desk.read_with(cx, |desk, _| {
        desk.capture.editor.as_ref().unwrap().0.clone()
    });
    keys(cx, "escape");
    cx.update(|window, cx| desk.update(cx, |desk, cx| desk.execute("capture", window, cx)));
    render(cx);
    assert_eq!(
        input,
        desk.read_with(cx, |desk, _| desk
            .capture
            .editor
            .as_ref()
            .unwrap()
            .0
            .clone())
    );
    keys(
        cx,
        if cfg!(target_os = "macos") {
            "cmd-z"
        } else {
            "ctrl-z"
        },
    );
    desk.read_with(cx, |desk, _| assert_eq!(desk.capture.draft, "first line\n"));
    keys(
        cx,
        if cfg!(target_os = "macos") {
            "cmd-enter"
        } else {
            "ctrl-enter"
        },
    );
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.capture.draft, "first line\n");
        assert!(
            desk.capture.error.is_some(),
            "send-and-stay reaches dispatch validation"
        );
    });
}

#[gpui::test]
fn capture_composer_wraps_hints_and_keeps_controls_inside_a_narrow_window(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(gpui::px(360.), gpui::px(480.)));
    cx.update(|window, cx| desk.update(cx, |desk, cx| desk.execute("capture", window, cx)));
    type_text(cx, "a thought in a narrow window");
    let panel = cx.debug_bounds("capture-panel").unwrap();
    assert!(panel.left() >= gpui::px(16.) && panel.right() <= gpui::px(344.));
    assert!(panel.bottom() <= gpui::px(464.));
    keys(cx, "enter");
    assert_eq!(modal(&desk, cx), Modal::Capture);
    let panel = cx.debug_bounds("capture-panel").unwrap();
    assert!(panel.bottom() <= gpui::px(464.), "refusal still fits");
    for _ in 0..7 {
        keys(cx, "shift-enter");
        type_text(cx, "one more line");
    }
    desk.update(cx, |desk, cx| {
        desk.capture.error = Some("A long refusal with details\n".repeat(12));
        cx.notify();
    });
    cx.simulate_resize(gpui::size(gpui::px(320.), gpui::px(280.)));
    render(cx);
    let panel = cx.debug_bounds("capture-panel").unwrap();
    assert!(
        panel.bottom() <= gpui::px(264.),
        "long drafts and refusals scroll inside the sheet"
    );
}
