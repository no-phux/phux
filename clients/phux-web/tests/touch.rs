//! One-finger touch classification (`phux_web::touch`), under node.

use phux_web::touch::{LONG_PRESS_MS, TOUCH_SLOP_PX, TouchGesture, TouchMode, TouchStep};
use wasm_bindgen_test::wasm_bindgen_test;

const START: (f64, f64) = (100.0, 200.0);

#[wasm_bindgen_test]
fn a_finger_that_lifts_in_place_quickly_is_a_tap() {
    let mut gesture = TouchGesture::begin(7, START, 1_000.0);
    assert_eq!(gesture.pointer_id, 7);
    let jitter = (START.0 + TOUCH_SLOP_PX / 2.0, START.1);
    assert_eq!(gesture.moved(jitter, 1_050.0), TouchStep::Wait);
    assert_eq!(gesture.mode(), TouchMode::Undecided);
    assert!(gesture.is_tap(1_100.0));
    assert!(
        !gesture.is_tap(1_000.0 + LONG_PRESS_MS),
        "a held finger is not a tap"
    );
}

#[wasm_bindgen_test]
fn a_drag_past_the_slop_scrolls_one_to_one_from_where_it_landed() {
    let mut gesture = TouchGesture::begin(1, START, 0.0);
    // Up 20px: the content follows the finger, toward the live screen.
    assert_eq!(
        gesture.moved((START.0, START.1 - 20.0), 50.0),
        TouchStep::Scroll(20.0)
    );
    assert_eq!(gesture.mode(), TouchMode::Scroll);
    // Down 50px: toward history.
    assert_eq!(
        gesture.moved((START.0, START.1 + 30.0), 80.0),
        TouchStep::Scroll(-50.0)
    );
    // Once scrolling, a pause never turns it into a selection or a tap.
    assert_eq!(
        gesture.moved((START.0, START.1 + 30.0), 80.0 + LONG_PRESS_MS * 2.0),
        TouchStep::Scroll(0.0)
    );
    assert!(!gesture.is_tap(100.0));
}

#[wasm_bindgen_test]
fn a_sideways_drag_past_the_slop_scrolls_without_vertical_travel() {
    let mut gesture = TouchGesture::begin(1, START, 0.0);
    assert_eq!(
        gesture.moved((START.0 + TOUCH_SLOP_PX + 1.0, START.1), 30.0),
        TouchStep::Scroll(0.0)
    );
}

#[wasm_bindgen_test]
fn a_hold_then_drag_selects() {
    let mut gesture = TouchGesture::begin(1, START, 0.0);
    assert_eq!(gesture.moved(START, LONG_PRESS_MS - 1.0), TouchStep::Wait);
    assert_eq!(
        gesture.moved((START.0 + 1.0, START.1), LONG_PRESS_MS),
        TouchStep::StartSelect(START)
    );
    assert_eq!(gesture.mode(), TouchMode::Select);
    assert_eq!(
        gesture.moved((START.0 + 80.0, START.1 + 40.0), LONG_PRESS_MS + 100.0),
        TouchStep::ExtendSelect
    );
    assert!(!gesture.is_tap(LONG_PRESS_MS + 200.0));
}

#[wasm_bindgen_test]
fn travel_after_the_hold_selects_rather_than_scrolls() {
    let mut gesture = TouchGesture::begin(1, START, 0.0);
    assert_eq!(
        gesture.moved((START.0, START.1 + 60.0), LONG_PRESS_MS + 5.0),
        TouchStep::StartSelect(START)
    );
}
