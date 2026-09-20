//! The control plane driven sans-IO: frames fed directly, the way Cockpit
//! drives `phux-client-ffi`, with no socket and no driver.

#![allow(clippy::expect_used, reason = "test assertions")]
#![allow(clippy::unwrap_used, reason = "test assertions")]
#![allow(clippy::panic, reason = "test assertions")]

use phux_client_runtime::control::{
    ControlError, ControlOptions, ControlPlane, Event, FileUploadOutcome, SpawnRequest, Status,
    StreamRecovery,
};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, Layer, LayerSet, ServerCapabilities,
    ServerFeature, ServerFeatureSet,
};
use phux_protocol::ids::{BootstrapId, ClientId, ResourceId, SessionId, StreamId, WindowId};
use phux_protocol::wire::frame::{
    AttachTarget, Command, CommandResult, CommandValue, DetachReason, ErrorCode, FrameKind,
    SpawnResult,
};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

const fn terminal() -> ResourceId {
    ResourceId::local(7)
}

fn hello_ok(patch: u16) -> FrameKind {
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: patch,
        server_caps: ServerCapabilities::new()
            .with_layers(LayerSet::with(&[Layer::L3]))
            .with_features(ServerFeatureSet::new()),
        server_id: vec![0xAB; 16],
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::default(),
    }
}

fn hello_ok_with(features: &[ServerFeature]) -> FrameKind {
    let FrameKind::HelloOk {
        protocol_major,
        protocol_minor,
        protocol_patch,
        server_caps,
        server_id,
        selected_profile,
        bootstrap_limits,
    } = hello_ok(PROTOCOL_VERSION.patch)
    else {
        unreachable!()
    };
    FrameKind::HelloOk {
        protocol_major,
        protocol_minor,
        protocol_patch,
        server_caps: server_caps.with_features(ServerFeatureSet::with(features)),
        server_id,
        selected_profile,
        bootstrap_limits,
    }
}

fn snapshot() -> SessionSnapshot {
    SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal())
        .with_sessions(vec![SessionInfo::new(SessionId::new(1), "main")])
        .with_windows(vec![WindowInfo::new(
            WindowId::new(1),
            SessionId::new(1),
            "shell",
        )])
        .with_resources(vec![ResourceInfo::new(terminal(), WindowId::new(1), 20, 4)])
}

fn two_session_snapshot(include_own_spawn: bool) -> SessionSnapshot {
    let main = SessionId::new(1);
    let beta = SessionId::new(2);
    let main_window = WindowId::new(1);
    let beta_window = WindowId::new(2);
    let mut resources = vec![
        ResourceInfo::new(terminal(), main_window, 20, 4),
        ResourceInfo::new(ResourceId::local(8), beta_window, 20, 4),
    ];
    if include_own_spawn {
        resources.push(ResourceInfo::new(ResourceId::local(9), beta_window, 20, 4));
    }
    SessionSnapshot::new(main, main_window, terminal())
        .with_sessions(vec![
            SessionInfo::new(main, "main"),
            SessionInfo::new(beta, "beta"),
        ])
        .with_windows(vec![
            WindowInfo::new(main_window, main, "shell"),
            WindowInfo::new(beta_window, beta, "other"),
        ])
        .with_resources(resources)
}

fn decode(frame: &[u8]) -> FrameKind {
    let (decoded, rest) = FrameKind::decode(frame).expect("outbound frame decodes");
    assert!(rest.is_empty());
    decoded
}

/// A plane past `HELLO_OK`, with the attach id its `ATTACH` carried.
fn negotiated() -> (ControlPlane, u32) {
    let mut plane = ControlPlane::new(ControlOptions {
        attach: Some(AttachTarget::ByName("main".to_owned())),
        viewport: (20, 4),
        ..ControlOptions::default()
    });
    assert_eq!(plane.status(), Status::Idle);
    plane.connection_opened();
    let opening = plane.take_outbound();
    assert_eq!(opening.len(), 1);
    assert!(matches!(decode(&opening[0]), FrameKind::Hello { .. }));
    assert_eq!(plane.status(), Status::Connecting);

    plane
        .feed(hello_ok(PROTOCOL_VERSION.patch))
        .expect("HELLO_OK");
    assert_eq!(plane.status(), Status::Negotiated);
    let followups = plane.take_outbound();
    assert_eq!(followups.len(), 2, "SUBSCRIBE_EVENTS then ATTACH");
    assert!(matches!(
        decode(&followups[0]),
        FrameKind::SubscribeEvents {
            terminal: None,
            after_seq: None
        }
    ));
    let FrameKind::Attach {
        attach_id, target, ..
    } = decode(&followups[1])
    else {
        panic!("second follow-up is ATTACH");
    };
    assert_eq!(target, AttachTarget::ByName("main".to_owned()));
    (plane, attach_id)
}

