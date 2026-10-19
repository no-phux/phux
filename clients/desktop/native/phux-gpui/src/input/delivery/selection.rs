//! Mouse selection, its repaint, and its isolation between views.

use super::*;

/// A local selection change republishes the view without a runtime
/// wake. The painter is a cached view, so the input must notify it or the
/// new frame waits for unrelated output.
#[gpui::test]
fn local_selection_notifies_the_painter(cx: &mut TestAppContext) {
    let client = session(None);
    let view = client.create_view(&terminal()).expect("view");
    let cx = cx.add_empty_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let notified = std::rc::Rc::new(std::cell::Cell::new(0));
    let input = cx.update(|window, cx| {
        let input = bind(&client, view, window, cx);
        arm(&input, false, 160., 68., window, cx);
        input.update(cx, |state, cx| state.focus.focus(window, cx));
        let count = notified.clone();
        cx.observe(&input, move |_, _| count.set(count.get() + 1))
            .detach();
        input
    });
    let at = |x: f32| point(px(x), px(8.));
    cx.update(|window, cx| {
        input.update(cx, |state, cx| {
            let down = gpui::MouseDownEvent {
                button: gpui::MouseButton::Left,
                position: at(2.),
                click_count: 1,
                ..Default::default()
            };
            assert_eq!(state.mouse_down(&down, window, cx), Ok(true));
        });
    });
    assert_eq!(notified.get(), 1, "a press starts a selection");
    cx.update(|window, cx| {
        input.update(cx, |state, cx| {
            let drag = gpui::MouseMoveEvent {
                position: at(30.),
                pressed_button: Some(gpui::MouseButton::Left),
                ..Default::default()
            };
            assert_eq!(state.mouse_move(&drag, window, cx), Ok(true));
        });
    });
    assert_eq!(notified.get(), 2, "a drag extends it");
    cx.update(|window, cx| {
        input.update(cx, |state, cx| {
            let up = gpui::MouseUpEvent {
                button: gpui::MouseButton::Left,
                position: at(30.),
                click_count: 1,
                ..Default::default()
            };
            assert_eq!(state.mouse_up(&up, window, cx), Ok(true));
        });
    });
    assert_eq!(notified.get(), 3, "a release settles it");
}

fn selected_cells(client: &Client, view: ViewId) -> usize {
    let frame = client.acquire_view(view).expect("frame");
    frame
        .buffer
        .cells
        .iter()
        .filter(|cell| cell.flags & phux_client_core::grid::CELL_SELECTED != 0)
        .count()
}

/// Press, drag, and release across the first row of the pane.
fn drag(
    input: &Entity<TerminalInput>,
    window: &mut Window,
    cx: &mut gpui::App,
) -> Result<bool, crate::input::InputError> {
    let at = |x: f32| point(px(x), px(8.));
    input.update(cx, |state, cx| {
        state.mouse_down(
            &gpui::MouseDownEvent {
                button: gpui::MouseButton::Left,
                position: at(2.),
                click_count: 1,
                ..Default::default()
            },
            window,
            cx,
        )?;
        state.mouse_move(
            &gpui::MouseMoveEvent {
                position: at(30.),
                pressed_button: Some(gpui::MouseButton::Left),
                ..Default::default()
            },
            window,
            cx,
        )?;
        state.mouse_up(
            &gpui::MouseUpEvent {
                button: gpui::MouseButton::Left,
                position: at(30.),
                click_count: 1,
                ..Default::default()
            },
            window,
            cx,
        )
    })
}

fn left_arrow() -> KeyDownEvent {
    KeyDownEvent {
        keystroke: Keystroke::parse("left").expect("stroke"),
        is_held: false,
        prefer_character_input: false,
    }
}

