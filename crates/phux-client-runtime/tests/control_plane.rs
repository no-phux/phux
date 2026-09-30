//! The control plane driven sans-IO: frames fed directly, the way Cockpit
//! drives `phux-client-ffi`, with no socket and no driver.

#![allow(clippy::expect_used, reason = "test assertions")]
#![allow(clippy::unwrap_used, reason = "test assertions")]
#![allow(clippy::panic, reason = "test assertions")]

#[cfg(feature = "engine")]
use bytes::BytesMut;
use phux_client_runtime::control::{
    ControlError, ControlOptions, ControlPlane, DeliveryOutcome, Event, FileUploadOutcome,
    MAX_PATH_ANSWERS, PathFailure, PathMatchKind, PathSearchStatus, SpawnRequest, Status,
    StreamRecovery,
};
#[cfg(feature = "engine")]
use phux_client_runtime::engine::EngineEvent;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, Layer, LayerSet, ServerCapabilities,
    ServerFeature, ServerFeatureExt, ServerFeatureExtSet, ServerFeatureSet,
};
use phux_protocol::ids::{
    BootstrapId, ClientId, ResourceId, SatelliteHost, SessionId, StreamId, WindowId,
};
use phux_protocol::input::InputEvent;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    AttachTarget, Command, CommandResult, CommandValue, DetachReason, ErrorCode, FrameKind,
    PathKind, PathResults, PathRow, PathStatus, SpawnResult,
};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

const fn terminal() -> ResourceId {
    ResourceId::local(7)
}

#[path = "support/geometry.rs"]
mod geometry;

#[path = "support/roster.rs"]
mod roster;

#[path = "support/recovery.rs"]
mod recovery;

#[path = "support/views.rs"]
mod views;

#[cfg(feature = "engine")]
#[test]
fn replacing_an_engine_synchronously_retires_all_outgoing_view_slots() {
    let client = embedded_with_history(None);
    let old_owner = client.engine().unwrap();
    let view = client.create_view(&terminal()).unwrap();
    let slot = client.view_slot(view).unwrap();
    let default_slot = client.slot(&terminal()).unwrap();
    let held = slot.acquire().unwrap();
    let text = held.text();
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    let mut hello = hello_ok(PROTOCOL_VERSION.patch);
    if let FrameKind::HelloOk {
        bootstrap_limits, ..
    } = &mut hello
    {
        *bootstrap_limits = BootstrapLimits::new(1024, 1024).unwrap();
    }
    client.feed(hello).unwrap();
    assert!(client.acquire_view(view).is_none());
    assert!(slot.acquire().is_none());
    assert!(default_slot.acquire().is_none());
    assert!(client.destroy_view(view).is_err());
    assert!(old_owner.republish_view(view).is_err());
    assert_eq!(held.text(), text);
    client.close();
    drop(client);
    assert!(slot.acquire().is_none());
    assert_eq!(held.text(), text);
}

#[cfg(feature = "engine")]
#[test]
fn dropping_client_retires_views_even_with_an_external_owner_clone() {
    let client = embedded_with_history(None);
    let owner = client.engine().unwrap();
    let view = client.create_view(&terminal()).unwrap();
    let slot = client.view_slot(view).unwrap();
    let held = slot.acquire().unwrap();
    drop(client);
    assert!(slot.acquire().is_none());
    assert!(owner.republish_view(view).is_err());
    assert_eq!(held.row_text(0), "two");
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

#[test]
fn path_query_requires_extended_capability_and_correlates_typed_results() {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let _ = plane.take_outbound();
    assert_eq!(plane.path_query("/home".into(), "a".into(), true, None), 0);
    assert!(
        plane.take_outbound().is_empty(),
        "older server must not hang picker"
    );

    plane.connection_opened();
    let _ = plane.take_outbound();
    let mut hello = hello_ok(PROTOCOL_VERSION.patch);
    if let FrameKind::HelloOk { server_caps, .. } = &mut hello {
        *server_caps = server_caps
            .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::PathQuery]));
    }
    plane.feed(hello).unwrap();
    let _ = plane.take_outbound();
    let id = plane.path_query(
        "/home".into(),
        "a".into(),
        true,
        Some(SatelliteHost::new("peer")),
    );
    assert_ne!(id, 0);
    assert!(
        matches!(&decode(&plane.take_outbound()[0]), FrameKind::PathQuery {
        request_id, root, query, recursive: true, host: Some(host)
    } if *request_id == id && root == "/home" && query == "a" && host.as_str() == "peer")
    );
    plane
        .feed(FrameKind::PathResults {
            request_id: id,
            result: Ok(PathResults {
                root: "/home".into(),
                parent: Some("/".into()),
                rows: vec![PathRow {
                    path: "/home/a b".into(),
                    kind: PathKind::Symlink,
                }],
                status: PathStatus::Warming,
            }),
        })
        .unwrap();
    let answers = plane.take_path_answers();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].request_id, id);
    assert_eq!(answers[0].rows[0].kind, PathMatchKind::Symlink);
    assert_eq!(answers[0].rows[0].path, "/home/a b");
    assert_eq!(answers[0].status, Some(PathSearchStatus::Warming));
    assert_eq!(answers[0].failure, None);
    plane
        .feed(FrameKind::PathResults {
            request_id: id,
            result: Err(phux_protocol::wire::frame::PathQueryError {
                root: "/home".into(),
                code: phux_protocol::wire::frame::PathErrorCode::NotFound,
                message: "late".into(),
            }),
        })
        .unwrap();
    assert!(
        plane.take_path_answers().is_empty(),
        "duplicate must not satisfy a new query"
    );
}

