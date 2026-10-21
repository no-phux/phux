//! Session switches and spawns: what is attached, replayed, or sized.

use super::*;

#[test]
fn live_session_switch_uses_the_same_socket_and_preserves_home_pumps() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"home");
    let _ = plane.take_events();
    let _ = plane.take_outbound();
    refresh_to(&mut plane, two_session_snapshot(false));

    let _ = plane.take_outbound(); // Metadata discovery belongs to the refresh.
    assert!(!plane.attach_session(AttachTarget::ByName("beta".to_owned())));
    assert_eq!(plane.attached_session(), Some(1));
    assert_eq!(plane.selected_session(), Some(2));
    let switched = plane
        .take_outbound()
        .into_iter()
        .map(|frame| decode(&frame))
        .collect::<Vec<_>>();
    assert_eq!(switched.len(), 2, "one attach and its required resize");
    assert!(matches!(
        &switched[0],
        FrameKind::Command {
            command: Command::AttachResource { terminal_id, .. },
            ..
        } if *terminal_id == ResourceId::local(8)
    ));
    assert!(matches!(
        &switched[1],
        FrameKind::ResizeTerminal { terminal_id, .. }
            if *terminal_id == ResourceId::local(8)
    ));

    assert!(!plane.attach_session(AttachTarget::ByName("main".to_owned())));
    assert_eq!(plane.selected_session(), Some(1));
    let home = plane
        .take_outbound()
        .into_iter()
        .map(|frame| decode(&frame))
        .collect::<Vec<_>>();
    assert_eq!(home.len(), 1, "home already rides the session pumps");
    assert!(matches!(
        &home[0],
        FrameKind::Command {
            command: Command::DetachResource { terminal_id },
            ..
        } if *terminal_id == ResourceId::local(8)
    ));
}

#[test]
fn a_picked_up_foreign_pane_is_not_attached_again_on_session_switch() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"home");
    let _ = plane.take_events();
    let _ = plane.take_outbound();
    refresh_to(&mut plane, two_session_snapshot(false));
    let _ = plane.take_outbound();

    plane
        .feed(FrameKind::Event {
            terminal: Some(ResourceId::local(8)),
            event: phux_protocol::wire::frame::AgentEvent::ResourceSpawned {
                kind: phux_protocol::ResourceKind::Terminal,
                parent: None,
            },
            stamp: None,
        })
        .expect("foreign spawn event");
    let pickup = plane.take_outbound();
    assert_eq!(
        pickup
            .iter()
            .map(|frame| decode(frame))
            .filter(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: Command::AttachResource { terminal_id, .. },
                    ..
                } if *terminal_id == ResourceId::local(8)
            ))
            .count(),
        1
    );

    assert!(!plane.attach_session(AttachTarget::ByName("beta".to_owned())));
    assert!(
        plane
            .take_outbound()
            .iter()
            .map(|frame| decode(frame))
            .all(|frame| !matches!(
                frame,
                FrameKind::Command {
                    command: Command::AttachResource { terminal_id, .. },
                    ..
                } if terminal_id == ResourceId::local(8)
            )),
        "session switch must reuse the existing foreign stream"
    );
}

#[test]
fn own_spawns_are_never_attached_a_second_time() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"home");
    let _ = plane.take_events();
    let _ = plane.take_outbound();
    let request_id = plane.spawn_terminal(SpawnRequest::default());
    let _ = plane.take_outbound();
    plane
        .feed(FrameKind::ResourceSpawned {
            request_id,
            result: SpawnResult::Ok(ResourceId::local(9)),
        })
        .expect("own spawn reply");
    let _ = plane.take_outbound();
    refresh_to(&mut plane, two_session_snapshot(true));

    assert!(!plane.attach_session(AttachTarget::ByName("beta".to_owned())));
    let frames = plane
        .take_outbound()
        .into_iter()
        .map(|frame| decode(&frame))
        .collect::<Vec<_>>();
    assert!(frames.iter().any(|frame| matches!(
        frame,
        FrameKind::Command {
            command: Command::AttachResource { terminal_id, .. },
            ..
        } if *terminal_id == ResourceId::local(8)
    )));
    assert!(!frames.iter().any(|frame| match frame {
        FrameKind::Command {
            command: Command::AttachResource { terminal_id, .. },
            ..
        }
        | FrameKind::ResizeTerminal { terminal_id, .. } => {
            *terminal_id == ResourceId::local(9)
        }
        _ => false,
    }));
    assert_eq!(
        plane.ensure_stream(&ResourceId::local(9)),
        StreamRecovery::Reconnect
    );
    assert!(plane.take_outbound().is_empty());
}

