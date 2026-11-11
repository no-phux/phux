//! The session picker's sort: need-you, then the stored order, then name.

use super::*;

/// At a glance, a session with panes that need you sorts right after the
/// current one and says how many; quiet sessions keep name order.
#[test]
fn session_picker_puts_sessions_that_need_you_first() {
    let sessions = [sinfo(1, "work"), sinfo(2, "alpha"), sinfo(3, "zeta")];
    let counts = SessionCounts {
        needs_you: HashMap::from([(SessionId::new(3), 2)]),
        ..SessionCounts::default()
    };
    let items = session_picker_items(&sessions, Some(SessionId::new(1)), &counts);
    let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["work", "zeta", "alpha"]);
    assert_eq!(items[1].secondary.as_deref(), Some("2 need you"));
}

/// `phux.session-order/v1` outranks name; the unnamed follow by name.
#[test]
fn session_picker_follows_the_stored_session_order() {
    let key = |n| phux_protocol::ids::SessionKey::new([n; 16]);
    let mut sessions = [
        sinfo(1, "work"),
        sinfo(2, "alpha"),
        sinfo(3, "zeta"),
        sinfo(4, "beta"),
    ];
    sessions[2].key = key(3);
    sessions[3].key = key(4);
    let counts = SessionCounts {
        order: vec![key(3).unwrap(), key(4).unwrap()],
        ..SessionCounts::default()
    };
    let items = session_picker_items(&sessions, Some(SessionId::new(1)), &counts);
    let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["work", "zeta", "beta", "alpha"]);
}
