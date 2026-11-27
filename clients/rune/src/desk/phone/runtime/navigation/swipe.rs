//! A sideways thumb on the terminal: it pages between tabs, steps over into
//! the next session past the last, and, from the left edge, pulls Sessions
//! in. Driven through real touches.

use super::*;
use crate::desk::phone::touch::Feedback;
use rune::TouchPhase::{Ended, Moved, Started};

fn scene() -> (std::sync::Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.update(|d, _| d.phone.as_mut().unwrap().keyboard = false);
    // Let the canvas spring to the laid-out stage before a finger lands.
    for _ in 0..90 {
        rt.frame(1.0 / 60.0);
    }
    let _ = client.take_outbound();
    (client, rt)
}

fn tick(rt: &mut Runtime<Desk>, clock: f64) {
    rt.update(|d, tx| {
        d.poll(clock, tx);
    });
    rt.frame(1.0 / 60.0);
}

/// Mid-stage, clear of the left edge.
fn middle(rt: &Runtime<Desk>) -> (f32, f32) {
    let stage = crate::ui::element(rt, "phone-spatial-stage").rect;
    (stage[0] + stage[2] * 0.6, stage[1] + stage[3] * 0.5)
}

/// One finger from `(x, y)` along `steps` of sideways travel a poll apart,
/// resting `rest` seconds before it lifts.
fn drag(rt: &mut Runtime<Desk>, (x, y): (f32, f32), steps: &[f32], rest: f64) -> f64 {
    let mut clock = 1.0;
    rt.touch(9, Started, x, y, clock);
    tick(rt, clock);
    for dx in steps {
        clock += 0.016;
        rt.touch(9, Moved, x + dx, y, clock);
        tick(rt, clock);
    }
    clock += rest;
    tick(rt, clock);
    let last = steps.last().copied().unwrap_or_default();
    rt.touch(9, Ended, x + last, y, clock);
    tick(rt, clock + 0.016);
    clock
}

fn swipe(rt: &Runtime<Desk>) -> &crate::desk::phone::swipe::Swipe {
    &rt.state.phone.as_ref().unwrap().swipe
}

#[test]
fn batched_moves_release_at_the_last_position_for_sessions_and_the_drawer() {
    for edge in [false, true] {
        let (client, mut rt) = scene();
        let first = rt.state.selected_session_name();
        let next = rt.state.session_beside(1);
        let (middle, y) = middle(&rt);
        let x = if edge { 4.0 } else { middle };
        let sign = if edge { 1.0 } else { -1.0 };
        rt.touch(9, Started, x, y, 1.0);
        tick(&mut rt, 1.0);
        // Both moves reach the handlers in one frame, sharing Desk::clock.
        rt.touch(9, Moved, x + sign * 20.0, y, 1.016);
        rt.touch(9, Moved, x + sign * 240.0, y, 1.020);
        rt.touch(9, Ended, x + sign * 240.0, y, 1.024);
        tick(&mut rt, 1.032);
        if edge {
            assert_eq!(rt.state.phone_page(), Some(Page::Sessions));
            assert_eq!(rt.state.selected_session_name(), first);
        } else {
            assert_eq!(rt.state.selected_session_name(), next);
            assert_ne!(rt.state.selected_session_name(), first);
        }
        no_terminal_input(&client);
    }
}

#[test]
fn batched_withdrawals_and_cancelled_swipes_do_not_navigate() {
    for edge in [false, true] {
        for cancelled in [false, true] {
            let (client, mut rt) = scene();
            let first = rt.state.selected_session_name();
            let (middle, y) = middle(&rt);
            let x = if edge { 4.0 } else { middle };
            let sign = if edge { 1.0 } else { -1.0 };
            at_batched_release(&mut rt, x, y, sign, cancelled);
            assert_eq!(rt.state.selected_session_name(), first);
            assert_eq!(rt.state.phone_page(), None);
            assert_eq!(swipe(&rt).pull, None);
            assert!(!swipe(&rt).paging);
            no_terminal_input(&client);
        }
    }
}