#[test]
fn path_queries_are_bounded_and_connection_scoped() {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    let _ = plane.take_outbound();
    let mut hello = hello_ok(PROTOCOL_VERSION.patch);
    if let FrameKind::HelloOk { server_caps, .. } = &mut hello {
        *server_caps = server_caps
            .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::PathQuery]));
    }
    plane.feed(hello).unwrap();
    let _ = plane.take_outbound();
    let ids: Vec<_> = (0..MAX_PATH_ANSWERS)
        .map(|_| plane.path_query("/tmp".into(), String::new(), false, None))
        .collect();
    assert!(ids.iter().all(|id| *id != 0));
    let before = plane.take_outbound();
    assert_eq!(before.len(), MAX_PATH_ANSWERS);
    assert_eq!(
        plane.path_query("/tmp".into(), String::new(), false, None),
        0
    );
    assert!(plane.take_outbound().is_empty());
    plane
        .feed(FrameKind::Error {
            request_id: Some(ids[0]),
            code: ErrorCode::InvalidCommand,
            message: "refused".into(),
        })
        .unwrap();
    plane.connection_lost(None);
    let answers = plane.take_path_answers();
    assert_eq!(answers.len(), MAX_PATH_ANSWERS);
    assert_eq!(
        answers
            .iter()
            .find(|answer| answer.request_id == ids[0])
            .unwrap()
            .failure,
        Some(PathFailure::Other)
    );
    assert_eq!(
        answers
            .iter()
            .filter(|answer| answer.failure == Some(PathFailure::Unanswered))
            .count(),
        MAX_PATH_ANSWERS - 1
    );
    assert_eq!(
        plane.path_query("/tmp".into(), String::new(), false, None),
        0
    );
    // A late reply from the old socket cannot resolve any new correlation.
    plane.connection_opened();
    plane
        .feed(FrameKind::PathResults {
            request_id: ids[1],
            result: Ok(PathResults {
                root: "/tmp".into(),
                parent: Some("/".into()),
                rows: Vec::new(),
                status: PathStatus::Complete,
            }),
        })
        .unwrap_err(); // no server frame before HELLO_OK
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

#[cfg(feature = "engine")]
fn encode(frame: &FrameKind) -> Vec<u8> {
    let mut bytes = BytesMut::new();
    frame.encode(&mut bytes);
    bytes.to_vec()
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
    attach_with_history(plane, attach_id, bytes, None);
}

fn attach_with_history(
    plane: &mut ControlPlane,
    attach_id: u32,
    bytes: &[u8],
    history_cursor: Option<Vec<u8>>,
) {
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
            history_cursor: history_cursor.map(Into::into),
        })
        .expect("BOOTSTRAP_READY");
    plane
        .feed(FrameKind::AttachReady { attach_id })
        .expect("ATTACH_READY");
}

