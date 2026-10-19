//! Settled PTY resizes and the visible-owner claim.

use super::*;

#[gpui::test]
fn a_hidden_owners_delayed_fit_does_not_override_the_focused_view(cx: &mut TestAppContext) {
    let client = session(None);
    let older = client.create_view(&terminal()).expect("hidden view");
    let focused = client.create_view(&terminal()).expect("focused view");
    cx.add_empty_window().update(|window, cx| {
        // Press, tab switch, and zoom all end here: the newly focused
        // writable view fits, and the previous owner is no longer painted.
        let hidden = bind(&client, older, window, cx);
        let shown = bind(&client, focused, window, cx);
        arm(&hidden, true, 800., 408., window, cx);
        arm(&shown, true, 400., 204., window, cx);
        settle(&hidden, cx);
        assert!(
            resizes(&client).is_empty(),
            "a delayed fit from the hidden owner must not resize"
        );
        settle(&shown, cx);
        assert_eq!(resizes(&client), vec![(50, 12)]);

        // Returning to the first view settles that view's geometry.
        arm(&hidden, true, 800., 408., window, cx);
        settle(&hidden, cx);
        assert_eq!(resizes(&client), vec![(100, 24)]);

        // A later paint that is no longer the owner drops its timer.
        arm(&hidden, true, 560., 272., window, cx);
        arm(&hidden, false, 560., 272., window, cx);
        settle(&hidden, cx);
        assert!(resizes(&client).is_empty());
    });
}

#[gpui::test]
fn blur_of_an_inactive_window_still_settles_the_visible_owner(cx: &mut TestAppContext) {
    let client = session(None);
    let view = client.create_view(&terminal()).expect("view");
    cx.add_empty_window().update(|window, cx| {
        let input = bind(&client, view, window, cx);
        arm(&input, true, 800., 408., window, cx);
        window.blur(cx);
        assert!(
            !window.is_window_active(),
            "occlusion and blur keep the visible owner"
        );
        input.update(cx, |state, _| {
            state.flush_fit();
            assert!(state.settling.is_some(), "the clock has not settled");
        });
        assert!(resizes(&client).is_empty());
        settle(&input, cx);
        assert_eq!(resizes(&client), vec![(100, 24)]);
    });
}

#[gpui::test]
fn a_resize_burst_sends_only_the_settled_proposal(cx: &mut TestAppContext) {
    let client = session(None);
    let view = client.create_view(&terminal()).expect("view");
    cx.add_empty_window().update(|window, cx| {
        let input = bind(&client, view, window, cx);
        arm(&input, true, 800., 408., window, cx);
        arm(&input, true, 640., 340., window, cx);
        input.update(cx, |state, _| state.flush_fit());
        assert!(resizes(&client).is_empty());
        settle(&input, cx);
        assert_eq!(resizes(&client), vec![(80, 20)]);
    });
}

#[gpui::test]
fn reconnect_drops_the_hidden_fit_until_the_new_generation_is_shown(cx: &mut TestAppContext) {
    let client = session(None);
    let view = client.create_view(&terminal()).expect("view");
    cx.add_empty_window().update(|window, cx| {
        let stale = bind(&client, view, window, cx);
        arm(&stale, true, 800., 408., window, cx);
        open_session(&client, 2);
        settle(&stale, cx);
        assert!(
            resizes(&client).is_empty(),
            "the pre-reconnect proposal is a replaced generation"
        );
        let view = client
            .create_view(&terminal())
            .expect("view after reconnect");
        let fresh = bind(&client, view, window, cx);
        arm(&fresh, true, 800., 408., window, cx);
        settle(&fresh, cx);
        assert_eq!(resizes(&client), vec![(100, 24)]);
    });
}

#[gpui::test]
fn a_viewer_is_not_resized(cx: &mut TestAppContext) {
    let client = session(Some(RolePolicy::VIEWER));
    let view = client.create_view(&terminal()).expect("view");
    cx.add_empty_window().update(|window, cx| {
        let input = bind(&client, view, window, cx);
        arm(&input, true, 800., 408., window, cx);
        settle(&input, cx);
        assert!(resizes(&client).is_empty());
    });
}

#[gpui::test]
fn a_settled_fit_does_not_acknowledge_an_unknown_delivery_fence(cx: &mut TestAppContext) {
    let client = session(None);
    let view = client.create_view(&terminal()).expect("view");
    let armed = fence(&client).expect("fence");
    cx.add_empty_window().update(|window, cx| {
        let input = bind(&client, view, window, cx);
        arm(&input, true, 800., 408., window, cx);
        settle(&input, cx);
        assert_eq!(resizes(&client), vec![(100, 24)]);
        let current = client.with_control(|control| control.projection_fence(&terminal()));
        assert_eq!(current, Some(armed));
    });
}
