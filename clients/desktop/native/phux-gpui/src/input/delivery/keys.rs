//! Keys and committed text through a focused input.

use super::*;

#[cfg(feature = "patched-gpui")]
#[gpui::test]
fn only_the_terminal_surface_disables_apple_press_and_hold(cx: &mut TestAppContext) {
    use gpui::{Bounds, ElementInputHandler, InputHandler, PlatformWindow};
    let client = session(None);
    let view = client.create_view(&terminal()).expect("view");
    let handle = cx.add_window(|window, cx| {
        crate::terminal::Terminal::new(
            client.clone(),
            view,
            terminal(),
            crate::terminal::Settings::default(),
            window,
            cx,
        )
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        let input = bind(&client, view, window, cx);
        let mut default_handler = ElementInputHandler::new(Bounds::default(), input);
        assert!(default_handler.apple_press_and_hold_enabled());
    })
    .unwrap();
    let mut installed = cx
        .test_window(handle.into())
        .take_input_handler()
        .expect("the painted terminal installed its text handler");
    assert!(
        !installed.apple_press_and_hold_enabled(),
        "held printable keys must bypass the macOS accent picker"
    );
}

#[gpui::test]
fn printable_key_commits_preserve_repeat_and_release(cx: &mut TestAppContext) {
    with_focused(cx, |client, input, window, cx| {
        for (stroke, text) in [("a->a", "a"), ("shift-a->A", "A"), ("space-> ", " ")] {
            let stroke = Keystroke::parse(stroke).expect("stroke");
            for (is_held, action) in [
                (false, KeyAction::Press),
                (true, KeyAction::Repeat),
                (true, KeyAction::Repeat),
            ] {
                input.update(cx, |state, cx| {
                    assert_eq!(
                        state.key_down(&down(&stroke, is_held), window, cx),
                        Ok(KeyDisposition::Platform)
                    );
                    assert!(client.take_outbound().is_empty(), "wait for platform text");
                    state.replace_text_in_range(None, text, window, cx);
                });
                let events = keys(client);
                assert_eq!(events.len(), 1, "one commit per down, no duplicate input");
                assert_eq!(events[0].action, action);
                assert_eq!(events[0].text.as_deref(), Some(text));
            }
            input.update(cx, |state, cx| {
                let up = KeyUpEvent { keystroke: stroke };
                assert_eq!(state.key_up(&up, window, cx), Ok(KeyDisposition::Consumed));
                assert!(state.pressed.is_empty());
            });
            let events = keys(client);
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].action, KeyAction::Release);
            assert_eq!(events[0].text, None);
        }
    });
}

#[gpui::test]
fn named_and_control_keys_repeat_without_platform_text(cx: &mut TestAppContext) {
    with_focused(cx, |client, input, window, cx| {
        input.update(cx, |state, cx| {
            for stroke in ["left", "backspace", "enter", "ctrl-a"] {
                let stroke = Keystroke::parse(stroke).expect("stroke");
                for is_held in [false, true] {
                    assert_eq!(
                        state.key_down(&down(&stroke, is_held), window, cx),
                        Ok(KeyDisposition::Consumed)
                    );
                }
                let up = KeyUpEvent { keystroke: stroke };
                assert_eq!(state.key_up(&up, window, cx), Ok(KeyDisposition::Consumed));
                let events = keys(client);
                assert_eq!(
                    events.iter().map(|key| key.action).collect::<Vec<_>>(),
                    vec![KeyAction::Press, KeyAction::Repeat, KeyAction::Release]
                );
                assert!(events.iter().all(|key| key.text.is_none()));
                assert!(events.iter().all(|key| key.key == events[0].key));
            }
        });
    });
}

#[gpui::test]
fn composing_held_keys_stay_local_until_ime_commit(cx: &mut TestAppContext) {
    with_focused(cx, |client, input, window, cx| {
        input.update(cx, |state, cx| {
            let mut down = down(&Keystroke::parse("e->e").expect("stroke"), false);
            assert_eq!(
                state.key_down(&down, window, cx),
                Ok(KeyDisposition::Platform)
            );
            state.replace_and_mark_text_in_range(None, "か", Some(1..1), window, cx);
            assert!(state.pending_key.is_none(), "preedit cancels the raw key");
            down.is_held = true;
            assert_eq!(
                state.key_down(&down, window, cx),
                Ok(KeyDisposition::Platform)
            );
            assert_eq!(state.marked_text_range(window, cx), Some(0..1));
            assert!(
                client.take_outbound().is_empty(),
                "preedit must not reach the PTY"
            );
            state.replace_text_in_range(None, "漢", window, cx);
            let events = keys(client);
            assert_eq!(events.len(), 1);
            assert_eq!(
                events[0].action,
                KeyAction::Press,
                "IME commit is not a raw repeat"
            );
            assert_eq!(events[0].text.as_deref(), Some("漢"));
            assert_eq!(state.preedit().0, "");
            state.replace_and_mark_text_in_range(None, "´", Some(1..1), window, cx);
            state.unmark_text(window, cx);
            assert!(
                client.take_outbound().is_empty(),
                "canceled dead key stays local"
            );
            assert_eq!(state.preedit().0, "");
        });
    });
}
