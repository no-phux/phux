//! Wheel hit ancestry through the real floating Sessions dock.
use super::*;

use crate::ui::element;

#[track_caller]
fn near(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 0.5,
        "expected {expected:.2}, got {actual:.2}"
    );
}

fn first_row_y(rt: &Runtime<Desk>) -> f32 {
    element(rt, "phone-session-1").rect[1] - element(rt, "phone-session-list").rect[1]
}

#[test]
fn the_floating_session_dock_does_not_wheel_the_list_behind_it() {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((320.0, 568.0), 2.0);
    rt.update(|d, _| d.open_phone_sessions());
    rt.settle_all();
    rt.frame(0.0);
    // Let the phone's deferred viewport refit finish before measuring wheel
    // traffic. Even its later idle frames must not send input or commands.
    rt.frame(0.51);
    rt.frame(0.0);
    let list = element(&rt, "phone-session-list").rect;
    let dock = element(&rt, "phone-session-new").rect;
    let dock_at = (dock[0] + dock[2] / 2.0, dock[1] + dock[3] / 2.0);
    assert!(
        dock_at.0 > list[0]
            && dock_at.0 < list[0] + list[2]
            && dock_at.1 > list[1]
            && dock_at.1 < list[1] + list[3],
        "the interactive sibling really overlaps the scroll viewport: {dock:?} vs {list:?}"
    );
    let first = first_row_y(&rt);
    let _ = client.take_outbound();

    // A fresh vertical wheel hits the dock, not its overlapping sibling.
    // No app on_wheel handler intercepts this: Rune must use hit ancestry.
    rt.wheel_xy(dock_at.0, dock_at.1, 0.0, -40.0);
    rt.frame(0.0);
    near(first_row_y(&rt), first);
    assert_eq!(element(&rt, "phone-session-new").rect, dock);

    // Reuse the unchanged frame and send the event API's next delta too.
    let commits = rt.stats.commits;
    rt.frame(0.0);
    assert_eq!(rt.stats.commits, commits, "the frame is cached");
    rt.wheel_event(rune::WheelEvent::new(0.0, -40.0).at(dock_at.0, dock_at.1));
    rt.frame(0.0);
    near(first_row_y(&rt), first);

    // The uncovered list remains scrollable; this also proves the fixture
    // overflows and would move if the dock wheel leaked through.
    let list_at = (list[0] + list[2] / 2.0, list[1] + 20.0);
    assert!(list_at.1 + 10.0 < dock[1]);
    rt.wheel_xy(list_at.0, list_at.1, 0.0, -40.0);
    rt.frame(0.0);
    near(first_row_y(&rt), first - 40.0);
    let commits = rt.stats.commits;
    rt.frame(0.0);
    assert_eq!(rt.stats.commits, commits, "the scrolled frame is cached");
    rt.wheel_event(rune::WheelEvent::new(0.0, -40.0).at(list_at.0, list_at.1));
    rt.frame(0.0);
    near(first_row_y(&rt), first - 80.0);

    // Moving back to the dock starts a new run, not a continuation of the
    // list's owner. It still cannot move the list from its interior offset.
    rt.frame(0.51);
    rt.wheel_xy(dock_at.0, dock_at.1, 0.0, -40.0);
    rt.frame(0.0);
    near(first_row_y(&rt), first - 80.0);
    assert_eq!(element(&rt, "phone-session-new").rect, dock);
    assert_eq!(element(&rt, "phone-session-list").rect, list);
    assert_eq!(rt.state.phone_page(), Some(Page::Sessions));
    assert!(rt.state.phone.as_ref().unwrap().query.text.is_empty());
    assert!(!rt.state.phone_keyboard_visible());
    assert!(rt.state.connection.pending_session.is_none());
    assert_eq!(rt.state.connection.sessions_seq, 0);
    let outbound: Vec<_> = client
        .take_outbound()
        .into_iter()
        .map(|bytes| FrameKind::decode(&bytes).expect("decode outbound").0)
        .collect();
    assert!(
        outbound.is_empty(),
        "wheel browsing must not type, create a session, or send commands: {outbound:?}"
    );
}
