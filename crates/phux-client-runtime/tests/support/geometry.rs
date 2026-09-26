use super::*;
use phux_client_runtime::control::TerminalResizeOutcome as Resize;
use phux_protocol::wire::frame::RolePolicy;

fn browse(options: ControlOptions) -> ControlPlane {
    let mut plane = ControlPlane::new(options);
    plane.connection_opened();
    plane.take_outbound();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    plane.take_outbound();
    plane
}

fn confirm(plane: &mut ControlPlane, request_id: u32) {
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        })
        .unwrap();
}

fn bootstrap(plane: &mut ControlPlane, id: &ResourceId, generation: u64) {
    let stream_id = StreamId::new(generation).unwrap();
    let bootstrap_id = BootstrapId::new(generation).unwrap();
    plane
        .feed(FrameKind::BootstrapBegin {
            terminal_id: id.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 20,
            rows: 4,
            base_seq: 0,
        })
        .unwrap();
    plane
        .feed(FrameKind::BootstrapReady {
            terminal_id: id.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        })
        .unwrap();
}

fn ready(plane: &mut ControlPlane, id: &ResourceId) {
    let request = plane.attach_terminal_preserving_geometry(id);
    assert_ne!(request, 0);
    bootstrap(plane, id, 1);
    confirm(plane, request);
    plane.take_outbound();
}

#[test]
fn targeted_resize_only_sends_one_resource_and_never_fabricates_readback() {
    let mut plane = browse(ControlOptions::default());
    let other = ResourceId::local(8);
    ready(&mut plane, &terminal());
    ready(&mut plane, &other);
    #[cfg(feature = "engine")]
    let before = plane.publication().acquire(&terminal()).unwrap();
    assert_eq!(plane.resize_terminal(&terminal(), 100, 30), Resize::Queued);
    let frames = plane.take_outbound();
    assert_eq!(frames.len(), 1);
    assert!(
        matches!(decode(&frames[0]), FrameKind::ResizeTerminal { terminal_id, cols: 100, rows: 30 } if terminal_id == terminal())
    );
    assert_eq!(plane.viewport(), (80, 24));
    #[cfg(feature = "engine")]
    {
        let after = plane.publication().acquire(&terminal()).unwrap();
        assert_eq!(after.generation, before.generation);
        assert_eq!((after.cols, after.rows), (20, 4));
        let untouched = plane.publication().acquire(&other).unwrap();
        assert_eq!((untouched.cols, untouched.rows), (20, 4));
    }
    assert!(plane.take_events().iter().all(|event| !matches!(event, Event::Frame(frame) if matches!(frame.as_ref(), FrameKind::ResizeTerminal { .. }))));
}

#[test]
fn resize_rejects_invalid_sizes_viewers_and_unready_resources_before_send() {
    let mut plane = browse(ControlOptions::default());
    for (cols, rows) in [(0, 24), (80, 0), (65536, 24), (80, u32::MAX)] {
        assert_eq!(
            plane.resize_terminal(&terminal(), cols, rows),
            Resize::InvalidSize
        );
    }
    assert_eq!(plane.resize_terminal(&terminal(), 80, 24), Resize::NotReady);
    let request = plane.attach_terminal_preserving_geometry(&terminal());
    bootstrap(&mut plane, &terminal(), 1);
    assert_eq!(plane.resize_terminal(&terminal(), 80, 24), Resize::NotReady);
    confirm(&mut plane, request);
    plane.take_outbound();
    plane.connection_lost(None);
    assert_eq!(plane.resize_terminal(&terminal(), 80, 24), Resize::NotReady);
    assert!(plane.take_outbound().is_empty());

    let mut observer = browse(ControlOptions {
        attach_role: Some(RolePolicy::VIEWER),
        ..ControlOptions::default()
    });
    ready(&mut observer, &terminal());
    assert_eq!(
        observer.resize_terminal(&terminal(), 80, 24),
        Resize::Observer
    );
    assert!(observer.take_outbound().is_empty());
}

#[test]
fn preserving_subscriptions_replay_without_global_resize_or_old_replica_readiness() {
    let mut plane = browse(ControlOptions::default());
    ready(&mut plane, &terminal());
    assert_eq!(plane.attach_terminal_preserving_geometry(&terminal()), 0);
    assert_eq!(plane.attach_terminal(&terminal()), 0);
    assert!(plane.take_outbound().is_empty());
    plane.connection_lost(None);
    plane.connection_opened();
    plane.take_outbound();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let frames: Vec<_> = plane
        .take_outbound()
        .iter()
        .map(|frame| decode(frame))
        .collect();
    assert!(!frames.iter().any(|frame| matches!(
        frame,
        FrameKind::ResizeTerminal { .. }
            | FrameKind::ViewportResize { .. }
            | FrameKind::Attach { .. }
    )));
    let requests: Vec<_> = frames
        .iter()
        .filter_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command: Command::AttachResource { terminal_id, .. },
            } if *terminal_id == terminal() => Some(*request_id),
            _ => None,
        })
        .collect();
    assert_eq!(requests.len(), 1);
    confirm(&mut plane, requests[0]);
    assert_eq!(
        plane.resize_terminal(&terminal(), 80, 24),
        Resize::NotReady,
        "retained old publication is not current-connection evidence"
    );
    bootstrap(&mut plane, &terminal(), 2);
    plane.take_outbound();
    assert_eq!(plane.resize_terminal(&terminal(), 80, 24), Resize::Queued);
}

