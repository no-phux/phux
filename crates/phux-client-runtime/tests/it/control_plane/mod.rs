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
#[cfg(feature = "engine")]
use phux_protocol::caps::BootstrapLimits;
use phux_protocol::caps::{
    BootstrapStreamProfile, ServerFeature, ServerFeatureExt, ServerFeatureExtSet, ServerFeatureSet,
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

use crate::embedded::*;

mod agent_sessions;
mod approvals;
mod batching;
mod checkout;
mod geometry;
mod global_metadata;
#[cfg(feature = "engine")]
mod kitty;
mod lifecycle;
mod native_agent;
mod newer_server;
mod observation;
mod paste;
mod path_query;
mod recovery;
mod roster;
mod service_http;
mod spawns;
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

/// A manually driven attach does not allocate a project-tag read. Cockpit
/// answers the first metadata GET as workspace layout.
#[test]
fn a_manual_lifecycle_attach_does_not_fetch_the_project_tag() {
    use phux_protocol::wire::frame::{SESSION_PROJECT_KEY, Scope, ViewportInfo};

    let mut plane = ControlPlane::new(ControlOptions {
        automatic_lifecycle: false,
        ..ControlOptions::default()
    });
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane
        .feed(hello_ok(PROTOCOL_VERSION.patch))
        .expect("HELLO_OK");
    assert!(plane.attach_explicit(
        7,
        AttachTarget::ByName("main".to_owned()),
        ViewportInfo::new(20, 4),
        false,
        0,
        None,
    ));
    let _ = plane.take_outbound();
    plane
        .feed(FrameKind::Attached {
            attach_id: 7,
            snapshot: two_session_snapshot(false),
            initial_client_id: ClientId::new(1),
        })
        .unwrap();
    let outbound: Vec<FrameKind> = plane
        .take_outbound()
        .iter()
        .map(|frame| decode(frame))
        .collect();
    assert!(
        outbound.iter().all(|frame| !matches!(
            frame,
            FrameKind::GetMetadata { scope: Scope::Global, key, .. }
                | FrameKind::SubscribeMetadata { scope: Scope::Global, key }
                if key == SESSION_PROJECT_KEY
        )),
        "a manual attach must leave project-tag reads to the embedder: {outbound:?}"
    );
}

/// Attach subscribes to `phux.session.project/v1` and a stored value becomes
/// one event. The test feeds the frames; it does not insert the tag by hand.
#[test]
fn attach_reads_the_session_project_tag_into_an_event() {
    use phux_protocol::wire::frame::{SESSION_PROJECT_KEY, Scope, encode_session_project};

    let (mut plane, attach_id) = negotiated();
    plane
        .feed(FrameKind::Attached {
            attach_id,
            snapshot: two_session_snapshot(false),
            initial_client_id: ClientId::new(1),
        })
        .unwrap();
    let outbound: Vec<FrameKind> = plane
        .take_outbound()
        .iter()
        .map(|frame| decode(frame))
        .collect();
    assert!(
        outbound.iter().any(|frame| matches!(
            frame,
            FrameKind::SubscribeMetadata { scope: Scope::Global, key }
                if key == SESSION_PROJECT_KEY
        )),
        "attach must subscribe to the project tag: {outbound:?}"
    );
    let request_id = outbound
        .iter()
        .find_map(|frame| match frame {
            FrameKind::GetMetadata {
                request_id,
                scope: Scope::Global,
                key,
            } if key == SESSION_PROJECT_KEY => Some(*request_id),
            _ => None,
        })
        .expect("attach GETs the project tag");
    plane
        .feed(FrameKind::MetadataValue {
            request_id,
            value: Some(encode_session_project("beta", "phux")),
        })
        .unwrap();
    assert!(
        plane.take_events().iter().any(|event| matches!(
            event,
            Event::SessionProject { name, project }
                if name == "beta" && project.as_deref() == Some("phux")
        )),
        "the GET value must surface as a session project event"
    );
    plane
        .feed(FrameKind::MetadataChanged {
            scope: Scope::Global,
            key: SESSION_PROJECT_KEY.to_owned(),
            value: None,
            actor: None,
        })
        .unwrap();
    assert!(
        plane.take_events().iter().any(|event| matches!(
            event,
            Event::SessionProject { name, project } if name.is_empty() && project.is_none()
        )),
        "a tombstone must clear the project tag"
    );
}

fn attach(plane: &mut ControlPlane, attach_id: u32, bytes: &[u8]) {
    attach_with_history(plane, attach_id, bytes, None);
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

fn outbound_attach_id(plane: &mut ControlPlane) -> u32 {
    plane
        .take_outbound()
        .into_iter()
        .find_map(|frame| {
            if let FrameKind::Attach { attach_id, .. } = decode(&frame) {
                Some(attach_id)
            } else {
                None
            }
        })
        .unwrap()
}

fn finish_reconnected_attach(plane: &mut ControlPlane) {
    let next_attach = outbound_attach_id(plane);
    plane
        .feed(FrameKind::Attached {
            attach_id: next_attach,
            snapshot: snapshot(),
            initial_client_id: ClientId::new(2),
        })
        .unwrap();
    let stream_id = StreamId::new(2).unwrap();
    let bootstrap_id = BootstrapId::new(2).unwrap();
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
        .unwrap();
    plane
        .feed(FrameKind::BootstrapReady {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        })
        .unwrap();
    plane
        .feed(FrameKind::AttachReady {
            attach_id: next_attach,
        })
        .unwrap();
}
