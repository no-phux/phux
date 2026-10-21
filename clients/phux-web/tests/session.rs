//! Protocol-0.7 web session transcripts against the real wasm engine.

use bytes::{Bytes, BytesMut};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::ResourceKind;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapProfileKind, EngineCodec, EngineFeatureSet,
    ImageProtocolSet, ServerCapabilities, ServerFeatureExt, ServerFeatureExtSet,
};
use phux_protocol::ids::{
    ActivationId, AuthorityId, BootstrapId, ClientId, ResourceId, ResourceKey, Revision,
    SatelliteHost, ServerInstance, SessionId, SessionKey, StreamId, WindowId,
};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    ActivationInfo, AttachTarget, Command, CommandResult, CommandValue, ExecState, FrameKind,
    OrgFilter, OrganizationPage, PathKind, PathResults, PathRow, PathStatus, Refusal,
    ResourceRecord, SessionRecord,
};
use phux_protocol::wire::info::{AgentFacet, ResourceInfo, SessionInfo, SessionSnapshot};
use phux_vt_web::Vt;
use phux_web::{AttachPreference, Connection};
use wasm_bindgen_test::wasm_bindgen_test;

#[path = "session/agents.rs"]
mod agents;
#[path = "session/bootstrap.rs"]
mod bootstrap;
#[path = "session/handshake.rs"]
mod handshake;
#[path = "session/native.rs"]
mod native;
#[path = "session/newer_server.rs"]
mod newer_server;
#[path = "session/organization.rs"]
mod organization;
#[path = "session/path_query.rs"]
mod path_query;
#[path = "session/splits.rs"]
mod splits;
#[path = "session/viewport.rs"]
mod viewport;

fn stream(raw: u64) -> StreamId {
    StreamId::new(raw).expect("non-zero stream")
}

fn bootstrap(raw: u64) -> BootstrapId {
    BootstrapId::new(raw).expect("non-zero bootstrap")
}

fn key() -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::A,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: Some("a".to_owned()),
        unshifted_codepoint: Some(u32::from(b'a')),
    }
}

fn hello_ok(profile: BootstrapProfile, limits: BootstrapLimits) -> FrameKind {
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: phux_protocol::caps::ServerCapabilities::new(),
        server_id: Vec::new(),
        selected_profile: profile,
        bootstrap_limits: limits,
    }
}

fn attached(terminal_id: ResourceId, cols: u16, rows: u16) -> FrameKind {
    FrameKind::Attached {
        attach_id: 1,
        snapshot: SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal_id.clone())
            .with_resources(vec![ResourceInfo::new(
                terminal_id,
                WindowId::new(1),
                cols,
                rows,
            )]),
        initial_client_id: ClientId::new(1),
    }
}

fn begin(
    terminal_id: ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    profile: phux_protocol::caps::BootstrapStreamProfile,
    cols: u16,
    rows: u16,
    base_seq: u64,
) -> FrameKind {
    FrameKind::BootstrapBegin {
        terminal_id,
        stream_id,
        bootstrap_id,
        profile,
        cols,
        rows,
        base_seq,
    }
}

fn native_profile() -> BootstrapProfile {
    BootstrapProfile::NativeState {
        codec: EngineCodec::LibghosttySnapshotV1,
        features: EngineFeatureSet::required_native(),
    }
}

fn decode_one(frame: &[u8]) -> FrameKind {
    let (decoded, rest) = FrameKind::decode(frame).expect("decodable client frame");
    assert!(rest.is_empty());
    decoded
}

fn path_hello(supported: bool) -> FrameKind {
    let mut frame = hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    );
    if let FrameKind::HelloOk { server_caps, .. } = &mut frame {
        *server_caps = if supported {
            ServerCapabilities::new()
                .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::PathQuery]))
        } else {
            ServerCapabilities::new()
        };
    }
    frame
}

async fn path_session(supported: bool, second_pane: bool) -> Connection {
    let panes = if second_pane {
        vec![ResourceId::local(101), ResourceId::local(102)]
    } else {
        vec![ResourceId::local(101)]
    };
    path_session_with_panes(supported, panes).await
}