fn at_batched_release(rt: &mut Runtime<Desk>, x: f32, y: f32, sign: f32, cancelled: bool) {
    rt.touch(9, Started, x, y, 1.0);
    tick(rt, 1.0);
    let (first, last) = if cancelled {
        (20.0, 240.0)
    } else {
        (240.0, 20.0)
    };
    rt.touch(9, Moved, x + sign * first, y, 1.016);
    rt.touch(9, Moved, x + sign * last, y, 1.020);
    let phase = if cancelled {
        rune::TouchPhase::Cancelled
    } else {
        Ended
    };
    rt.touch(9, phase, x + sign * last, y, 1.024);
    tick(rt, 1.032);
}

#[test]
fn past_the_last_tab_a_heavier_pull_steps_over_into_the_next_session() {
    let (client, mut rt) = scene();
    let first = rt.state.selected_session_name().unwrap();
    let next = rt
        .state
        .session_beside(1)
        .expect("a session after the first");
    let (x, y) = middle(&rt);
    rt.touch(9, Started, x, y, 1.0);
    tick(&mut rt, 1.0);
    rt.touch(9, Moved, x - 60.0, y, 1.016);
    tick(&mut rt, 1.016);
    assert!(swipe(&rt).paging);
    assert!(
        (swipe(&rt).page + 36.0).abs() < 0.01,
        "{}: another session gives less than the finger travels",
        swipe(&rt).page
    );
    assert!(
        rt.find("phone-page-peek").is_some(),
        "names where it goes, quietly"
    );
    assert!(rt.find("phone-session-peek").is_none(), "no big pill");
    rt.touch(9, Moved, x - 150.0, y, 1.032);
    tick(&mut rt, 1.032);
    assert_eq!(
        rt.state.phone.as_ref().unwrap().feedback,
        None,
        "a tab's distance is not enough to leave the session"
    );
    rt.touch(9, Moved, x - 240.0, y, 1.048);
    tick(&mut rt, 1.048);
    assert_eq!(
        rt.state.phone.as_mut().unwrap().feedback.take(),
        Some(Feedback::Threshold),
        "a firmer detent where lifting crosses over"
    );
    rt.touch(9, Ended, x - 240.0, y, 1.3);
    tick(&mut rt, 1.3);
    assert_eq!(rt.state.selected_session_name().as_deref(), Some(&*next));
    assert_ne!(first, next);
    assert!(!swipe(&rt).paging);
    assert_eq!(swipe(&rt).arrival, Some(-1.0));
    assert!(rt.find("phone-page-peek").is_none());
    no_terminal_input(&client);
}

#[test]
fn a_short_slow_drag_springs_home_and_a_flick_pages() {
    let (client, mut rt) = scene();
    let first = rt.state.selected_session_name().unwrap();
    let at = middle(&rt);
    drag(&mut rt, at, &[-30.0, -60.0, -90.0], 0.5);
    assert_eq!(rt.state.selected_session_name(), Some(first.clone()));
    assert_eq!(swipe(&rt).page, 0.0);
    // Thrown, it is still too short to leave the session.
    drag(&mut rt, at, &[-30.0, -60.0, -90.0], 0.0);
    assert_eq!(rt.state.selected_session_name(), Some(first.clone()));
    // A throw a tab's length crosses over.
    drag(&mut rt, at, &[-50.0, -100.0, -150.0], 0.0);
    assert_ne!(rt.state.selected_session_name(), Some(first));
    no_terminal_input(&client);
}

#[test]
fn the_first_session_gives_like_a_rubber_band_and_stays() {
    let (client, mut rt) = scene();
    let first = rt.state.selected_session_name().unwrap();
    assert_eq!(
        rt.state.session_beside(-1),
        None,
        "the fixture starts first"
    );
    let (x, y) = middle(&rt);
    rt.touch(9, Started, x, y, 1.0);
    tick(&mut rt, 1.0);
    rt.touch(9, Moved, x + 200.0, y, 1.016);
    tick(&mut rt, 1.016);
    let page = swipe(&rt).page;
    assert!(page > 0.0 && page < 120.0, "{page}: resists past the end");
    assert!(rt.find("phone-session-peek").is_none());
    assert_eq!(rt.state.phone.as_ref().unwrap().feedback, None);
    rt.touch(9, Ended, x + 200.0, y, 1.032);
    tick(&mut rt, 1.032);
    assert_eq!(rt.state.selected_session_name(), Some(first));
    assert_eq!(swipe(&rt).page, 0.0);
    no_terminal_input(&client);
}