#[cfg(feature = "engine")]
fn embedded_with_history(history_cursor: Option<Vec<u8>>) -> phux_client_runtime::Client {
    let client = phux_client_runtime::Runtime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName("main".into())),
        viewport: (20, 4),
        ..ControlOptions::default()
    });
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let attach_id = client
        .take_outbound()
        .iter()
        .find_map(|bytes| match decode(bytes) {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .unwrap();
    client.with_control(|plane| {
        attach_with_history(
            plane,
            attach_id,
            b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive",
            history_cursor,
        );
    });
    let _ = client.take_outbound();
    client
}

/// The delivery shape sets the owner-thread cost, and the `runtime.*`
/// counters show it: a transport read fed as one batch is one apply round
/// trip and one grid publication; the same frames fed one at a time are one
/// round trip each, and publish per frame only for a consumer that reads
/// every frame. Unread, they cost one projection, pulled by the next read.
#[cfg(feature = "engine")]
#[test]
fn a_batched_read_publishes_once_and_frame_at_a_time_publishes_per_read() {
    use phux_client_runtime::perf::{ACQUIRED, APPLY_BATCHES, CAUGHT_UP, DEFERRED, PUBLISHED};
    const FRAMES: u64 = 8;
    let client = embedded_with_history(None);
    let frame = |seq: u64| {
        let mut encoded = BytesMut::new();
        FrameKind::ResourceOutput {
            terminal_id: terminal(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq,
            bytes: format!("\r\nline {seq}").into_bytes().into(),
        }
        .encode(&mut encoded);
        encoded.to_vec()
    };

    let _ = client.acquire(&terminal()).unwrap();
    let (batches, published) = (APPLY_BATCHES.get(), PUBLISHED.get());
    let read: Vec<_> = (1..=FRAMES).map(frame).collect();
    client
        .with_control(|plane| plane.feed_bytes_batch(&read))
        .unwrap();
    assert_eq!(APPLY_BATCHES.get() - batches, 1, "one round trip per read");
    assert_eq!(PUBLISHED.get() - published, 1, "one publication per read");

    // A consumer that reads every frame is published every frame.
    let (batches, published) = (APPLY_BATCHES.get(), PUBLISHED.get());
    for seq in FRAMES + 1..=2 * FRAMES {
        let _ = client.acquire(&terminal()).unwrap();
        client
            .with_control(|plane| plane.feed_bytes(&frame(seq)))
            .unwrap();
    }
    assert_eq!(APPLY_BATCHES.get() - batches, FRAMES);
    assert_eq!(PUBLISHED.get() - published, FRAMES);

    // Nobody reads: every frame after the first unread one is deferred, and
    // the read that follows pulls one projection of the final state.
    let _ = client.acquire(&terminal()).unwrap();
    let (batches, published, deferred, acquired) = (
        APPLY_BATCHES.get(),
        PUBLISHED.get(),
        DEFERRED.get(),
        ACQUIRED.get(),
    );
    for seq in 2 * FRAMES + 1..=3 * FRAMES {
        client
            .with_control(|plane| plane.feed_bytes(&frame(seq)))
            .unwrap();
    }
    assert_eq!(APPLY_BATCHES.get() - batches, FRAMES);
    assert_eq!(PUBLISHED.get() - published, 1, "only the first frame");
    assert_eq!(DEFERRED.get() - deferred, FRAMES - 1);
    let caught_up = CAUGHT_UP.get();
    assert!(
        client
            .acquire(&terminal())
            .unwrap()
            .row_text(3)
            .starts_with("line 24")
    );
    assert_eq!(CAUGHT_UP.get() - caught_up, 1);
    assert_eq!(PUBLISHED.get() - published, 2, "the read pulled one more");
    assert_eq!(ACQUIRED.get() - acquired, 1);
}

#[cfg(feature = "engine")]
#[test]
fn client_clones_share_the_default_but_explicit_views_are_local_without_wire_operations() {
    use phux_client_runtime::engine::Scroll;
    let client = embedded_with_history(None);
    let clone = client.clone();
    let a = client.create_view(&terminal()).unwrap();
    let b = clone.create_view(&terminal()).unwrap();
    assert!(
        client.take_outbound().is_empty(),
        "creating views cannot attach, resize, or spawn"
    );
    client.scroll(&terminal(), Scroll::Top).unwrap();
    assert_eq!(clone.acquire(&terminal()).unwrap().row_text(0), "zero");
    assert!(clone.acquire_view(a).unwrap().scrollbar.at_tail());
    let b_generation = clone.view_generation(b);
    clone.scroll_view(a, Scroll::Top).unwrap();
    assert_eq!(client.acquire_view(a).unwrap().row_text(0), "zero");
    assert_eq!(clone.view_generation(b), b_generation);
    clone.destroy_view(a).unwrap();
    assert!(client.has_projection(&terminal()));
    assert!(
        client.take_outbound().is_empty(),
        "local view operations cannot detach or resize"
    );
    assert_eq!(client.with_control(|plane| plane.viewport()), (20, 4));
}

#[cfg(feature = "engine")]
#[test]
fn view_scrolling_routes_history_requests_through_control_plane() {
    use phux_client_runtime::engine::Scroll;
    use phux_protocol::wire::frame::HistoryRejectionReason;
    let client = embedded_with_history(Some(b"older".to_vec()));
    client
        .feed(FrameKind::HistoryRejected {
            terminal_id: terminal(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            cursor: b"older".to_vec().into(),
            reason: HistoryRejectionReason::ZeroLimit,
            required_bytes: 0,
            required_rows: 0,
        })
        .unwrap();
    let _ = client.take_outbound();
    let view = client.create_view(&terminal()).unwrap();
    assert!(client.take_outbound().is_empty());
    client.scroll_view(view, Scroll::Top).unwrap();
    let requests = client.take_outbound();
    assert_eq!(requests.len(), 1);
    assert!(
        matches!(decode(&requests[0]), FrameKind::HistoryRequest { cursor, terminal_id, .. }
        if cursor.as_ref() == b"older" && terminal_id == terminal())
    );
    assert!(client.acquire(&terminal()).unwrap().scrollbar.at_tail());
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

fn unknown_input_delivery(plane: &mut ControlPlane) -> u64 {
    let delivery = plane.apply_line(&terminal(), "command");
    let request = plane
        .take_outbound()
        .iter()
        .find_map(|bytes| match decode(bytes) {
            FrameKind::Command {
                request_id,
                command: Command::ApplyInput { .. },
            } => Some(request_id),
            _ => None,
        })
        .expect("APPLY_INPUT attempt");
    plane
        .feed(FrameKind::CommandResult {
            request_id: request,
            result: CommandResult::Error {
                code: ErrorCode::InputDeliveryUnknown,
                message: "ambiguous test delivery".into(),
            },
        })
        .unwrap();
    assert!(plane.delivery_fenced(&terminal()));
    delivery
}

#[test]
fn conditional_projection_ack_rejects_a_newer_fence_without_reconnecting() {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    plane.take_outbound();
    plane
        .feed(hello_ok_with(&[ServerFeature::AcknowledgedInput]))
        .unwrap();
    plane.take_outbound();
    let first_delivery = unknown_input_delivery(&mut plane);
    let first = plane.projection_fence(&terminal()).unwrap();
    assert_eq!(first.delivery_id, first_delivery);
    assert!(plane.acknowledge_projection_if(&terminal(), first));
    let second_delivery = unknown_input_delivery(&mut plane);
    let second = plane.projection_fence(&terminal()).unwrap();
    assert_eq!(second.delivery_id, second_delivery);
    assert_eq!(first.connection_epoch, second.connection_epoch);
    assert_ne!(first.delivery_id, second.delivery_id);
    assert!(!plane.acknowledge_projection_if(&terminal(), first));
    assert!(plane.delivery_fenced(&terminal()));
    assert!(!plane.acknowledge_projection_if(&ResourceId::local(99), second));
    assert!(plane.acknowledge_projection_if(&terminal(), second));
    assert!(!plane.delivery_fenced(&terminal()));
    assert!(!plane.acknowledge_projection_if(&terminal(), second));
    unknown_input_delivery(&mut plane);
    let before_reconnect = plane.projection_fence(&terminal()).unwrap();
    plane.connection_opened();
    assert!(!plane.acknowledge_projection_if(&terminal(), before_reconnect));
    assert!(plane.delivery_fenced(&terminal()));
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
#[expect(
    clippy::cognitive_complexity,
    reason = "one attach-to-input scenario; every assert! scores as a branch"
)]
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

#[cfg(feature = "engine")]
#[test]
fn a_transport_batch_publishes_once_and_preserves_ordered_changes() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"ready");
    let _ = plane.take_events();
    // A caught-up consumer, so the batch publishes without being asked.
    let before = plane
        .publication()
        .acquire(&terminal())
        .expect("bootstrap published")
        .generation;
    let event_count = 32_u64;
    let frames = (1..=event_count)
        .map(|seq| {
            encode(&FrameKind::ResourceOutput {
                terminal_id: terminal(),
                stream_id: StreamId::new(1).expect("stream"),
                bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
                seq,
                bytes: b"x".to_vec().into(),
            })
        })
        .collect::<Vec<_>>();

    plane
        .feed_bytes_batch(&frames)
        .expect("ordered transport batch");

    assert_eq!(
        plane.publication().generation(&terminal()),
        Some(before + 1)
    );
    assert_eq!(
        observed_text(&plane).matches('x').count(),
        usize::try_from(event_count).expect("fixture count fits usize")
    );
    assert_eq!(
        plane
            .take_events()
            .into_iter()
            .filter(|event| matches!(event, Event::TerminalChanged { .. }))
            .count(),
        1,
        "the binding wake queue coalesces duplicate terminal changes"
    );
}

#[cfg(feature = "engine")]
#[test]
fn a_control_frame_splits_engine_batches_without_reordering() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"ready");
    let _ = plane.take_outbound();
    let before = plane
        .publication()
        .acquire(&terminal())
        .expect("bootstrap published")
        .generation;
    let output = |seq, bytes: &'static [u8]| {
        encode(&FrameKind::ResourceOutput {
            terminal_id: terminal(),
            stream_id: StreamId::new(1).expect("stream"),
            bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
            seq,
            bytes: bytes.to_vec().into(),
        })
    };
    let frames = vec![
        output(1, b"one"),
        encode(&FrameKind::Ping { nonce: 17 }),
        output(2, b"two"),
    ];

    plane.feed_bytes_batch(&frames).expect("ordered batch");

    let frame = plane.publication().acquire(&terminal()).expect("published");
    assert_eq!(frame.generation, before + 2);
    assert_eq!(frame.last_seq, 2);
    assert!(matches!(
        decode(&plane.take_outbound()[0]),
        FrameKind::Pong { nonce: 17 }
    ));
}