#[test]
fn keyed_spawn_replays_after_lost_reply_with_the_same_key_and_correlation() {
    let mut plane = ControlPlane::new(ControlOptions {
        attach: Some(AttachTarget::ByName("main".to_owned())),
        ..ControlOptions::default()
    });
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane
        .feed(hello_ok_with(&[ServerFeature::SpawnIdempotency]))
        .unwrap();
    let first_attach = outbound_attach_id(&mut plane);
    attach(&mut plane, first_attach, b"home");
    let _ = plane.take_outbound();
    let _ = plane.take_events();

    let request_id = plane.spawn_terminal(SpawnRequest::default());
    let sent = plane.take_outbound();
    assert_eq!(sent.len(), 1);
    let FrameKind::SpawnResource {
        resource: Some(resource),
        ..
    } = decode(&sent[0])
    else {
        panic!("spawn must carry a resource key");
    };
    assert!(resource.idempotency_key.is_some());
    plane.connection_lost(Some("socket closed after send".to_owned()));
    plane.connection_opened();
    let _ = plane.take_outbound();
    assert!(!plane.take_events().iter().any(|event| matches!(event,
        Event::TerminalSpawned { request_id: id, .. } if *id == request_id
    )));
    plane
        .feed(hello_ok_with(&[ServerFeature::SpawnIdempotency]))
        .unwrap();
    finish_reconnected_attach(&mut plane);
    let replayed = plane.take_outbound();
    assert!(replayed.iter().any(|frame| *frame == sent[0]));
    plane
        .feed(FrameKind::ResourceSpawned {
            request_id,
            result: SpawnResult::Replayed {
                id: ResourceId::local(9),
                instance: None,
            },
        })
        .unwrap();
    assert!(plane.take_events().iter().any(|event| matches!(event,
        Event::TerminalSpawned { request_id: id, terminal_id: Some(id_value), error: None }
            if *id == request_id && *id_value == ResourceId::local(9)
    )));
}

#[test]
fn unkeyed_spawn_is_not_retried_after_a_lost_connection() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"home");
    let _ = plane.take_outbound();
    let _ = plane.take_events();
    let request_id = plane.spawn_terminal(SpawnRequest::default());
    let sent = plane.take_outbound();
    assert!(matches!(
        decode(&sent[0]),
        FrameKind::SpawnResource { resource: None, .. }
    ));
    plane.connection_lost(Some("socket closed".to_owned()));
    plane.connection_opened();
    assert!(plane.take_events().iter().any(|event| matches!(event,
        Event::TerminalSpawned { request_id: id, terminal_id: None, error: Some(_) }
            if *id == request_id
    )));
    assert!(!plane.take_outbound().iter().any(|frame| *frame == sent[0]));
}

#[test]
fn a_connection_without_a_geometry_vote_never_sizes_terminals_implicitly() {
    let mut plane = ControlPlane::new(ControlOptions {
        attach: Some(AttachTarget::ByName("main".to_owned())),
        viewport: (0, 0),
        ..ControlOptions::default()
    });
    assert!(!plane.votes_on_geometry());
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane
        .feed(hello_ok(PROTOCOL_VERSION.patch))
        .expect("HELLO_OK");
    let attach = plane
        .take_outbound()
        .into_iter()
        .map(|frame| decode(&frame))
        .find_map(|frame| match frame {
            FrameKind::Attach {
                attach_id,
                viewport,
                ..
            } => Some((attach_id, viewport)),
            _ => None,
        })
        .expect("ATTACH");
    assert_eq!((attach.1.cols, attach.1.rows), (0, 0));
    attach_with_history(&mut plane, attach.0, b"home", None);
    let _ = plane.take_events();
    let _ = plane.take_outbound();

    let _ = plane.spawn_terminal(SpawnRequest::default());
    let spawned = plane
        .take_outbound()
        .into_iter()
        .map(|frame| decode(&frame))
        .find_map(|frame| match frame {
            FrameKind::SpawnResource { initial_size, .. } => Some(initial_size),
            _ => None,
        })
        .expect("SPAWN_RESOURCE");
    assert_eq!(spawned, None, "the server's default size applies");

    refresh_to(&mut plane, two_session_snapshot(false));
    let _ = plane.take_outbound();
    assert_ne!(plane.attach_terminal(&ResourceId::local(8)), 0);
    assert!(
        !plane
            .take_outbound()
            .into_iter()
            .any(|frame| matches!(decode(&frame), FrameKind::ResizeTerminal { .. })),
        "a foreign attach is not reflowed to a viewport",
    );
}