fn attach(plane: &mut ControlPlane, attach_id: u32, bytes: &[u8]) {
    plane
        .feed(FrameKind::Attached {
            attach_id,
            snapshot: snapshot(),
            initial_client_id: ClientId::new(1),
        })
        .expect("ATTACHED");
    let stream_id = StreamId::new(1).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    plane
        .feed(FrameKind::BootstrapBegin {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 20,
            rows: 4,
            base_seq: 0,
        })
        .expect("BOOTSTRAP_BEGIN");
    plane
        .feed(FrameKind::BootstrapChunk {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: bytes.to_vec().into(),
        })
        .expect("BOOTSTRAP_CHUNK");
    plane
        .feed(FrameKind::BootstrapReady {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        })
        .expect("BOOTSTRAP_READY");
    plane
        .feed(FrameKind::AttachReady { attach_id })
        .expect("ATTACH_READY");
}

fn refresh_to(plane: &mut ControlPlane, snapshot: SessionSnapshot) {
    let request_id = plane.refresh_topology().expect("negotiated topology read");
    let _ = plane.take_outbound();
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::State(snapshot)),
        })
        .expect("GET_STATE reply");
    let _ = plane.take_events();
}

fn output(plane: &mut ControlPlane, seq: u64, bytes: &[u8]) {
    plane
        .feed(FrameKind::ResourceOutput {
            terminal_id: terminal(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq,
            bytes: bytes.to_vec().into(),
        })
        .expect("RESOURCE_OUTPUT");
}

#[cfg(feature = "engine")]
fn observed_text(plane: &ControlPlane) -> String {
    plane
        .publication()
        .acquire(&terminal())
        .expect("published frame")
        .text()
}

#[cfg(not(feature = "engine"))]
fn observed_text(plane: &ControlPlane) -> String {
    String::from_utf8_lossy(&plane.engine().expect("engine").take_output(&terminal())).into_owned()
}

#[test]
fn a_fed_attach_publishes_the_terminal_and_input_goes_out_as_frames() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"ready");
    assert_eq!(plane.status(), Status::Attached);
    assert!(plane.attached_once());
    let topology = plane.topology().expect("topology");
    assert_eq!(topology.panes.len(), 1);
    assert_eq!(topology.panes[0].session_name, "main");
    assert_eq!(plane.attached_session(), Some(1));

    let events = plane.take_events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::TerminalChanged { terminal_id } if *terminal_id == terminal())),
        "the bootstrap changed the terminal: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::TopologyChanged))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Attached { attach_id: id } if *id == attach_id))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::StatusChanged(Status::Attached)))
    );
    assert!(observed_text(&plane).contains("ready"));

    #[cfg(feature = "engine")]
    {
        let first = plane.publication().acquire(&terminal()).expect("frame");
        assert_eq!(first.generation, 1);
        assert_eq!(plane.publication().generation(&terminal()), Some(1));
        output(&mut plane, 1, b"\x1b[3;1Hthird");
        let second = plane.publication().acquire(&terminal()).expect("frame");
        assert_eq!(second.generation, 2);
        assert_eq!(second.row_text(2), "third");
        assert!(second.is_row_dirty(2));
        assert_eq!(first.text(), "ready", "the held frame never changes");
    }
    #[cfg(not(feature = "engine"))]
    {
        output(&mut plane, 1, b"more");
        assert_eq!(observed_text(&plane), "more");
    }
    assert!(plane.input_ready(&terminal()));
    // The kernel re-subscribes to events when the attach barrier releases.
    let released = plane.take_outbound();
    assert!(
        released
            .iter()
            .any(|frame| matches!(decode(frame), FrameKind::SubscribeEvents { .. }))
    );

    assert!(plane.send_text(&terminal(), "hi"));
    let keys = plane.take_outbound();
    assert_eq!(keys.len(), 2);
    assert!(matches!(decode(&keys[0]), FrameKind::InputKey { .. }));

    let request_id = plane.spawn_terminal(SpawnRequest {
        command: Some(vec!["/bin/cat".to_owned()]),
        ..SpawnRequest::default()
    });
    let spawn = plane.take_outbound();
    assert!(matches!(
        decode(&spawn[0]),
        FrameKind::SpawnResource { request_id: id, .. } if id == request_id
    ));
    plane
        .feed(FrameKind::ResourceSpawned {
            request_id,
            result: SpawnResult::Ok(ResourceId::local(9)),
        })
        .expect("RESOURCE_SPAWNED");
    let events = plane.take_events();
    assert!(events.iter().any(|event| matches!(
        event,
        Event::TerminalSpawned { request_id: id, terminal_id: Some(spawned), error: None }
            if *id == request_id && *spawned == ResourceId::local(9)
    )));
    let refresh = plane.take_outbound();
    assert!(matches!(
        decode(&refresh[0]),
        FrameKind::Command {
            command: Command::GetState { .. },
            ..
        }
    ));
}