#[test]
fn global_viewport_fanout_and_existing_default_attach_keep_their_policy() {
    let mut plane = browse(ControlOptions::default());
    let default = ResourceId::local(8);
    plane.attach_terminal_preserving_geometry(&terminal());
    let initial = plane.take_outbound();
    assert_eq!(initial.len(), 1);
    assert!(matches!(
        decode(&initial[0]),
        FrameKind::Command {
            command: Command::AttachResource { .. },
            ..
        }
    ));
    plane.attach_terminal(&default);
    assert_eq!(plane.attach_terminal_preserving_geometry(&default), 0);
    let initial = plane.take_outbound();
    assert_eq!(initial.len(), 2);
    assert!(
        matches!(decode(&initial[1]), FrameKind::ResizeTerminal { terminal_id, cols: 80, rows: 24 } if terminal_id == default)
    );
    plane.resize_viewport(90, 25);
    let frames = plane.take_outbound();
    assert_eq!(frames.len(), 2);
    assert!(matches!(
        decode(&frames[0]),
        FrameKind::ViewportResize { .. }
    ));
    assert!(
        matches!(decode(&frames[1]), FrameKind::ResizeTerminal { terminal_id, cols: 90, rows: 25 } if terminal_id == default)
    );
}

#[test]
fn manual_reconnect_retains_policy_but_leaves_attach_scheduling_to_embedder() {
    let mut plane = browse(ControlOptions {
        automatic_lifecycle: false,
        ..ControlOptions::default()
    });
    ready(&mut plane, &terminal());
    plane.connection_opened();
    plane.take_outbound();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    assert!(plane.take_outbound().is_empty());
    plane.attach_terminal(&terminal());
    assert_eq!(
        plane.take_outbound().len(),
        1,
        "policy survives an embedder's legacy reattach call"
    );
    plane.detach_terminal(&terminal());
    plane.take_outbound();
    // Resolve the replaced attach correlation before starting a fresh intent.
    plane.connection_opened();
    plane.take_outbound();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    plane.attach_terminal(&terminal());
    assert_eq!(
        plane.take_outbound().len(),
        2,
        "detach releases preserving policy"
    );
}

#[test]
fn stream_recovery_preserves_geometry() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"home");
    refresh_to(&mut plane, two_session_snapshot(false));
    let foreign = ResourceId::local(8);
    ready(&mut plane, &foreign);
    assert_eq!(plane.ensure_stream(&foreign), StreamRecovery::Attached);
    let frames = plane.take_outbound();
    assert_eq!(frames.len(), 1);
    assert!(matches!(
        decode(&frames[0]),
        FrameKind::Command {
            command: Command::AttachResource { .. },
            ..
        }
    ));
}

fn reconnect(plane: &mut ControlPlane) -> Vec<FrameKind> {
    plane.connection_lost(None);
    plane.connection_opened();
    plane.take_outbound();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    plane
        .take_outbound()
        .iter()
        .map(|frame| decode(frame))
        .collect()
}

fn resource_attaches(frames: &[FrameKind]) -> Vec<ResourceId> {
    frames
        .iter()
        .filter_map(|frame| match frame {
            FrameKind::Command {
                command: Command::AttachResource { terminal_id, .. },
                ..
            } => Some(terminal_id.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn authoritative_close_retires_preserving_intent_without_ui_release() {
    let mut plane = browse(ControlOptions::default());
    let survivor = ResourceId::local(8);
    ready(&mut plane, &terminal());
    ready(&mut plane, &survivor);
    plane
        .feed(FrameKind::ResourceClosed {
            terminal_id: terminal(),
            exit_status: Some(0),
            signal: None,
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
        })
        .unwrap();
    assert_eq!(plane.resize_terminal(&terminal(), 80, 24), Resize::NotReady);
    for _ in 0..2 {
        let frames = reconnect(&mut plane);
        assert_eq!(resource_attaches(&frames), vec![survivor.clone()]);
    }
}

#[test]
fn browse_to_session_reconnect_defers_preserving_replay_until_inventory_is_ready() {
    let mut plane = browse(ControlOptions::default());
    let foreign = ResourceId::local(8);
    ready(&mut plane, &terminal());
    ready(&mut plane, &foreign);
    assert!(plane.attach_session(AttachTarget::ByName("main".to_owned())));
    let frames = reconnect(&mut plane);
    assert!(
        resource_attaches(&frames).is_empty(),
        "session inventory must arrive before resource replay"
    );
    let attach_id = frames
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Attach {
                attach_id, target, ..
            } => {
                assert_eq!(target, &AttachTarget::ByName("main".to_owned()));
                Some(*attach_id)
            }
            _ => None,
        })
        .expect("current session target was queued");
    plane
        .feed(FrameKind::Attached {
            attach_id,
            snapshot: snapshot(),
            initial_client_id: ClientId::new(1),
        })
        .unwrap();
    bootstrap(&mut plane, &terminal(), 2);
    assert!(
        resource_attaches(
            &plane
                .take_outbound()
                .iter()
                .map(|frame| decode(frame))
                .collect::<Vec<_>>()
        )
        .is_empty()
    );
    plane.feed(FrameKind::AttachReady { attach_id }).unwrap();
    let frames: Vec<_> = plane
        .take_outbound()
        .iter()
        .map(|frame| decode(frame))
        .collect();
    assert_eq!(resource_attaches(&frames), vec![foreign]);
    assert!(
        !frames.iter().any(|frame| matches!(
            frame,
            FrameKind::ResizeTerminal { .. } | FrameKind::ViewportResize { .. }
        )),
        "preserving replay must not resize: {frames:?}"
    );
}
