//! Application-owned drags survive Rune's automatic long press. Exercise the
//! real touch route, including its runtime clock, not just Desk's hold timer.

use super::*;
use phux_protocol::input::key::PhysicalKey;
use phux_protocol::wire::frame::FrameKind;

#[test]
fn navigation_handles_keep_their_drag_after_a_stationary_long_hold() {
    for key in ["phone-navigation", "phone-minimap"] {
        let (client, mut rt) = fixture(true);
        let arrangement = stored(&client);
        let (x, y) = rt.find(key).unwrap();
        touch(&mut rt, Started, x, y);
        rt.frame(0.65);
        touch(&mut rt, Moved, x - 60.0, y);
        assert_eq!(focus(&rt), id(1), "no commit before release: {key}");
        assert!(rt.state.phone.as_ref().unwrap().travel[0] < -40.0);
        touch(&mut rt, Ended, x - 60.0, y);
        assert_eq!(focus(&rt), id(3), "held drag reaches neighbor: {key}");
        assert!(!rt.state.phone.as_ref().unwrap().navigating());
        assert!(rt.state.pointer.menu.is_none());
        assert_eq!(stored(&client), arrangement);
        no_input(&client);
    }
}

#[test]
fn a_cancelled_held_navigation_drag_never_commits_a_pane() {
    for key in ["phone-navigation", "phone-minimap"] {
        let (client, mut rt) = fixture(true);
        let (x, y) = rt.find(key).unwrap();
        touch(&mut rt, Started, x, y);
        rt.frame(0.65);
        touch(&mut rt, Moved, x - 60.0, y);
        touch(&mut rt, Cancelled, x - 60.0, y);
        assert_eq!(focus(&rt), id(1), "cancelled: {key}");
        assert_eq!(rt.state.phone.as_ref().unwrap().travel, [0.0; 2]);
        assert!(!rt.state.phone.as_ref().unwrap().navigating());
        assert!(rt.state.pointer.menu.is_none());
        no_input(&client);
    }
}

#[test]
fn a_held_arrow_pad_still_sends_each_step_once_and_stops_on_cancel() {
    let (client, mut rt) = fixture(true);
    let (x, y) = rt.find("phone-pad").unwrap();
    touch(&mut rt, Started, x, y);
    rt.frame(0.65);
    touch(&mut rt, Moved, x - 32.0, y);
    touch(&mut rt, Moved, x - 32.0, y);
    touch(&mut rt, Cancelled, x - 32.0, y);
    rt.frame(0.65);
    let keys: Vec<_> = outbound(&client)
        .into_iter()
        .filter_map(|frame| match frame {
            FrameKind::InputKey { event, .. } => Some(event.key),
            FrameKind::ResizeTerminal { .. } => None,
            other => panic!("pad sent unexpected input: {other:?}"),
        })
        .collect();
    assert_eq!(keys, [PhysicalKey::ArrowLeft; 2]);
    assert_eq!(focus(&rt), id(1));
    assert!(rt.state.pointer.menu.is_none());
    assert!(!rt.touching());
}