/// Two windows, each showing its own view of one terminal. Selection
/// and its clearing stay in the view that made them, and keys and
/// pointer gestures reach the terminal only through the focused view in
/// the active window.
#[gpui::test]
fn two_windows_keep_selection_and_input_in_their_own_view(cx: &mut TestAppContext) {
    let client = session(None);
    let left = client.create_view(&terminal()).expect("left view");
    let right = client.create_view(&terminal()).expect("right view");
    let first = cx.add_window(|_, _| gpui::Empty);
    let second = cx.add_window(|_, _| gpui::Empty);
    let open = |cx: &mut TestAppContext, handle: gpui::WindowHandle<gpui::Empty>, view| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.activate_window();
            let input = bind(&client, view, window, cx);
            arm(&input, false, 160., 68., window, cx);
            input.update(cx, |state, cx| state.focus.focus(window, cx));
            input
        })
        .expect("window")
    };
    let right_input = open(cx, second, right);
    let left_input = open(cx, first, left);
    cx.run_until_parked();
    let _ = client.take_outbound();
    let right_generation = client.view_generation(right).expect("right frame");

    // The inactive window's view refuses keys and pointer gestures, even
    // though it holds focus inside its own window.
    cx.update_window(second.into(), |_, window, cx| {
        assert!(!window.is_window_active());
        right_input.update(cx, |state, cx| {
            assert_eq!(
                state.key_down(&left_arrow(), window, cx),
                Err(crate::input::InputError::WrongFocus)
            );
        });
        assert_eq!(
            drag(&right_input, window, cx),
            Err(crate::input::InputError::WrongFocus)
        );
    })
    .unwrap();
    assert!(client.take_outbound().is_empty(), "no wrong-view input");

    // A view bound to one window never accepts that window's sibling's
    // events as its own.
    cx.update_window(first.into(), |_, window, cx| {
        right_input.update(cx, |state, cx| {
            assert_eq!(
                state.key_down(&left_arrow(), window, cx),
                Err(crate::input::InputError::WrongFocus)
            );
        });
    })
    .unwrap();
    assert!(client.take_outbound().is_empty());

    // The active window's view selects locally.
    cx.update_window(first.into(), |_, window, cx| {
        assert_eq!(drag(&left_input, window, cx), Ok(true));
    })
    .unwrap();
    let engine = client.engine().expect("engine");
    assert!(selected_cells(&client, left) > 0, "the left view selected");
    assert!(
        !engine
            .view_selection_text(left)
            .expect("left text")
            .is_empty()
    );
    assert_eq!(selected_cells(&client, right), 0);
    assert!(
        engine
            .view_selection_text(right)
            .map_or(true, |text| text.is_empty()),
        "the right view has no selection"
    );
    assert_eq!(
        client.view_generation(right),
        Some(right_generation),
        "selecting in one view never republishes the other"
    );

    // Its key goes to the terminal and, as typing does, clears only
    // this view's selection.
    cx.update_window(first.into(), |_, window, cx| {
        left_input.update(cx, |state, cx| {
            assert_eq!(
                state.key_down(&left_arrow(), window, cx),
                Ok(KeyDisposition::Consumed)
            );
        });
    })
    .unwrap();
    let sent = keys(&client);
    assert_eq!(sent.len(), 1, "one key from the focused view");
    assert_eq!(sent[0].action, KeyAction::Press);
    assert_eq!(selected_cells(&client, left), 0, "the left view cleared");
    assert_eq!(
        client.view_generation(right),
        Some(right_generation),
        "clearing one view's selection never republishes the other"
    );

    // Activating the second window moves input there and only there.
    let _ = cx.update_window(second.into(), |_, window, _| window.activate_window());
    cx.run_until_parked();
    cx.update_window(first.into(), |_, window, cx| {
        assert!(!window.is_window_active());
        left_input.update(cx, |state, cx| {
            assert_eq!(
                state.key_down(&left_arrow(), window, cx),
                Err(crate::input::InputError::WrongFocus)
            );
        });
    })
    .unwrap();
    assert!(client.take_outbound().is_empty());
    cx.update_window(second.into(), |_, window, cx| {
        assert_eq!(drag(&right_input, window, cx), Ok(true));
    })
    .unwrap();
    assert!(
        selected_cells(&client, right) > 0,
        "the right view selected"
    );
    assert_eq!(selected_cells(&client, left), 0, "the left view did not");
    cx.update_window(second.into(), |_, window, cx| {
        right_input.update(cx, |state, cx| {
            assert_eq!(
                state.key_down(&left_arrow(), window, cx),
                Ok(KeyDisposition::Consumed)
            );
        });
    })
    .unwrap();
    assert_eq!(keys(&client).len(), 1);
}

/// Moving across the far half of the last cell, without leaving it,
/// still takes that cell: the drag is not quantized to whole cells.
#[gpui::test]
fn a_drag_ending_inside_the_last_cell_selects_it(cx: &mut TestAppContext) {
    let client = session(None);
    let view = client.create_view(&terminal()).expect("view");
    let cx = cx.add_empty_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    cx.update(|window, cx| {
        let input = bind(&client, view, window, cx);
        arm(&input, false, 160., 68., window, cx);
        input.update(cx, |state, cx| {
            state.focus.focus(window, cx);
            // "ready": the y is column 4, cells are 8 px wide.
            press_at(state, point(px(1.), px(8.)), window, cx);
            drag_to(state, point(px(33.), px(8.)), window, cx);
            drag_to(state, point(px(39.), px(8.)), window, cx);
            release_at(state, point(px(39.), px(8.)), window, cx);
        });
    });
    assert_eq!(selection(&client, view), "ready");
}
