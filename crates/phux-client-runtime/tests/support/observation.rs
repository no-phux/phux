use super::*;
use phux_client_runtime::control::{EVENT_QUEUE_CAP, Observation};

/// Drop the transport and complete a full reconnect to a server that hands
/// back the same session, window and terminal ids.
fn reconnect_with_reused_ids(plane: &mut ControlPlane) {
    let epoch = plane.connection_epoch();
    plane.connection_lost(Some("transport dropped".to_owned()));
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let attach_id = plane
        .take_outbound()
        .iter()
        .find_map(|bytes| match decode(bytes) {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .expect("the reconnect attaches");
    reattach(plane, attach_id, epoch + 1);
    let _ = plane.take_outbound();
}

/// `attach`, with the fresh bootstrap generation a new connection carries.
fn reattach(plane: &mut ControlPlane, attach_id: u32, generation: u64) {
    plane
        .feed(FrameKind::Attached {
            attach_id,
            snapshot: snapshot(),
            initial_client_id: ClientId::new(1),
        })
        .unwrap();
    let stream_id = StreamId::new(1).unwrap();
    let bootstrap_id = BootstrapId::new(generation).unwrap();
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 20,
            rows: 4,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: b"again".to_vec().into(),
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
        FrameKind::AttachReady { attach_id },
    ] {
        plane.feed(frame).unwrap();
    }
}

fn attached() -> ControlPlane {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"first");
    let _ = plane.take_outbound();
    plane
}

fn bell(plane: &mut ControlPlane) {
    plane
        .feed(FrameKind::Bell {
            terminal_id: terminal(),
        })
        .unwrap();
}

fn boundaries(observation: &Observation) -> Vec<u64> {
    observation
        .events
        .iter()
        .filter_map(|event| match event {
            Event::ConnectionOpened { connection_epoch } => Some(*connection_epoch),
            _ => None,
        })
        .collect()
}

#[test]
fn a_reconnect_reusing_every_id_is_visible_between_two_observations() {
    let mut plane = attached();
    let before = plane.take_observation();
    assert_eq!(before.connection_epoch, 1);
    assert_eq!(before.status, Status::Attached);
    assert_eq!(boundaries(&before), [1]);

    reconnect_with_reused_ids(&mut plane);
    let after = plane.take_observation();
    // Status, topology and ids are indistinguishable from `before`; only
    // the incarnation and the ordered boundary say the panes are new.
    assert_eq!(after.status, Status::Attached);
    assert_eq!(
        after.topology.as_ref().map(|topology| topology.panes.len()),
        before
            .topology
            .as_ref()
            .map(|topology| topology.panes.len())
    );
    assert_eq!(
        after.topology.as_ref().unwrap().panes[0].terminal_id,
        before.topology.as_ref().unwrap().panes[0].terminal_id
    );
    assert_eq!(after.connection_epoch, 2);
    assert_eq!(boundaries(&after), [2]);
    let lost = after
        .events
        .iter()
        .position(|event| matches!(event, Event::ConnectionLost { .. }))
        .expect("the loss is in the batch");
    let opened = after
        .events
        .iter()
        .position(|event| matches!(event, Event::ConnectionOpened { .. }))
        .unwrap();
    assert!(lost < opened);
}

#[test]
fn a_recovery_between_drain_and_status_sampling_stays_in_one_observation() {
    let mut plane = attached();
    let _ = plane.take_observation();
    plane.connection_lost(None);
    // Sampled mid-recovery: the loss and the status it led to, together.
    let mid = plane.take_observation();
    assert_eq!(mid.status, Status::Connecting);
    assert!(
        mid.events
            .iter()
            .any(|event| matches!(event, Event::ConnectionLost { .. }))
    );
    assert_eq!(mid.connection_epoch, 1);

    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let attach_id = plane
        .take_outbound()
        .iter()
        .find_map(|bytes| match decode(bytes) {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .unwrap();
    reattach(&mut plane, attach_id, 2);
    let done = plane.take_observation();
    assert_eq!(done.status, Status::Attached);
    assert_eq!(done.connection_epoch, 2);
    assert_eq!(boundaries(&done), [2]);
}

#[test]
fn old_events_queued_before_a_reconnect_precede_its_boundary() {
    let mut plane = attached();
    let _ = plane.take_observation();
    bell(&mut plane);
    reconnect_with_reused_ids(&mut plane);
    bell(&mut plane);
    let observation = plane.take_observation();
    let order: Vec<&str> = observation
        .events
        .iter()
        .filter_map(|event| match event {
            Event::Bell { .. } => Some("bell"),
            Event::ConnectionLost { .. } => Some("lost"),
            Event::ConnectionOpened { .. } => Some("opened"),
            _ => None,
        })
        .collect();
    assert_eq!(order, ["bell", "lost", "opened", "bell"]);
}

#[test]
fn an_ordinary_refresh_is_not_a_new_incarnation() {
    let mut plane = attached();
    let before = plane.take_observation();
    let request_id = plane.refresh_topology().unwrap();
    let _ = plane.take_outbound();
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::State(snapshot())),
        })
        .unwrap();
    let after = plane.take_observation();
    assert_eq!(after.connection_epoch, before.connection_epoch);
    assert!(boundaries(&after).is_empty());
    assert!(!after.events_dropped);
    assert!(
        after
            .events
            .iter()
            .any(|event| matches!(event, Event::TopologyChanged))
    );
}

#[test]
fn overflow_is_bounded_reported_once_and_keeps_the_newest_boundary() {
    let mut plane = attached();
    let _ = plane.take_observation();
    for _ in 0..3 {
        reconnect_with_reused_ids(&mut plane);
        for _ in 0..EVENT_QUEUE_CAP {
            bell(&mut plane);
        }
    }
    let flooded = plane.take_observation();
    assert!(flooded.events_dropped);
    assert!(flooded.events.len() <= EVENT_QUEUE_CAP + 1);
    assert_eq!(flooded.connection_epoch, 4);
    assert_eq!(boundaries(&flooded), [4]);
    assert!(
        flooded
            .events
            .iter()
            .any(|event| matches!(event, Event::TopologyChanged)),
        "the loss is followed by a topology barrier"
    );
    let calm = plane.take_observation();
    assert!(!calm.events_dropped);
    assert!(calm.events.is_empty());
}

#[test]
fn take_events_clears_the_overflow_report_too() {
    let mut plane = attached();
    for _ in 0..=EVENT_QUEUE_CAP {
        bell(&mut plane);
    }
    let _ = plane.take_events();
    assert!(!plane.take_observation().events_dropped);
}