#[cfg(feature = "engine")]
#[test]
fn a_valid_batch_prefix_is_applied_before_a_later_decode_error() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"ready");
    let before = plane
        .publication()
        .generation(&terminal())
        .expect("bootstrap published");
    let frames = vec![
        encode(&FrameKind::ResourceOutput {
            terminal_id: terminal(),
            stream_id: StreamId::new(1).expect("stream"),
            bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
            seq: 1,
            bytes: b"valid".to_vec().into(),
        }),
        vec![0, 0, 0, 0],
    ];

    let error = plane
        .feed_bytes_batch(&frames)
        .expect_err("malformed second frame");

    assert!(matches!(error, ControlError::Protocol(_)));
    let frame = plane.publication().acquire(&terminal()).expect("published");
    assert_eq!(frame.generation, before + 1);
    assert_eq!(frame.last_seq, 1);
}

#[cfg(feature = "engine")]
#[test]
fn a_control_ending_outranks_an_earlier_stale_engine_frame() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"ready");
    let frames = vec![
        encode(&FrameKind::ResourceOutput {
            terminal_id: terminal(),
            stream_id: StreamId::new(2).expect("stale stream"),
            bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
            seq: 1,
            bytes: b"stale".to_vec().into(),
        }),
        encode(&FrameKind::Detached {
            reason: Some(DetachReason::ProtocolError),
            message: "fatal ending".to_owned(),
        }),
    ];

    let error = plane
        .feed_bytes_batch(&frames)
        .expect_err("fatal ending must not be hidden");

    assert!(
        matches!(&error, ControlError::Refused(message) if message.contains("fatal ending")),
        "expected the ending to outrank stale generation, got {error:?}"
    );
}

