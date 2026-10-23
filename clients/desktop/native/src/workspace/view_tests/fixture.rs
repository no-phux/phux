//! An embedded runtime attached to one session's terminals.

use super::super::*;
use phux_client_runtime::control::ControlPlane;
use phux_protocol::{
    PROTOCOL_VERSION,
    caps::{
        BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
        ServerFeatureSet,
    },
    ids::{BootstrapId, ClientId, SessionId, StreamId, WindowId},
    wire::{
        frame::FrameKind,
        info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo},
    },
};

use std::sync::Arc;

pub(super) const SESSION: &str = "views";

pub(super) fn terminal() -> ResourceId {
    ResourceId::local(7)
}

pub(super) fn stream() -> StreamId {
    StreamId::new(1).unwrap()
}

pub(super) fn bootstrap() -> BootstrapId {
    BootstrapId::new(1).unwrap()
}

/// A connection attached to one 20x4 terminal with two lines of history.
pub(crate) fn attached() -> Connection {
    attached_with(&[])
}

/// [`attached`], with `others` in the same session, listed but not yet streamed.
pub(crate) fn attached_with(others: &[ResourceId]) -> Connection {
    let mut connection = Connection::connect("/views.sock".into(), SESSION.into()).unwrap();
    let client = Arc::clone(&connection.client);
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client
        .feed(FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new()
                .with_features(ServerFeatureSet::with(&[ServerFeature::AcknowledgedInput])),
            server_id: vec![1; 16],
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        })
        .unwrap();
    let attach_id = client
        .take_outbound()
        .iter()
        .find_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .expect("attach request");
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal())
        .with_sessions(vec![SessionInfo::new(SessionId::new(1), SESSION)])
        .with_windows(vec![WindowInfo::new(
            WindowId::new(1),
            SessionId::new(1),
            "shell",
        )])
        .with_resources(
            std::iter::once(terminal())
                .chain(others.iter().cloned())
                .map(|id| ResourceInfo::new(id, WindowId::new(1), 20, 4))
                .collect(),
        );
    for frame in [
        FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        },
        FrameKind::BootstrapBegin {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 20,
            rows: 4,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            chunk_seq: 0,
            payload: b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive"
                .to_vec()
                .into(),
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            history_cursor: None,
        },
    ] {
        client.feed(frame).unwrap();
    }
    for (index, other) in (2..).zip(others) {
        bootstrap_empty(&client, other, index);
    }
    client.feed(FrameKind::AttachReady { attach_id }).unwrap();
    connection.drain();
    let _ = client.take_outbound();
    assert_eq!(connection.status, Status::Attached);
    assert!(connection.ready(&terminal()), "fixture terminal is ready");
    connection
}

/// Stream `terminal` with no history, on stream and bootstrap `index`.
fn bootstrap_empty(client: &phux_client_runtime::Client, terminal: &ResourceId, index: u64) {
    let stream_id = StreamId::new(index).unwrap();
    let bootstrap_id = BootstrapId::new(index).unwrap();
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: terminal.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 20,
            rows: 4,
            base_seq: 0,
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ] {
        client.feed(frame).unwrap();
    }
}

/// A workspace showing the session's home terminal in one placement.
pub(crate) fn opened(connection: &mut Connection) -> Workspace {
    let mut workspace = Workspace::new(SESSION.into(), WorkspaceOptions::default());
    workspace.restore(None);
    let events = connection.drain();
    workspace.handle(&events, connection);
    workspace.settle(connection);
    assert_eq!(workspace.views_of(&terminal()), 1, "home terminal opened");
    workspace
}