#[test]
fn the_left_edge_pulls_the_sessions_drawer_in_over_the_terminal() {
    let (client, mut rt) = scene();
    let first = rt.state.selected_session_name().unwrap();
    let (_, y) = middle(&rt);
    rt.touch(9, Started, 4.0, y, 1.0);
    tick(&mut rt, 1.0);
    rt.touch(9, Moved, 64.0, y, 1.016);
    tick(&mut rt, 1.016);
    assert_eq!(swipe(&rt).pull, Some(60.0));
    assert!(!swipe(&rt).paging, "the edge pulls, it does not page");
    let list = crate::ui::element(&rt, "phone-navigation-page");
    assert!(list.inert, "a drawer in the finger's hand takes no taps");
    let shown = 60.0 / rt.viewport.0;
    assert!(
        list.rect[0] < -1.0 && list.rect[0] > -80.0,
        "{list:?} drifts in behind the finger, a short way"
    );
    // The terminal frosts over in place as the finger pulls: the glass
    // never slides, it thickens.
    let frost = crate::ui::element(&rt, "phone-drawer-frost");
    assert_eq!(frost.rect[0], 0.0, "{frost:?} stays put");
    assert!(list.opacity < shown, "{list:?} sharpens after the glass");
    let thin = frost.fill.a;
    assert_eq!(rt.state.phone_page(), None, "not open until lifted");
    rt.touch(9, Moved, 204.0, y, 1.032);
    tick(&mut rt, 1.032);
    let thick = crate::ui::element(&rt, "phone-drawer-frost").fill.a;
    assert!(thin > 0.0 && thin < thick, "{thin} thickens to {thick}");
    assert_eq!(
        rt.state.phone.as_mut().unwrap().feedback.take(),
        Some(Feedback::Detent)
    );
    rt.touch(9, Ended, 204.0, y, 1.3);
    tick(&mut rt, 1.3);
    assert_eq!(rt.state.phone_page(), Some(Page::Sessions));
    assert_eq!(swipe(&rt).pull, None);
    assert!(!crate::ui::element(&rt, "phone-navigation-page").inert);
    assert_eq!(rt.state.selected_session_name(), Some(first));
    no_terminal_input(&client);
}

#[test]
fn a_short_edge_pull_slides_back_and_an_edge_scroll_still_scrolls() {
    let (client, mut rt) = scene();
    let (_, y) = middle(&rt);
    drag(&mut rt, (4.0, y), &[20.0, 40.0, 60.0], 0.5);
    assert_eq!(rt.state.phone_page(), None);
    assert_eq!(swipe(&rt).pull, None);
    // Down from the edge is a scroll, not the drawer.
    rt.touch(9, Started, 4.0, y, 3.0);
    tick(&mut rt, 3.0);
    rt.touch(9, Moved, 10.0, y + 80.0, 3.016);
    tick(&mut rt, 3.016);
    assert_eq!(swipe(&rt).pull, None);
    rt.touch(9, Ended, 10.0, y + 80.0, 3.032);
    tick(&mut rt, 3.5);
    assert_eq!(rt.state.phone_page(), None);
    no_terminal_input(&client);
}

#[test]
fn a_reconnect_retires_page_and_drawer_touches_but_a_new_finger_still_works() {
    for edge in [false, true] {
        let (client, mut rt) = scene();
        let first = rt.state.selected_session_name();
        let (middle, y) = middle(&rt);
        let x = if edge { 4.0 } else { middle };
        let dx = if edge { 200.0 } else { -200.0 };
        rt.touch(9, Started, x, y, 1.0);
        tick(&mut rt, 1.0);
        rt.touch(9, Moved, x + dx, y, 1.016);
        tick(&mut rt, 1.016);
        assert!(swipe(&rt).paging || swipe(&rt).pull.is_some());
        rt.update(|d, _| assert!(d.reopen_after_reconnect(&client)));
        tick(&mut rt, 1.1);
        rt.touch(9, Moved, x + dx, y, 1.2);
        rt.touch(9, Ended, x + dx, y, 1.3);
        tick(&mut rt, 1.3);
        assert_eq!(rt.state.selected_session_name(), first, "old page touch");
        assert_eq!(rt.state.phone_page(), None, "old drawer touch");
        assert_eq!(swipe(&rt), &Default::default());
        assert!(rt.state.connection.pending_session.is_none());

        // The cancelled page springs home; the next finger lands on its
        // settled terminal, not on the gap exposed by that animation.
        for _ in 0..90 {
            rt.frame(1.0 / 60.0);
        }
        rt.touch(10, Started, x, y, 2.0);
        tick(&mut rt, 2.0);
        rt.touch(10, Moved, x + dx, y, 2.016);
        tick(&mut rt, 2.016);
        rt.touch(10, Ended, x + dx, y, 2.3);
        tick(&mut rt, 2.3);
        if edge {
            assert_eq!(rt.state.phone_page(), Some(Page::Sessions));
        } else {
            assert_ne!(rt.state.selected_session_name(), first);
        }
        no_terminal_input(&client);
    }
}

