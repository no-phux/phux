//! Real Galaxy handlers; translated events are not native host-routing proof.
use rune::{MagnifyEvent, Mods, Runtime, WheelEvent};

use crate::desk::Desk;
use crate::galaxy::{Camera, MAX_SCALE, MIN_SCALE, Pt};

fn fixture() -> (Runtime<Desk>, Pt) {
    let (_, mut rt) = crate::test_support::input_fixture();
    rt.set_system_reduced_motion(true);
    rt.update(|d, _| d.toggle_galaxy());
    rt.settle_all();
    rt.frame(0.0);
    // The first frame queues on_layout; the second delivers its initial fit.
    rt.frame(0.0);
    let [w, h] = rt.state.galaxy.viewport;
    assert!(w > 0.0 && h > 0.0, "Galaxy's first layout was delivered");
    let camera = Camera {
        scale: 1.0,
        offset: [30.0, -15.0],
    };
    rt.update(|d, _| d.galaxy.camera = camera);
    rt.frame(0.0);
    assert_eq!(
        rt.state.galaxy.camera, camera,
        "fixture camera must survive layout before gesture delivery"
    );
    let [x, y, w, h] = crate::ui::element(&rt, "galaxy-sky").rect;
    (rt, [x + w * 0.85, y + h * 0.75])
}

fn near(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < 0.001, "{actual} != {expected}");
}

fn anchored(rt: &Runtime<Desk>, before: Camera, at: Pt, scale: f32) {
    let [x, y, _, _] = crate::ui::element(rt, "galaxy-sky").rect;
    let local = [at[0] - x, at[1] - y];
    let world = [
        (local[0] - before.offset[0]) / before.scale,
        (local[1] - before.offset[1]) / before.scale,
    ];
    let after = rt.state.galaxy.camera;
    near(after.scale, scale);
    for (actual, expected) in after.screen(world).into_iter().zip(local) {
        near(actual, expected);
    }
}

#[test]
fn wheel_derived_magnify_uses_the_same_anchored_zoom_as_a_pinch() {
    // Rune translates Ctrl-wheel to this event on every desktop platform.
    // Public Runtime input exercises our handler, not that private translation.
    for (dy, expected) in [(-20.0, 1.1), (20.0, 0.9)] {
        let (mut rt, at) = fixture();
        let before = rt.state.galaxy.camera;
        let mut event = MagnifyEvent::new(-dy / 200.0).at(at[0], at[1]);
        event.wheel = true;
        rt.magnify_event(event);
        rt.frame(0.0);
        anchored(&rt, before, at, expected);
    }
}

#[test]
fn native_pinch_keeps_its_anchor_at_zero_and_both_scale_limits() {
    for (delta, expected) in [
        (0.1, 1.1),
        (-0.1, 0.9),
        (0.0, 1.0),
        (10.0, MAX_SCALE),
        (-0.9, MIN_SCALE),
    ] {
        let (mut rt, at) = fixture();
        let before = rt.state.galaxy.camera;
        rt.magnify_event(MagnifyEvent::new(delta).at(at[0], at[1]));
        rt.frame(0.0);
        anchored(&rt, before, at, expected);
    }
}

#[test]
fn delivered_command_wheel_retains_its_distinct_zoom_policy() {
    let (mut rt, at) = fixture();
    let before = rt.state.galaxy.camera;
    rt.wheel_event(WheelEvent::new(0.0, -20.0).at(at[0], at[1]).mods(Mods {
        cmd: true,
        ..Mods::default()
    }));
    rt.frame(0.0);
    anchored(&rt, before, at, 0.8);
}

#[test]
fn unmodified_wheel_pans_without_zooming() {
    let (mut rt, at) = fixture();
    let before = rt.state.galaxy.camera;
    rt.wheel_event(WheelEvent::new(12.0, -20.0).at(at[0], at[1]));
    rt.frame(0.0);
    let after = rt.state.galaxy.camera;
    near(after.scale, before.scale);
    near(after.offset[0], before.offset[0] + 12.0);
    near(after.offset[1], before.offset[1] - 20.0);
}
