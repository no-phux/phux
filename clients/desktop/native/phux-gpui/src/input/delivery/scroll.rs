//! The wheel, and a selection drag that scrolls past the pane edge.

use super::*;

#[gpui::test]
fn wheel_scrolls_the_pane_under_the_pointer_and_repaints(cx: &mut TestAppContext) {
    let lines: String = (0..12).map(|line| format!("line {line}\r\n")).collect();
    let client = session_with(None, lines.as_bytes());
    let view = client.create_view(&terminal()).expect("view");
    let cx = cx.add_empty_window();
    cx.run_until_parked();
    let offset = |client: &Client| client.acquire_view(view).expect("frame").scrollbar.offset;
    let at_tail = offset(&client);
    let wheel = |pixels: f32| gpui::ScrollEvent {
        position: point(px(4.), px(4.)),
        delta: gpui::ScrollDelta::Pixels(point(px(0.), px(pixels))),
        ..Default::default()
    };
    let input = cx.update(|window, cx| {
        let input = bind(&client, view, window, cx);
        arm(&input, false, 160., 68., window, cx);
        input
    });
    let notified = Rc::new(Cell::new(0));
    cx.update(|_, cx| {
        let notified = notified.clone();
        cx.observe(&input, move |_, _| notified.set(notified.get() + 1))
            .detach();
    });

    let scroll = |cx: &mut gpui::VisualTestContext, pixels: f32| {
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                assert_eq!(state.scroll(&wheel(pixels), window, cx), Ok(true));
            });
        });
        cx.run_until_parked();
        let frame = client.acquire_view(view).expect("frame");
        let fraction = cx.update(|_, cx| input.read(cx).fraction_of(&frame));
        (at_tail - frame.scrollbar.offset, fraction)
    };
    let near = |actual: f32, expected: f32| (actual - expected).abs() < 1e-4;

    // Unfocused: half a row up from the tail shows the row above the
    // viewport, scrolled past by the other half, and repaints.
    let (back, fraction) = scroll(cx, 8.5);
    assert_eq!(back, 1);
    assert!(near(fraction, 0.5), "{fraction}");
    assert_eq!(notified.get(), 1, "the observing terminal repaints");

    // A move within the current row stays local.
    let (back, fraction) = scroll(cx, -4.25);
    assert_eq!(back, 1);
    assert!(near(fraction, 0.75), "{fraction}");
    assert_eq!(notified.get(), 2);

    // Crossing rows moves the view and keeps the remainder.
    let (back, fraction) = scroll(cx, 34.);
    assert_eq!(back, 3);
    assert!(near(fraction, 0.75), "{fraction}");

    // The live tail never shows a fraction.
    let (back, fraction) = scroll(cx, -1_000.);
    assert_eq!((back, fraction), (0, 0.));
}

/// Through the real surface: a drag that leaves the window still reaches
/// the view that captured it, extends to the edge row, and scrolls
/// history in on its own timer until released outside.
#[gpui::test]
fn a_surface_drag_past_the_window_edge_follows_and_autoscrolls(cx: &mut TestAppContext) {
    let lines: String = (0..30).map(|line| format!("line {line}\r\n")).collect();
    let client = session_with(None, lines.as_bytes());
    let view = client.create_view(&terminal()).expect("view");
    let (surface, cx) = cx.add_window_view(|window, cx| {
        crate::terminal::Terminal::new(
            client.clone(),
            view,
            terminal(),
            crate::terminal::Settings::default(),
            window,
            cx,
        )
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let bounds = cx.update(|_, cx| surface.read(cx).painted().geometry.bounds);
    assert!(bounds.size.height > px(0.), "the surface painted");
    let tail = client.acquire_view(view).expect("frame").scrollbar.offset;
    // The second row of the grid, a little inside its left edge.
    let start = bounds.origin + point(px(1.), px(24.));
    cx.simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::default());
    let outside = point(bounds.left() + px(1.), px(-40.));
    cx.simulate_mouse_move(
        outside,
        Some(gpui::MouseButton::Left),
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();
    let first = selection(&client, view);
    assert!(
        first.starts_with("line 27"),
        "the drag reached the top row: {first:?}"
    );
    for _ in 0..4 {
        cx.executor().advance_clock(crate::input::pointer::tick());
        cx.run_until_parked();
    }
    let offset = client.acquire_view(view).expect("frame").scrollbar.offset;
    assert!(offset < tail, "history scrolled in: {offset} vs {tail}");
    cx.simulate_mouse_up(outside, gpui::MouseButton::Left, gpui::Modifiers::default());
    cx.run_until_parked();
    let scrolled = offset;
    cx.executor()
        .advance_clock(crate::input::pointer::tick() * 4);
    cx.run_until_parked();
    assert_eq!(
        client.acquire_view(view).expect("frame").scrollbar.offset,
        scrolled,
        "release stops the scrolling"
    );
    let text = selection(&client, view);
    assert!(text.len() > first.len(), "{text:?} grew from {first:?}");
}

/// A drag held above the pane scrolls history in and selects it, and the
/// selection still reaches the row the drag began on.
#[gpui::test]
fn a_drag_past_the_top_edge_scrolls_and_extends_the_selection(cx: &mut TestAppContext) {
    let lines: String = (0..12).map(|line| format!("line {line}\r\n")).collect();
    let client = session_with(None, lines.as_bytes());
    let view = client.create_view(&terminal()).expect("view");
    let cx = cx.add_empty_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let at_tail = client.acquire_view(view).expect("frame").scrollbar.offset;
    let above = point(px(1.), px(-30.));
    let input = cx.update(|window, cx| {
        let input = bind(&client, view, window, cx);
        arm(&input, false, 160., 68., window, cx);
        input.update(cx, |state, cx| {
            state.focus.focus(window, cx);
            // Row 2 is "line 11".
            press_at(state, point(px(60.), px(42.)), window, cx);
            drag_to(state, above, window, cx);
            assert!(state.autoscroll.is_some(), "held past the edge");
            for _ in 0..3 {
                assert!(state.autoscroll_tick(cx));
            }
        });
        input
    });
    let offset = client.acquire_view(view).expect("frame").scrollbar.offset;
    assert_eq!(at_tail - offset, 6, "two rows a tick, a line past the edge");
    cx.update(|window, cx| {
        input.update(cx, |state, cx| {
            release_at(state, above, window, cx);
            assert!(state.autoscroll.is_none());
            assert!(!state.autoscroll_tick(cx), "the timer stops with the drag");
        });
    });
    let text = selection(&client, view);
    assert!(text.starts_with("line 3"), "{text:?}");
    assert!(text.contains("line 10\nline 11"), "{text:?}");
}