#[test]
fn a_failed_page_landing_rebuilds_the_idle_stage_from_the_poll_alone() {
    let (_client, mut rt) = scene();
    let since = rt.clock;
    rt.update(|d, tx| {
        d.clock = since;
        let key = d.page_key();
        let phone = d.phone.as_mut().unwrap();
        phone.swipe.arrival = Some(-1.0);
        phone.swipe.leaving = Some((key, since));
        tx.instant();
    });
    rt.frame(0.0);
    let key = format!("phone-page-{}", rt.state.page_key());
    let stage = crate::ui::element(&rt, "phone-spatial-stage").rect;
    assert!(crate::ui::element(&rt, &key).rect[0] < stage[0] - 100.0);
    // This is the shared poll result Desk::poll consumes. No rt.update,
    // input, cursor blink or client news may mask a lost invalidation.
    rt.poll = Some(|d, clock, tx| {
        d.clock = clock;
        d.phone_touch_tick(tx)
    });
    for _ in 0..180 {
        rt.frame(1.0 / 60.0);
    }
    assert!(swipe(&rt).leaving.is_none(), "the deadline cleared state");
    let page = crate::ui::element(&rt, &key).rect;
    assert!((page[0] - stage[0]).abs() < 1.0, "{page:?} returns onstage");
}

#[test]
fn return_is_pinned_beside_the_keyboard_and_sends_enter() {
    let (client, mut rt) = scene();
    let enter = crate::ui::element(&rt, "phone-key-Enter");
    let strip = crate::ui::element(&rt, "phone-strip");
    assert!(
        !enter.path.starts_with(&format!("{}/", strip.path)),
        "pinned"
    );
    let toggle = crate::ui::element(&rt, "phone-keyboard-toggle");
    assert!(enter.rect[0] > toggle.rect[0], "under the right thumb");
    assert!(enter.rect[2] >= 44.0 && enter.rect[3] >= 44.0);
    assert!(
        rt.find("phone-latch-Shift").is_some(),
        "local selection override"
    );
    tap(&mut rt, "phone-key-Enter", 1.0);
    let keys: Vec<_> = client
        .take_outbound()
        .iter()
        .filter_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
            FrameKind::InputKey { event, .. } => Some(event.key),
            _ => None,
        })
        .collect();
    assert_eq!(
        keys,
        [phux_protocol::input::key::PhysicalKey::Enter],
        "one Return"
    );
}

#[test]
fn closing_sessions_fades_the_frost_and_the_list_without_flattening_the_glass() {
    let (_client, mut rt) = scene();
    let (_, y) = middle(&rt);
    drag(&mut rt, (4.0, y), &[60.0, 140.0, 220.0], 1.0);
    tick(&mut rt, 3.0);
    assert_eq!(rt.state.phone_page(), Some(Page::Sessions));
    assert_eq!(crate::ui::glass_panels(&rt), 1, "open: one full glass");
    rt.update(|d, _| d.close_phone_navigation());
    tick(&mut rt, 3.05);
    for e in rt.elements() {
        if let Some(k @ ("phone-drawer" | "phone-drawer-frost" | "phone-navigation-page")) =
            e.key.as_deref()
        {
            eprintln!(
                "{k}: exiting {} opacity {} rect {:?}",
                e.exiting, e.opacity, e.rect
            );
        }
    }
    tick(&mut rt, 4.0);
    assert!(rt.find("phone-drawer-frost").is_none(), "gone once settled");
}