#[cfg(feature = "engine")]
#[test]
fn a_batch_cannot_hide_a_fatal_error_behind_an_earlier_stale_generation() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"ready");
    let error = plane
        .apply_engine_events(vec![
            EngineEvent::Output {
                terminal_id: terminal(),
                stream_id: StreamId::new(2).expect("stale stream"),
                bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
                seq: 1,
                bytes: b"stale".to_vec().into(),
            },
            EngineEvent::Output {
                terminal_id: terminal(),
                stream_id: StreamId::new(1).expect("stream"),
                bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
                seq: 2,
                bytes: b"gap".to_vec().into(),
            },
        ])
        .expect_err("the current generation has a fatal sequence gap");

    assert!(
        matches!(&error, ControlError::Protocol(message) if message.contains("sequence gap")),
        "expected the fatal sequence gap to outrank stale generation, got {error:?}"
    );
}

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

/// A plane that negotiated acknowledged input, with nothing queued.
fn acknowledging() -> ControlPlane {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    plane.take_outbound();
    plane
        .feed(hello_ok_with(&[ServerFeature::AcknowledgedInput]))
        .unwrap();
    plane.take_outbound();
    plane
}

/// Every paste the frames carry, and whether it went acknowledged.
fn pastes(frames: &[Vec<u8>]) -> Vec<(bool, PasteEvent)> {
    frames
        .iter()
        .filter_map(|bytes| match decode(bytes) {
            FrameKind::Command {
                command: Command::ApplyInput { events, .. },
                ..
            } => events.into_iter().find_map(|event| match event {
                InputEvent::Paste(paste) => Some((true, paste)),
                _ => None,
            }),
            FrameKind::InputPaste { event, .. } => Some((false, event)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_user_paste_is_trusted_so_multiline_text_is_not_refused() {
    let mut plane = acknowledging();
    let delivery = plane.apply_user_paste(&terminal(), "echo a\necho b\n");
    assert!(delivery.is_some(), "one command's worth is acknowledged");
    let sent = pastes(&plane.take_outbound());
    assert_eq!(sent.len(), 1);
    let (acknowledged, paste) = &sent[0];
    assert!(acknowledged);
    assert_eq!(paste.trust, PasteTrust::Trusted);
    assert_eq!(paste.data, b"echo a\necho b\n");
    assert!(
        !plane
            .take_events()
            .iter()
            .any(|event| matches!(event, Event::InputDelivery { .. })),
        "nothing refused locally"
    );
}

#[test]
fn a_program_paste_stays_untrusted() {
    let mut plane = acknowledging();
    plane.apply_paste(&terminal(), "x\n");
    let sent = pastes(&plane.take_outbound());
    assert_eq!(sent[0].1.trust, PasteTrust::Untrusted);
}

#[test]
fn a_user_paste_too_large_for_one_command_goes_as_one_paste_event() {
    let mut plane = acknowledging();
    let text = "0123456789abcdef\n".repeat(8 * 1024);
    assert!(text.len() > 64 * 1024);
    assert_eq!(plane.apply_user_paste(&terminal(), &text), None);
    let sent = pastes(&plane.take_outbound());
    assert_eq!(sent.len(), 1, "never split (input.md 5.1)");
    let (acknowledged, paste) = &sent[0];
    assert!(!acknowledged);
    assert_eq!(paste.trust, PasteTrust::Trusted);
    assert_eq!(paste.data, text.as_bytes());
}

#[test]
fn a_large_user_paste_never_overtakes_unresolved_input() {
    let mut plane = acknowledging();
    plane.apply_user_paste(&terminal(), "first");
    plane.take_outbound();
    // A raw event could reach the PTY before the unresolved paste, so the
    // large one is refused, with a receipt, rather than reordered.
    let text = "x".repeat(70 * 1024);
    let refused = plane.apply_user_paste(&terminal(), &text).expect("receipt");
    assert!(pastes(&plane.take_outbound()).is_empty());
    assert!(plane.take_events().iter().any(|event| matches!(
        event,
        Event::InputDelivery { delivery_id, outcome: DeliveryOutcome::Refused, .. }
            if *delivery_id == refused
    )));
}

#[test]
fn a_user_paste_into_a_satellite_pane_goes_as_one_paste_event() {
    let mut plane = acknowledging();
    let satellite = ResourceId::satellite("peer", 3);
    assert_eq!(plane.apply_user_paste(&satellite, "ls\npwd\n"), None);
    let sent = pastes(&plane.take_outbound());
    assert_eq!(sent.len(), 1);
    assert!(!sent[0].0, "APPLY_INPUT is local-only");
    assert_eq!(sent[0].1.trust, PasteTrust::Trusted);
}
