//! Dedicated pane gestures choose one axis; terminal gestures keep their owner.
use super::*;

const SURFACES: [&str; 2] = ["phone-navigation", "phone-minimap"];

fn origin(rt: &rune::Runtime<Desk>, key: &str) -> (f32, f32) {
    let [x, y, w, h] = geometry(rt, key).rect;
    if key == "phone-minimap" {
        (x + w / 2.0, y + h - 2.0)
    } else {
        (x + w * 0.65, y + h / 2.0)
    }
}

fn move_to(rt: &mut rune::Runtime<Desk>, origin: (f32, f32), delta: [f32; 2]) {
    let (x, y) = (origin.0 + delta[0], origin.1 + delta[1]);
    assert!(x >= 0.0 && x < rt.viewport.0 && y >= 0.0 && y < rt.viewport.1);
    touch(rt, Moved, origin.0 + delta[0], origin.1 + delta[1]);
}

#[test]
fn a_horizontal_pane_swipe_cannot_turn_vertical_or_commit_on_orthogonal_travel() {
    for key in SURFACES {
        let (client, mut rt) = fixture(true);
        let before = stored(&client);
        let origin = origin(&rt, key);
        touch(&mut rt, Started, origin.0, origin.1);
        move_to(&mut rt, origin, [-9.0, -1.0]);
        assert_eq!(rt.state.phone.as_ref().unwrap().travel, [-9.0, 0.0]);
        move_to(&mut rt, origin, [-20.0, -60.0]);
        assert_eq!(rt.state.phone.as_ref().unwrap().travel, [-20.0, 0.0]);
        touch(&mut rt, Ended, origin.0 - 20.0, origin.1 - 60.0);
        settle(&mut rt);
        assert_eq!(focus(&rt), id(1), "{key}: 20 points is not a commit");
        assert_eq!(stored(&client), before);
        no_input(&client);
    }
}

#[test]
fn a_vertical_pane_swipe_keeps_its_neighbor_when_the_finger_drifts_sideways() {
    for key in SURFACES {
        let (client, mut rt) = fixture(true);
        let before = stored(&client);
        let origin = origin(&rt, key);
        touch(&mut rt, Started, origin.0, origin.1);
        move_to(&mut rt, origin, [-1.0, -9.0]);
        move_to(&mut rt, origin, [-100.0, -60.0]);
        assert_eq!(rt.state.phone.as_ref().unwrap().travel, [0.0, -60.0]);
        touch(&mut rt, Ended, origin.0 - 100.0, origin.1 - 60.0);
        settle(&mut rt);
        assert_eq!(focus(&rt), id(2), "{key}: down, not right");
        assert_eq!(stored(&client), before);
        no_input(&client);
    }
}

#[test]
fn an_axis_lock_keeps_boundary_resistance_and_allows_same_axis_reversal() {
    for key in SURFACES {
        let (client, mut rt) = fixture(true);
        rt.update(|d, _| d.want(id(3)));
        poll(&mut rt);
        settle(&mut rt);
        let before = stored(&client);
        let origin = origin(&rt, key);
        touch(&mut rt, Started, origin.0, origin.1);
        move_to(&mut rt, origin, [-10.0, -1.0]);
        assert_eq!(rt.state.phone.as_ref().unwrap().travel, [-2.0, 0.0]);
        move_to(&mut rt, origin, [40.0, -60.0]);
        assert_eq!(rt.state.phone.as_ref().unwrap().travel, [40.0, 0.0]);
        touch(&mut rt, Ended, origin.0 + 40.0, origin.1 - 60.0);
        settle(&mut rt);
        assert_eq!(focus(&rt), id(1), "{key}: reversal goes left, not down");
        assert_eq!(stored(&client), before);
        no_input(&client);
    }
}

#[test]
fn cancelling_a_locked_pane_swipe_rearms_the_next_gestures_axis() {
    for key in SURFACES {
        let (client, mut rt) = fixture(true);
        let start = origin(&rt, key);
        touch(&mut rt, Started, start.0, start.1);
        move_to(&mut rt, start, [-80.0, -10.0]);
        touch(&mut rt, Cancelled, start.0 - 80.0, start.1 - 10.0);
        assert_eq!(focus(&rt), id(1));
        assert!(!rt.state.phone.as_ref().unwrap().navigating());
        assert_eq!(rt.state.phone.as_ref().unwrap().travel, [0.0; 2]);
        settle(&mut rt);
        let origin = origin(&rt, key);
        touch(&mut rt, Started, origin.0, origin.1);
        move_to(&mut rt, origin, [-1.0, -60.0]);
        assert_eq!(rt.state.phone.as_ref().unwrap().travel, [0.0, -60.0]);
        touch(&mut rt, Ended, origin.0 - 1.0, origin.1 - 60.0);
        settle(&mut rt);
        assert_eq!(focus(&rt), id(2));
        no_input(&client);
    }
}

#[test]
fn pane_slop_holds_still_and_a_crossed_slop_map_drag_never_becomes_a_tap() {
    for key in SURFACES {
        let (client, mut rt) = fixture(true);
        let origin = origin(&rt, key);
        touch(&mut rt, Started, origin.0, origin.1);
        for delta in [[4.0, 3.0], [0.0, -8.0]] {
            move_to(&mut rt, origin, delta);
            assert_eq!(rt.state.phone.as_ref().unwrap().travel, [0.0; 2]);
        }
        // The radial slop is crossed; an exact tie retains vertical priority.
        move_to(&mut rt, origin, [6.0, 6.0]);
        assert_eq!(rt.state.phone.as_ref().unwrap().travel, [0.0, 1.2]);
        move_to(&mut rt, origin, [0.0; 2]);
        touch(&mut rt, Ended, origin.0, origin.1);
        settle(&mut rt);
        assert_eq!(focus(&rt), id(1), "{key}: not a map tap after a drag");
        no_input(&client);
    }
}

#[test]
fn pane_commit_threshold_uses_only_the_captured_axis() {
    for key in SURFACES {
        for (travel, destination) in [(27.9, 1), (28.0, 2)] {
            let (client, mut rt) = fixture(true);
            let origin = origin(&rt, key);
            touch(&mut rt, Started, origin.0, origin.1);
            move_to(&mut rt, origin, [0.0, -9.0]);
            move_to(&mut rt, origin, [-100.0, -travel]);
            touch(&mut rt, Ended, origin.0 - 100.0, origin.1 - travel);
            settle(&mut rt);
            assert_eq!(focus(&rt), id(destination), "{key}, {travel}");
            no_input(&client);
        }
    }
}

#[test]
fn the_final_touch_move_can_capture_an_axis_without_an_explicit_move_event() {
    for key in SURFACES {
        let (client, mut rt) = fixture(true);
        let origin = origin(&rt, key);
        touch(&mut rt, Started, origin.0, origin.1);
        touch(&mut rt, Ended, origin.0 - 80.0, origin.1 - 60.0);
        settle(&mut rt);
        assert_eq!(focus(&rt), id(3), "{key}");
        assert!(!rt.state.phone.as_ref().unwrap().navigating());
        no_input(&client);
    }
}
