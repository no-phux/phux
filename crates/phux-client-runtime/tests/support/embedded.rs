//! The fed-frame fixture both control-plane test binaries share: a plane
//! past `HELLO_OK`, attached to one 20x4 terminal named `main`, and (with the
//! engine) an embedded client holding that terminal's replica.

#![allow(unreachable_pub, reason = "shared test support")]

#[cfg(feature = "engine")]
use phux_client_runtime::control::ControlOptions;
use phux_client_runtime::control::ControlPlane;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, Layer, LayerSet, ServerCapabilities,
    ServerFeatureSet,
};
use phux_protocol::ids::{BootstrapId, ClientId, ResourceId, SessionId, StreamId, WindowId};
#[cfg(feature = "engine")]
use phux_protocol::wire::frame::AttachTarget;
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

pub const fn terminal() -> ResourceId {
    ResourceId::local(7)
}

pub fn hello_ok(patch: u16) -> FrameKind {
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

pub fn snapshot() -> SessionSnapshot {
    SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal())
        .with_sessions(vec![SessionInfo::new(SessionId::new(1), "main")])
        .with_windows(vec![WindowInfo::new(
            WindowId::new(1),
            SessionId::new(1),
            "shell",
        )])
        .with_resources(vec![ResourceInfo::new(terminal(), WindowId::new(1), 20, 4)])
}

pub fn decode(frame: &[u8]) -> FrameKind {
    let (decoded, rest) = FrameKind::decode(frame).expect("outbound frame decodes");
    assert!(rest.is_empty());
    decoded
}

pub fn attach_with_history(
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
pub fn embedded_with_history(history_cursor: Option<Vec<u8>>) -> phux_client_runtime::Client {
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