async fn path_session_with_panes(supported: bool, panes: Vec<ResourceId>) -> Connection {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 20, 3);
    session.on_frame(path_hello(supported));
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), panes[0].clone())
        .with_resources(
            panes
                .iter()
                .cloned()
                .map(|id| ResourceInfo::new(id, WindowId::new(1), 20, 3))
                .collect(),
        );
    session.on_frame(FrameKind::Attached {
        attach_id: 1,
        snapshot,
        initial_client_id: ClientId::new(1),
    });
    for (i, id) in panes.iter().enumerate() {
        let stream_id = stream(i as u64 + 1);
        let bootstrap_id = bootstrap(i as u64 + 1);
        session.on_frame(begin(
            id.clone(),
            stream_id,
            bootstrap_id,
            phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
            20,
            3,
            0,
        ));
        session.on_frame(FrameKind::BootstrapReady {
            terminal_id: id.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        });
    }
    session.on_frame(FrameKind::AttachReady { attach_id: 1 });
    session
}

fn requested_spawn(session: &mut Connection, axis: &str) -> u32 {
    match decode_one(&session.split_pane_frame(axis).expect("split request")) {
        FrameKind::SpawnResource { request_id, .. } => request_id,
        other => panic!("expected spawn, got {other:?}"),
    }
}

fn announce_split(session: &mut Connection, request_id: u32, id: ResourceId) -> u32 {
    let outcome = session.on_frame(FrameKind::ResourceSpawned {
        request_id,
        result: phux_protocol::wire::frame::SpawnResult::Ok(id.clone()),
    });
    match decode_one(&outcome.send[0]) {
        FrameKind::Command {
            request_id,
            command: phux_protocol::wire::frame::Command::AttachResource { terminal_id, .. },
        } => {
            assert_eq!(terminal_id, id);
            request_id
        }
        other => panic!("expected attach, got {other:?}"),
    }
}

fn bootstrap_split(
    session: &mut Connection,
    id: ResourceId,
    serial: u64,
    text: &'static [u8],
) -> phux_web::Outcome {
    let stream_id = stream(serial);
    let bootstrap_id = bootstrap(serial);
    let began = session.on_frame(begin(
        id.clone(),
        stream_id,
        bootstrap_id,
        phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
        9,
        3,
        0,
    ));
    assert!(began.fatal.is_none());
    let chunk = session.on_frame(FrameKind::BootstrapChunk {
        terminal_id: id.clone(),
        stream_id,
        bootstrap_id,
        chunk_seq: 0,
        payload: Bytes::from_static(text),
    });
    assert!(chunk.fatal.is_none());
    let ready = session.on_frame(FrameKind::BootstrapReady {
        terminal_id: id,
        stream_id,
        bootstrap_id,
        history_cursor: None,
    });
    assert!(ready.fatal.is_none());
    ready
}

/// Answer the `GET_STATE` that follows `HELLO_OK` with one existing session
/// and return the `ATTACH` the connection sends for it.
fn attach_after_state(session: &mut Connection, hello: &phux_web::Outcome) -> Vec<u8> {
    let request_id = state_request(&hello.send[1]);
    let reply = session.on_frame(state_reply(
        request_id,
        vec![SessionInfo::new(SessionId::new(1), "work")],
    ));
    assert_eq!(
        reply.send.len(),
        1,
        "the session list is answered by ATTACH"
    );
    reply.send[0].clone()
}

fn state_request(frame: &[u8]) -> u32 {
    match decode_one(frame) {
        FrameKind::Command {
            request_id,
            command: Command::GetState { .. },
        } => request_id,
        other => panic!("expected GET_STATE, got {other:?}"),
    }
}

fn state_reply(request_id: u32, sessions: Vec<SessionInfo>) -> FrameKind {
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_sessions(sessions);
    FrameKind::CommandResult {
        request_id,
        result: CommandResult::OkWith(CommandValue::State(snapshot)),
    }
}
