//! One finger on the terminal: a drag scrolls and coasts, a hold selects or
//! hands a program the drag, and a tap clicks. Timed by the app clock.

use super::*;
use phux_protocol::input::key::PhysicalKey;
use phux_protocol::input::mouse::{MouseAction, MouseButton};
use phux_protocol::wire::frame::FrameKind;
use rune::Runtime;

fn tick(rt: &mut Runtime<Desk>, clock: f64) {
    rt.update(|d, tx| {
        d.poll(clock, tx);
    });
    rt.frame(1.0 / 60.0);
}

/// A point a few cells into the focused terminal's grid.
fn in_grid(rt: &Runtime<Desk>) -> (f32, f32) {
    let [x, y] = rt.state.origins[&id(1)];
    (x + 30.0, y + 60.0)
}

fn offset(client: &phux_client_runtime::Client, rt: &Runtime<Desk>) -> u64 {
    let view = rt.state.shown.as_ref().unwrap().view.unwrap();
    client.acquire_view(view).unwrap().scrollbar.offset
}

/// A finger down at `at`, dragged `dy` over two polls a frame apart, then
/// lifted at `lift` on the clock and timed by one more poll.
fn swipe(rt: &mut Runtime<Desk>, (x, y): (f32, f32), dy: f32, lift: f64) {
    touch(rt, Started, x, y);
    tick(rt, 1.0);
    touch(rt, Moved, x, y + dy / 2.0);
    tick(rt, 1.016);
    touch(rt, Moved, x, y + dy);
    tick(rt, 1.032);
    touch(rt, Ended, x, y + dy);
    tick(rt, lift);
}

fn coasting(rt: &Runtime<Desk>) -> bool {
    rt.state.phone.as_ref().unwrap().fling.is_some()
}

