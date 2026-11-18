//! The visual bell never flashes more than once a second, and never under
//! reduce motion (design spec §1.11 "Flash", audit row 28).

use phux_web::bell::{BellGate, MIN_INTERVAL_MS, Ring, wash_ms};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn ten_bels_in_one_second_wash_once() {
    let mut gate = BellGate::default();
    let washes = (0..10)
        .map(|i| gate.ring(f64::from(i) * 99.0, false))
        .filter(|ring| *ring == Ring::Wash)
        .count();
    assert_eq!(washes, 1);
    assert_eq!(
        gate.ring(MIN_INTERVAL_MS, false),
        Ring::Wash,
        "a second later"
    );
}

#[wasm_bindgen_test]
fn reduce_motion_shows_the_dot_and_never_washes() {
    let mut gate = BellGate::default();
    for i in 0..10 {
        assert_eq!(gate.ring(f64::from(i) * 500.0, true), Ring::Dot);
    }
    assert!(!gate.washing(0.0));
}

#[wasm_bindgen_test]
fn a_wash_ends_after_its_duration() {
    let mut gate = BellGate::default();
    gate.ring(1_000.0, false);
    assert!(gate.washing(1_000.0 + f64::from(wash_ms()) - 1.0));
    assert!(!gate.washing(1_000.0 + f64::from(wash_ms())));
}
