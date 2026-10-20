//! Refusals, detaches, unconsumed frames, and upload replay across reconnects.

use super::*;

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

    // A refused HELLO (workload-auth §7) stops retrying like a violation.
    let (mut plane, _) = negotiated();
    assert!(matches!(
        plane.feed(FrameKind::Detached {
            reason: Some(DetachReason::AuthenticationFailed),
            message: "policy denied".to_owned(),
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