fn mice(frames: &[FrameKind]) -> Vec<(MouseAction, MouseButton)> {
    frames
        .iter()
        .filter_map(|f| match f {
            FrameKind::InputMouse { event, .. } => Some((event.action, event.button)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_drag_scrolls_history_and_a_quick_lift_coasts_on_then_stops() {
    let (client, mut rt) = fixture(true);
    let tail = offset(&client, &rt);
    let at = in_grid(&rt);
    swipe(&mut rt, at, 80.0, 1.04);
    let lifted = offset(&client, &rt);
    assert!(lifted < tail, "a finger moving down reveals older rows");
    assert!(coasting(&rt), "a quick lift coasts");
    let mut clock = 1.04;
    while coasting(&rt) && clock < 6.0 {
        clock += 0.016;
        tick(&mut rt, clock);
    }
    assert!(!coasting(&rt), "friction or the top of history ends it");
    assert!(
        offset(&client, &rt) + 3 < lifted,
        "the coast carried on past the lift: {} after {lifted}",
        offset(&client, &rt)
    );
    assert!(rt.state.phone.as_ref().unwrap().touch.is_none());
    no_input(&client);
}

#[test]
fn a_finger_that_rests_before_lifting_throws_nothing() {
    let (client, mut rt) = fixture(true);
    let at = in_grid(&rt);
    swipe(&mut rt, at, 80.0, 1.6);
    let lifted = offset(&client, &rt);
    assert!(!coasting(&rt));
    tick(&mut rt, 1.7);
    assert_eq!(offset(&client, &rt), lifted);
    no_input(&client);
}

#[test]
fn a_touch_that_catches_a_coast_stops_it_and_is_not_a_tap() {
    let (client, mut rt) = fixture(true);
    rt.update(|d, _| d.phone.as_mut().unwrap().keyboard = false);
    let at = in_grid(&rt);
    swipe(&mut rt, at, 80.0, 1.04);
    assert!(coasting(&rt));
    touch(&mut rt, Started, at.0, at.1);
    assert!(!coasting(&rt));
    let caught = offset(&client, &rt);
    touch(&mut rt, Ended, at.0, at.1);
    tick(&mut rt, 1.2);
    assert_eq!(offset(&client, &rt), caught);
    assert!(
        !rt.state.phone.as_ref().unwrap().keyboard,
        "catching a coast is not a tap"
    );
    no_input(&client);
}

#[test]
fn a_drag_is_a_pagers_arrows_and_a_tracking_programs_wheel_never_its_button() {
    let (client, mut rt) = fixture(true);
    output(&client, 1, 1, b"\x1b[?1049h");
    poll(&mut rt);
    let at = in_grid(&rt);
    swipe(&mut rt, at, 60.0, 1.6);
    let frames = outbound(&client);
    assert!(frames.iter().any(
        |f| matches!(f, FrameKind::InputKey { event, .. } if event.key == PhysicalKey::ArrowUp)
    ));
    assert!(mice(&frames).is_empty());
    output(&client, 1, 2, b"\x1b[?1002h\x1b[?1006h");
    poll(&mut rt);
    let at = in_grid(&rt);
    swipe(&mut rt, at, 60.0, 1.6);
    let frames = outbound(&client);
    let sent = mice(&frames);
    assert!(!sent.is_empty());
    assert!(
        sent.iter()
            .all(|step| *step == (MouseAction::Press, MouseButton::Four)),
        "a plain drag scrolls the program: {sent:?}"
    );
    assert!(
        !frames
            .iter()
            .any(|f| matches!(f, FrameKind::InputKey { .. } | FrameKind::InputPaste { .. }))
    );
    assert_eq!(focus(&rt), id(1));
    assert_eq!(rt.state.phone.as_ref().unwrap().travel, [0.0; 2]);
}

#[test]
fn a_hold_selects_by_words_and_lifting_copies_the_selection() {
    use crate::desk::phone::touch::Feedback;
    let (client, mut rt) = fixture(true);
    let (x, y) = in_grid(&rt);
    touch(&mut rt, Started, x, y);
    tick(&mut rt, 1.0);
    tick(&mut rt, 1.2);
    let view = rt.state.shown.as_ref().unwrap().view.unwrap();
    let engine = client.engine().unwrap();
    assert!(
        engine.view_selection_text(view).is_err(),
        "not held long enough yet"
    );
    tick(&mut rt, 1.4);
    assert_eq!(
        rt.state.phone.as_mut().unwrap().feedback.take(),
        Some(Feedback::Hold)
    );
    let word = engine.view_selection_text(view).unwrap();
    assert!(
        !word.is_empty(),
        "the hold selects the word under the finger"
    );
    let tail = offset(&client, &rt);
    touch(&mut rt, Moved, x + 90.0, y + 17.0);
    touch(&mut rt, Ended, x + 90.0, y + 17.0);
    let selected = engine.view_selection_text(view).unwrap();
    assert!(selected.len() > word.len(), "the drag extended it");
    assert_eq!(offset(&client, &rt), tail, "a held finger does not scroll");
    let copied = rt.copy_request.take().expect("lifting copies");
    assert_eq!(copied.content().text().as_bytes(), selected.as_slice());
    assert_eq!(
        rt.state.phone.as_ref().unwrap().feedback,
        Some(Feedback::Copied)
    );
    no_input(&client);
}

#[test]
fn a_hold_hands_a_tracking_program_its_drag_unless_shift_is_latched() {
    let (client, mut rt) = fixture(true);
    output(&client, 1, 1, b"\x1b[?1002h\x1b[?1006h");
    poll(&mut rt);
    let (x, y) = in_grid(&rt);
    let hold_and_drag = |rt: &mut Runtime<Desk>| {
        touch(rt, Started, x, y);
        tick(rt, 1.0);
        tick(rt, 1.4);
        touch(rt, Moved, x + 90.0, y + 17.0);
        touch(rt, Ended, x + 90.0, y + 17.0);
    };
    hold_and_drag(&mut rt);
    let sent = mice(&outbound(&client));
    assert_eq!(sent.first(), Some(&(MouseAction::Press, MouseButton::Left)));
    assert!(sent.contains(&(MouseAction::Motion, MouseButton::Left)));
    assert_eq!(
        sent.last(),
        Some(&(MouseAction::Release, MouseButton::Left))
    );
    assert!(rt.copy_request.is_none());
    // ⇧ keeps the selection local, as Shift does on a desktop.
    rt.update(|d, _| d.phone.as_mut().unwrap().shift = true);
    hold_and_drag(&mut rt);
    assert!(mice(&outbound(&client)).is_empty());
    assert!(rt.copy_request.take().is_some());
}

#[test]
fn a_tap_brings_the_keyboard_back_or_clicks_a_tracking_program() {
    let (client, mut rt) = fixture(true);
    rt.update(|d, _| d.phone.as_mut().unwrap().keyboard = false);
    let (x, y) = in_grid(&rt);
    touch(&mut rt, Started, x, y);
    touch(&mut rt, Ended, x, y);
    assert!(rt.state.phone.as_ref().unwrap().keyboard);
    no_input(&client);

    output(&client, 1, 1, b"\x1b[?1000h\x1b[?1006h");
    poll(&mut rt);
    rt.update(|d, _| d.phone.as_mut().unwrap().keyboard = false);
    touch(&mut rt, Started, x, y);
    touch(&mut rt, Ended, x, y);
    assert_eq!(
        mice(&outbound(&client)),
        [
            (MouseAction::Press, MouseButton::Left),
            (MouseAction::Release, MouseButton::Left)
        ]
    );
    assert!(
        !rt.state.phone.as_ref().unwrap().keyboard,
        "a click in a program is not a request to type"
    );
}
