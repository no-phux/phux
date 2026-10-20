//! Watching `Global` metadata keys: subscribe and read on request, again on
//! every attach, drain values newest-per-key, and keep live change frames
//! for every other consumer.

use super::*;
use phux_client_runtime::control::{GlobalMetadata, MAX_WATCHED_GLOBAL_KEYS};
use phux_protocol::wire::frame::Scope;

const KEY: &str = "phux.usage/v1";

fn outbound(plane: &mut ControlPlane) -> Vec<FrameKind> {
    plane
        .take_outbound()
        .iter()
        .map(|bytes| decode(bytes))
        .collect()
}

/// The request id of the `GET_METADATA` for `key`, after asserting the
/// subscription went out first.
fn watch_read(frames: &[FrameKind], key: &str) -> u32 {
    let subscribed = frames.iter().position(|frame| {
        matches!(frame, FrameKind::SubscribeMetadata { scope: Scope::Global, key: k } if k == key)
    });
    let (read_at, request_id) = frames
        .iter()
        .enumerate()
        .find_map(|(at, frame)| match frame {
            FrameKind::GetMetadata {
                request_id,
                scope: Scope::Global,
                key: k,
            } if k == key => Some((at, *request_id)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no GET of {key}: {frames:?}"));
    assert!(
        subscribed.is_some_and(|at| at < read_at),
        "subscribe before reading so no change falls between: {frames:?}"
    );
    request_id
}

fn attached() -> ControlPlane {
    let (mut plane, attach_id) = negotiated();
    plane
        .feed(FrameKind::Attached {
            attach_id,
            snapshot: two_session_snapshot(false),
            initial_client_id: ClientId::new(1),
        })
        .unwrap();
    let _ = plane.take_outbound();
    let _ = plane.take_events();
    plane
}

fn value(key: &str, bytes: Option<&[u8]>) -> GlobalMetadata {
    GlobalMetadata {
        key: key.to_owned(),
        value: bytes.map(<[u8]>::to_vec),
    }
}

#[test]
fn a_watch_reads_the_value_and_follows_its_changes() {
    let mut plane = attached();
    let request_id = plane.watch_global_metadata(KEY.to_owned());
    assert_ne!(request_id, 0);
    assert_eq!(watch_read(&outbound(&mut plane), KEY), request_id);
    assert!(
        plane.take_global_metadata().is_empty(),
        "nothing before the reply"
    );

    plane
        .feed(FrameKind::MetadataValue {
            request_id,
            value: Some(b"{\"a\":1}".to_vec()),
        })
        .unwrap();
    assert_eq!(
        plane.take_global_metadata(),
        [value(KEY, Some(b"{\"a\":1}"))]
    );
    assert!(
        plane
            .take_events()
            .iter()
            .all(|event| !matches!(event, Event::Frame(_))),
        "the watch's own read reply is consumed"
    );

    plane
        .feed(FrameKind::MetadataChanged {
            scope: Scope::Global,
            key: KEY.to_owned(),
            value: None,
            actor: None,
        })
        .unwrap();
    plane
        .feed(FrameKind::MetadataChanged {
            scope: Scope::Global,
            key: "phux.other/v1".to_owned(),
            value: Some(b"x".to_vec()),
            actor: None,
        })
        .unwrap();
    assert_eq!(
        plane.take_global_metadata(),
        [value(KEY, None)],
        "a delete is a None value"
    );
    let raw_changes = plane
        .take_events()
        .into_iter()
        .filter(|event| matches!(event, Event::Frame(frame) if matches!(**frame, FrameKind::MetadataChanged { .. })))
        .count();
    assert_eq!(raw_changes, 2, "live changes stay visible to raw consumers");
}

#[test]
fn changes_coalesce_to_the_newest_value_per_key() {
    let mut plane = attached();
    let usage = plane.watch_global_metadata(KEY.to_owned());
    let other = plane.watch_global_metadata("phux.other/v1".to_owned());
    let _ = plane.take_outbound();
    plane
        .feed(FrameKind::MetadataValue {
            request_id: usage,
            value: Some(b"1".to_vec()),
        })
        .unwrap();
    plane
        .feed(FrameKind::MetadataValue {
            request_id: other,
            value: None,
        })
        .unwrap();
    plane
        .feed(FrameKind::MetadataChanged {
            scope: Scope::Global,
            key: KEY.to_owned(),
            value: Some(b"2".to_vec()),
            actor: None,
        })
        .unwrap();
    assert_eq!(
        plane.take_global_metadata(),
        [value("phux.other/v1", None), value(KEY, Some(b"2"))]
    );
}

#[test]
fn a_watch_before_the_handshake_is_issued_at_attach_and_on_every_reattach() {
    let mut plane = ControlPlane::new(ControlOptions {
        attach: Some(AttachTarget::ByName("main".to_owned())),
        viewport: (20, 4),
        ..ControlOptions::default()
    });
    assert_eq!(
        plane.watch_global_metadata(KEY.to_owned()),
        0,
        "offline: nothing sent"
    );
    assert_eq!(plane.watch_global_metadata(String::new()), 0);
    plane.connection_opened();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let attach_id = outbound(&mut plane)
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Attach { attach_id, .. } => Some(*attach_id),
            _ => None,
        })
        .expect("ATTACH");
    plane
        .feed(FrameKind::Attached {
            attach_id,
            snapshot: two_session_snapshot(false),
            initial_client_id: ClientId::new(1),
        })
        .unwrap();
    let first = watch_read(&outbound(&mut plane), KEY);

    // The connection drops; its subscriptions end with it. The reconnect's
    // attach subscribes and reads again, and the old read is forgotten.
    plane.connection_lost(Some("gone".to_owned()));
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let attach_id = outbound(&mut plane)
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Attach { attach_id, .. } => Some(*attach_id),
            _ => None,
        })
        .expect("second ATTACH");
    plane
        .feed(FrameKind::Attached {
            attach_id,
            snapshot: two_session_snapshot(false),
            initial_client_id: ClientId::new(1),
        })
        .unwrap();
    let second = watch_read(&outbound(&mut plane), KEY);
    assert_ne!(first, second);
    plane
        .feed(FrameKind::MetadataValue {
            request_id: first,
            value: Some(b"old".to_vec()),
        })
        .unwrap();
    assert!(
        plane.take_global_metadata().is_empty(),
        "a forgotten read is not this watch's"
    );
    plane
        .feed(FrameKind::MetadataValue {
            request_id: second,
            value: Some(b"new".to_vec()),
        })
        .unwrap();
    assert_eq!(plane.take_global_metadata(), [value(KEY, Some(b"new"))]);
}

#[test]
fn a_refused_read_ends_quietly_and_the_key_budget_is_bounded() {
    let mut plane = attached();
    let request_id = plane.watch_global_metadata(KEY.to_owned());
    let _ = plane.take_outbound();
    plane
        .feed(FrameKind::Error {
            request_id: Some(request_id),
            code: ErrorCode::PermissionDenied,
            message: "denied".into(),
        })
        .unwrap();
    assert!(plane.take_global_metadata().is_empty());
    assert!(
        plane
            .take_events()
            .iter()
            .all(|event| !matches!(event, Event::ServerError { .. })),
        "a refused watch read is not a session error"
    );

    for n in 1..MAX_WATCHED_GLOBAL_KEYS {
        assert_ne!(plane.watch_global_metadata(format!("k/{n}")), 0);
    }
    assert_eq!(plane.watch_global_metadata("one/too/many".to_owned()), 0);
    assert_ne!(
        plane.watch_global_metadata(KEY.to_owned()),
        0,
        "re-watching a known key never counts against the budget"
    );
}