#[test]
fn live_session_switch_uses_the_same_socket_and_preserves_home_pumps() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"home");
    let _ = plane.take_events();
    let _ = plane.take_outbound();
    refresh_to(&mut plane, two_session_snapshot(false));

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
fn refusals_are_terminal_and_a_requested_detach_closes() {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    plane.take_outbound();
    assert!(matches!(
        plane.feed(hello_ok(PROTOCOL_VERSION.patch.wrapping_add(1))),
        Err(ControlError::Refused(_))
    ));

    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    assert!(matches!(
        plane.feed(FrameKind::AttachReady { attach_id: 1 }),
        Err(ControlError::Protocol(_))
    ));

    let (mut plane, _) = negotiated();
    assert!(matches!(
        plane.feed(FrameKind::Detached {
            reason: Some(DetachReason::ProtocolError),
            message: "bad frame".to_owned(),
        }),
        Err(ControlError::Refused(_))
    ));

    let (mut plane, _) = negotiated();
    assert!(matches!(
        plane.feed(FrameKind::Error {
            request_id: None,
            code: ErrorCode::VersionIncompatible,
            message: String::new(),
        }),
        Err(ControlError::Refused(_))
    ));
    assert!(plane.take_events().iter().any(|event| matches!(
        event,
        Event::ServerError {
            code: ErrorCode::VersionIncompatible,
            ..
        }
    )));

    let (mut plane, _) = negotiated();
    plane.detach();
    assert!(matches!(
        decode(&plane.take_outbound()[0]),
        FrameKind::Detach
    ));
    assert!(matches!(
        plane.feed(FrameKind::Detached {
            reason: Some(DetachReason::Requested),
            message: String::new(),
        }),
        Err(ControlError::Closed)
    ));
    assert_eq!(plane.status(), Status::Closed);
}

#[test]
fn frames_the_plane_does_not_consume_reach_a_binding_and_commands_correlate() {
    let (mut plane, _) = negotiated();
    plane
        .feed(FrameKind::MetadataValue {
            request_id: 5,
            value: None,
        })
        .expect("unconsumed frame");
    let events = plane.take_events();
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Frame(frame) if matches!(**frame, FrameKind::MetadataValue { request_id: 5, .. })
    )));

    let request_id = plane.send_command(Command::GetState {
        scope: phux_protocol::wire::frame::StateScope::Server,
    });
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: phux_protocol::wire::frame::CommandResult::Ok,
        })
        .expect("reply");
    assert!(plane.take_events().iter().any(|event| matches!(
        event,
        Event::CommandResult { request_id: id, .. } if *id == request_id
    )));
}

#[test]
fn file_upload_replays_the_same_chunk_after_a_reconnect() {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    plane.take_outbound();
    plane
        .feed(hello_ok_with(&[ServerFeature::FileUpload]))
        .expect("HELLO_OK");
    plane.take_outbound();

    let transfer_id = plane.put_file(terminal(), "png".to_owned(), b"image".to_vec());
    let first = plane.take_outbound();
    let FrameKind::Command {
        request_id: first_request,
        command:
            Command::PutFile {
                upload_id,
                offset,
                data,
                final_chunk,
                ..
            },
    } = decode(&first[0])
    else {
        panic!("upload command");
    };
    assert_eq!(offset, 0);
    assert_eq!(data, b"image");
    assert!(final_chunk);

    plane.connection_lost(Some("reset".to_owned()));
    plane.connection_opened();
    plane.take_outbound();
    plane
        .feed(hello_ok_with(&[ServerFeature::FileUpload]))
        .expect("replacement HELLO_OK");
    let replay = plane.take_outbound();
    let FrameKind::Command {
        request_id: replay_request,
        command:
            Command::PutFile {
                upload_id: replay_id,
                offset: replay_offset,
                data: replay_data,
                ..
            },
    } = decode(replay.last().expect("replayed upload"))
    else {
        panic!("replayed upload command");
    };
    assert_ne!(first_request, replay_request);
    assert_eq!(upload_id, replay_id, "retry-stable secret upload id");
    assert_eq!(replay_offset, 0);
    assert_eq!(replay_data, b"image");

    plane
        .feed(FrameKind::CommandResult {
            request_id: replay_request,
            result: CommandResult::OkWith(CommandValue::FileUpload(
                phux_protocol::wire::frame::FileUploadAck {
                    next_offset: 5,
                    path: Some("/tmp/image.png".to_owned()),
                },
            )),
        })
        .expect("upload ack");
    assert_eq!(
        plane.take_file_upload_receipts(),
        vec![phux_client_runtime::control::FileUploadReceipt {
            transfer_id,
            outcome: FileUploadOutcome::Completed,
            path: Some("/tmp/image.png".to_owned()),
            code: None,
            message: String::new(),
        }]
    );
}
