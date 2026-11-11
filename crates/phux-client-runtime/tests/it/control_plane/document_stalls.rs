//! A stalled document is recovered on its own: the plane attaches that
//! resource again and never drops the connection, and after the kernel's
//! re-attach budget it reports the document lost.

use super::*;

const MIS_TAGGED_RUN: u64 = 64;

const fn surface() -> ResourceId {
    ResourceId::local(30)
}

const fn generation(raw: u64) -> (StreamId, BootstrapId) {
    (StreamId::new(3).unwrap(), BootstrapId::new(raw).unwrap())
}

/// A plane past `HELLO_OK` with `surface()` attached and one document
/// published at bootstrap 1.
fn published_surface() -> ControlPlane {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let request_id = plane.attach_terminal(&surface());
    assert_ne!(request_id, 0);
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        })
        .unwrap();
    let (stream_id, bootstrap_id) = generation(1);
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: surface(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::PixelSurfacePngV1,
            cols: 0,
            rows: 0,
            base_seq: 1,
        },
        FrameKind::BootstrapReady {
            terminal_id: surface(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ] {
        plane.feed(frame).unwrap();
    }
    assert!(
        plane
            .take_events()
            .iter()
            .any(|event| matches!(event, Event::DocumentPublished { .. }))
    );
    let _ = plane.take_outbound();
    plane
}

/// Frames that all name a generation the document is not on: the kernel
/// treats a long enough run as a stall.
fn feed_mis_tagged_run(plane: &mut ControlPlane, first_seq: u64) {
    let (stream_id, other) = generation(9);
    for seq in first_seq..first_seq + MIS_TAGGED_RUN {
        let fed = plane.feed(FrameKind::ResourceOutput {
            terminal_id: surface(),
            stream_id,
            bootstrap_id: other,
            seq,
            bytes: bytes::Bytes::from_static(b"mis-tagged"),
        });
        // Dropped and reported to a strict feeder; never a protocol error.
        assert!(
            matches!(fed, Ok(()) | Err(ControlError::InvalidState(_))),
            "{fed:?}"
        );
    }
}

fn reattaches(plane: &mut ControlPlane) -> usize {
    plane
        .take_outbound()
        .iter()
        .filter(|bytes| {
            matches!(
                decode(bytes),
                FrameKind::Command {
                    command: Command::AttachResource { terminal_id, .. },
                    ..
                } if terminal_id == surface()
            )
        })
        .count()
}

fn answer_reattach(plane: &mut ControlPlane) -> usize {
    let attaches: Vec<u32> = plane
        .take_outbound()
        .iter()
        .filter_map(|bytes| match decode(bytes) {
            FrameKind::Command {
                request_id,
                command: Command::AttachResource { terminal_id, .. },
            } if terminal_id == surface() => Some(request_id),
            _ => None,
        })
        .collect();
    for request_id in &attaches {
        plane
            .feed(FrameKind::CommandResult {
                request_id: *request_id,
                result: CommandResult::Ok,
            })
            .unwrap();
    }
    attaches.len()
}

#[test]
fn a_stalled_document_is_attached_again_instead_of_reconnecting() {
    let mut plane = published_surface();
    feed_mis_tagged_run(&mut plane, 10);
    assert_eq!(
        reattaches(&mut plane),
        1,
        "only that resource is attached again"
    );
    let events = plane.take_events();
    assert!(events.iter().any(|event| matches!(
        event,
        Event::DocumentStalled { terminal_id, attempt: 1, .. } if *terminal_id == surface()
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::ResyncRequired { .. })),
        "a document stall is no connection resync"
    );
    assert_ne!(plane.status(), Status::Connecting);
}

#[test]
fn a_document_that_keeps_stalling_is_reported_lost() {
    let mut plane = published_surface();
    let mut seq = 10;
    for attempt in 1..=3 {
        feed_mis_tagged_run(&mut plane, seq);
        seq += MIS_TAGGED_RUN;
        assert_eq!(answer_reattach(&mut plane), 1, "attempt {attempt}");
        assert!(plane.take_events().iter().any(|event| matches!(
            event,
            Event::DocumentStalled { attempt: reported, .. } if *reported == attempt
        )));
    }
    feed_mis_tagged_run(&mut plane, seq);
    assert_eq!(reattaches(&mut plane), 0, "the re-attach budget is spent");
    assert!(plane.take_events().iter().any(|event| matches!(
        event,
        Event::DocumentLost { terminal_id, .. } if *terminal_id == surface()
    )));
    assert_ne!(plane.status(), Status::Connecting);
}
