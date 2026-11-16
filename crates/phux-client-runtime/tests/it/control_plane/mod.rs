//! The control plane driven sans-IO: frames fed directly, with no socket and
//! no driver.

#![allow(clippy::expect_used, reason = "test assertions")]
#![allow(clippy::unwrap_used, reason = "test assertions")]
#![allow(clippy::panic, reason = "test assertions")]

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
use phux_wire_fixtures::{BootstrapOptions, bootstrap_frames, bootstrap_frames_with, command_err};

mod agent_sessions;
mod approvals;
mod batching;
mod checkout;
#[cfg(feature = "engine")]
mod clipboard_read;
mod document_stalls;
#[cfg(feature = "engine")]
mod documents;
mod forward;
mod geometry;
mod global_metadata;
#[cfg(feature = "engine")]
mod kitty;
mod lifecycle;
mod mirror;
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
    server_hello()
        .features(ServerFeatureSet::with(features))
        .build()
}

fn hello_ok_ext(features: &[ServerFeatureExt]) -> FrameKind {
    server_hello()
        .features_ext(ServerFeatureExtSet::with(features))
        .build()
}

/// A browsing plane past `HELLO_OK` from a server advertising `features`,
/// its follow-ups drained.
fn browsing_with_ext(features: &[ServerFeatureExt]) -> ControlPlane {
    let mut plane = opened(ControlOptions::default(), hello_ok_ext(features));
    let _ = plane.take_outbound();
    plane
}

/// The `GET_STATE` among `frames`, if one was sent.
fn state_read(frames: &[FrameKind]) -> Option<u32> {
    frames.iter().find_map(|frame| match frame {
        FrameKind::Command {
            request_id,
            command: Command::GetState { .. },
        } => Some(*request_id),
        _ => None,
    })
}

/// Answer topology read `request_id` with `snapshot`.
fn answer_state(plane: &mut ControlPlane, request_id: u32, snapshot: SessionSnapshot) {
    plane.reply(
        request_id,
        CommandResult::OkWith(CommandValue::State(snapshot)),
    );
}

/// Answer metadata read `request_id` with `value`.
fn answer_metadata(plane: &mut ControlPlane, request_id: u32, value: Option<&[u8]>) {
    plane
        .feed(FrameKind::MetadataValue {
            request_id,
            value: value.map(<[u8]>::to_vec),
        })
        .unwrap();
}

/// A live `METADATA_CHANGED` of `Global` `key`.
fn global_changed(plane: &mut ControlPlane, key: &str, value: Option<&[u8]>) {
    plane
        .feed(FrameKind::MetadataChanged {
            scope: phux_protocol::wire::frame::Scope::Global,
            key: key.to_owned(),
            value: value.map(<[u8]>::to_vec),
            actor: None,
        })
        .unwrap();
}

/// Every queued outbound frame, decoded.
fn outbound(plane: &mut ControlPlane) -> Vec<FrameKind> {
    plane
        .take_outbound()
        .iter()
        .map(|bytes| decode(bytes))
        .collect()
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
use phux_wire_fixtures::encode;

/// A plane past `HELLO_OK`, with the attach id its `ATTACH` carried.
fn negotiated() -> (ControlPlane, u32) {
    let options = ControlOptions {
        attach: Some(AttachTarget::ByName("main".to_owned())),
        viewport: (20, 4),
        ..ControlOptions::default()
    };
    let (plane, followups) = negotiate(options, hello_ok(PROTOCOL_VERSION.patch), Some(2));
    assert_eq!(plane.status(), Status::Negotiated);
    assert!(matches!(
        followups[0],
        FrameKind::SubscribeEvents {
            terminal: None,
            after_seq: None
        }
    ));
    let FrameKind::Attach {
        attach_id,
        ref target,
        ..
    } = followups[1]
    else {
        panic!("second follow-up is ATTACH");
    };
    assert_eq!(*target, AttachTarget::ByName("main".to_owned()));
    (plane, attach_id)
}

/// Attach subscribes to `phux.session.project/v1` and a stored value becomes
/// one event. The test feeds the frames; it does not insert the tag by hand.
#[test]
fn attach_reads_the_session_project_tag_into_an_event() {
    use phux_protocol::wire::frame::{SESSION_PROJECT_KEY, Scope, encode_session_project};

    let (mut plane, attach_id) = negotiated();
    plane.attached(attach_id, two_session_snapshot(false));
    let outbound: Vec<FrameKind> = outbound(&mut plane);
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
    answer_metadata(
        &mut plane,
        request_id,
        Some(&encode_session_project("beta", "phux")),
    );
    assert!(
        plane.take_events().iter().any(|event| matches!(
            event,
            Event::SessionProject { name, project }
                if name == "beta" && project.as_deref() == Some("phux")
        )),
        "the GET value must surface as a session project event"
    );
    global_changed(&mut plane, SESSION_PROJECT_KEY, None);
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
    answer_state(plane, request_id, snapshot);
    let _ = plane.take_events();
}

fn output(plane: &mut ControlPlane, seq: u64, bytes: &[u8]) {
    plane
        .feed(phux_wire_fixtures::output(&terminal(), seq, bytes))
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
        .feed(command_err(
            request,
            ErrorCode::InputDeliveryUnknown,
            "ambiguous test delivery",
        ))
        .unwrap();
    assert!(plane.delivery_fenced(&terminal()));
    delivery
}

#[test]
fn conditional_projection_ack_rejects_a_newer_fence_without_reconnecting() {
    let mut plane = opened(
        ControlOptions::default(),
        hello_ok_with(&[ServerFeature::AcknowledgedInput]),
    );
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
    empty_bootstrap(plane, &terminal(), [2, 2]);
    plane
        .feed(FrameKind::AttachReady {
            attach_id: next_attach,
        })
        .unwrap();
}

/// A 20x4 raw synthesized bootstrap of `id` on `[stream, bootstrap]` that
/// carries no chunk: begin, then ready.
fn empty_bootstrap(plane: &mut ControlPlane, id: &ResourceId, ids: [u64; 2]) {
    let [begin, _, ready] = bootstrap_frames(id, ids, [20, 4], b"");
    plane.feed(begin).expect("BOOTSTRAP_BEGIN");
    plane.feed(ready).expect("BOOTSTRAP_READY");
}
