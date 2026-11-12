//! The session order every client lists sessions in.
//!
//! A person drags sessions into it; it is kept in the [`SESSION_ORDER_KEY`]
//! Global metadata key. Sessions the stored list does not name follow it, in
//! whatever order the client already falls back to.

use phux_protocol::ids::SessionKey;

/// A JSON array of session keys, first on top.
pub const SESSION_ORDER_KEY: &str = "phux.session-order/v1";

/// The stored order; empty when the key is absent or unreadable.
#[must_use]
pub fn decode(value: Option<&[u8]>) -> Vec<SessionKey> {
    value
        .and_then(|bytes| serde_json::from_slice(bytes).ok())
        .unwrap_or_default()
}

/// `order` as the key's value.
#[must_use]
pub fn encode(order: &[SessionKey]) -> Vec<u8> {
    serde_json::to_vec(order).unwrap_or_default()
}

/// Where a session sorts: its place in `order`, and after every named
/// session when unnamed or keyless. Compare ranks first, then the fallback.
#[must_use]
pub fn rank(order: &[SessionKey], key: Option<SessionKey>) -> usize {
    key.and_then(|key| order.iter().position(|named| *named == key))
        .unwrap_or(usize::MAX)
}

/// Sort `items` by `order`, stably, so the unnamed keep their current order.
pub fn sort<T>(items: &mut [T], order: &[SessionKey], key: impl Fn(&T) -> Option<SessionKey>) {
    items.sort_by_key(|item| rank(order, key(item)));
}

/// Whether sessions list most recently used first (`[sidebar] recent-first`).
// ponytail: one flag for the process, set when the config loads; the lists
// that sort are built far from any config.
static RECENT_FIRST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// List sessions most recently used first, or in the stored order.
pub fn set_recent_first(on: bool) {
    RECENT_FIRST.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Put `sessions` in the order they are listed in: the stored order, or,
/// when recent-first is on, the one used last on top.
pub fn arrange(sessions: &mut [&phux_protocol::wire::info::SessionInfo], order: &[SessionKey]) {
    let recent = RECENT_FIRST.load(std::sync::atomic::Ordering::Relaxed);
    arrange_by(sessions, order, recent);
}

fn arrange_by(
    sessions: &mut [&phux_protocol::wire::info::SessionInfo],
    order: &[SessionKey],
    recent_first: bool,
) {
    if recent_first {
        // Never used sorts last; the sort is stable, so those keep their order.
        sessions.sort_by_key(|session| std::cmp::Reverse(session.touched));
    } else {
        sort(sessions, order, |session| session.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> SessionKey {
        SessionKey::new([n; 16]).expect("nonzero")
    }

    #[test]
    fn named_sessions_lead_in_stored_order_and_the_rest_keep_theirs() {
        let order = decode(Some(&encode(&[key(3), key(1)])));
        assert_eq!(order, vec![key(3), key(1)]);
        let mut sessions = vec![
            ("a", Some(key(1))),
            ("b", None),
            ("c", Some(key(2))),
            ("d", Some(key(3))),
        ];
        sort(&mut sessions, &order, |s| s.1);
        let names: Vec<_> = sessions.iter().map(|s| s.0).collect();
        assert_eq!(names, ["d", "a", "b", "c"]);
        assert_eq!(rank(&order, Some(key(9))), usize::MAX);
        assert!(decode(None).is_empty());
        assert!(decode(Some(b"not json")).is_empty());
    }

    #[test]
    fn recent_first_puts_the_session_used_last_on_top() {
        use phux_protocol::ids::SessionId;
        use phux_protocol::wire::info::SessionInfo;
        let session = |id, touched| {
            SessionInfo::new(SessionId::new(id), format!("s{id}")).with_touched(touched)
        };
        let all = [
            session(1, Some(4)),
            session(2, None),
            session(3, Some(9)),
            session(4, None),
        ];
        let mut listed: Vec<&SessionInfo> = all.iter().collect();
        arrange_by(&mut listed, &[], true);
        let names: Vec<&str> = listed.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["s3", "s1", "s2", "s4"]);
        arrange_by(&mut listed, &[], false);
        let names: Vec<&str> = listed.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            ["s3", "s1", "s2", "s4"],
            "no stored order: left as it was"
        );
    }
}
