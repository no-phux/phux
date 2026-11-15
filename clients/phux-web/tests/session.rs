//! Protocol-0.7 web session transcripts against the real wasm engine.

use bytes::{Bytes, BytesMut};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::ResourceKind;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapProfileKind, BootstrapStreamProfile, EngineCodec,
    EngineFeatureSet, ImageProtocolSet, ServerCapabilities, ServerFeatureExt, ServerFeatureExtSet,
};
use phux_protocol::ids::{
    ActivationId, AuthorityId, BootstrapId, ClientId, ResourceId, ResourceKey, Revision,
    SatelliteHost, ServerInstance, SessionId, SessionKey, StreamId, WindowId,
};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    ActivationInfo, AttachTarget, CloseReason, Command, CommandResult, CommandValue, ErrorCode,
    ExecState, FrameKind, OrgFilter, OrganizationPage, PathKind, PathResults, PathRow, PathStatus,
    Refusal, ResourceRecord, SessionRecord, SpawnResult,
};
use phux_protocol::wire::info::{AgentFacet, ResourceInfo, SessionInfo, SessionSnapshot};
use phux_vt_web::Vt;
use phux_web::{AttachPreference, Connection};
use wasm_bindgen_test::wasm_bindgen_test;

#[path = "session/agents.rs"]
mod agents;
#[path = "session/bootstrap.rs"]
mod bootstrap;
#[path = "session/documents.rs"]
mod documents;
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

const RAW: BootstrapStreamProfile = BootstrapStreamProfile::SynthesizedVtRaw;

fn stream(raw: u64) -> StreamId {
    StreamId::new(raw).expect("non-zero stream")
}

fn bootstrap(raw: u64) -> BootstrapId {
    BootstrapId::new(raw).expect("non-zero bootstrap")
}

/// One bootstrap generation of a terminal, and the frames addressed to it.
#[derive(Clone)]
struct Gen {
    id: ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
}

/// Local terminal `id` on stream `stream_raw`, generation `bootstrap_raw`.
fn generation(id: u32, stream_raw: u64, bootstrap_raw: u64) -> Gen {
    Gen {
        id: ResourceId::local(id),
        stream_id: stream(stream_raw),
        bootstrap_id: bootstrap(bootstrap_raw),
    }
}

impl Gen {
    fn on(id: ResourceId, serial: u64) -> Self {
        Self {
            id,
            stream_id: stream(serial),
            bootstrap_id: bootstrap(serial),
        }
    }

    fn begin(&self, profile: BootstrapStreamProfile, cols: u16, rows: u16, base: u64) -> FrameKind {
        FrameKind::BootstrapBegin {
            terminal_id: self.id.clone(),
            stream_id: self.stream_id,
            bootstrap_id: self.bootstrap_id,
            profile,
            cols,
            rows,
            base_seq: base,
        }
    }

    fn chunk(&self, chunk_seq: u32, payload: impl Into<Bytes>) -> FrameKind {
        FrameKind::BootstrapChunk {
            terminal_id: self.id.clone(),
            stream_id: self.stream_id,
            bootstrap_id: self.bootstrap_id,
            chunk_seq,
            payload: payload.into(),
        }
    }

    fn ready(&self) -> FrameKind {
        self.ready_with(None)
    }

    fn ready_with(&self, history_cursor: Option<Bytes>) -> FrameKind {
        FrameKind::BootstrapReady {
            terminal_id: self.id.clone(),
            stream_id: self.stream_id,
            bootstrap_id: self.bootstrap_id,
            history_cursor,
        }
    }

    fn output(&self, seq: u64, bytes: impl Into<Bytes>) -> FrameKind {
        FrameKind::ResourceOutput {
            terminal_id: self.id.clone(),
            stream_id: self.stream_id,
            bootstrap_id: self.bootstrap_id,
            seq,
            bytes: bytes.into(),
        }
    }

    /// Begin, one chunk of `payload`, and READY, each asserted non-fatal.
    fn publish(
        &self,
        session: &mut Connection,
        profile: BootstrapStreamProfile,
        (cols, rows): (u16, u16),
        payload: impl Into<Bytes>,
    ) -> Vec<phux_web::Outcome> {
        let frames = [
            self.begin(profile, cols, rows, 0),
            self.chunk(0, payload),
            self.ready(),
        ];
        frames
            .into_iter()
            .map(|frame| {
                let outcome = session.on_frame(frame);
                assert!(outcome.fatal.is_none(), "{:?}", outcome.fatal);
                outcome
            })
            .collect()
    }
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
        server_caps: ServerCapabilities::new(),
        server_id: Vec::new(),
        selected_profile: Some(profile),
        bootstrap_limits: limits,
    }
}

/// `HELLO_OK` selecting raw synthesized VT with `caps`.
fn hello_with(caps: ServerCapabilities) -> FrameKind {
    let mut frame = synth_hello();
    if let FrameKind::HelloOk { server_caps, .. } = &mut frame {
        *server_caps = caps;
    }
    frame
}

fn synth_hello() -> FrameKind {
    hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    )
}

/// A fresh connection of `cols`x`rows` past a raw synthesized `HELLO_OK`.
async fn hello_session(cols: u16, rows: u16) -> Connection {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, cols, rows);
    session.on_frame(synth_hello());
    session
}

fn attached(terminal_id: ResourceId, cols: u16, rows: u16) -> FrameKind {
    attached_snapshot(
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal_id.clone())
            .with_resources(vec![ResourceInfo::new(
                terminal_id,
                WindowId::new(1),
                cols,
                rows,
            )]),
    )
}

fn attached_snapshot(snapshot: SessionSnapshot) -> FrameKind {
    FrameKind::Attached {
        attach_id: 1,
        snapshot,
        initial_client_id: ClientId::new(1),
    }
}

const ATTACH_READY: FrameKind = FrameKind::AttachReady { attach_id: 1 };

fn closed(terminal_id: ResourceId, reason: CloseReason) -> FrameKind {
    FrameKind::ResourceClosed {
        terminal_id,
        exit_status: None,
        reason,
        signal: None,
    }
}

fn reply(request_id: u32, result: CommandResult) -> FrameKind {
    FrameKind::CommandResult { request_id, result }
}

fn error_reply(request_id: u32, code: ErrorCode, message: &str) -> FrameKind {
    reply(
        request_id,
        CommandResult::Error {
            code,
            message: message.to_owned(),
        },
    )
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

/// The characters of a terminal's grid, row-major.
fn grid_text(terminal: &phux_vt_web::Terminal) -> String {
    terminal.grid().cells.iter().map(|cell| cell.ch).collect()
}

/// The first row of the connection's visible grid.
fn row0(session: &Connection) -> String {
    let grid = session.grid();
    grid.cells[..usize::from(grid.cols)]
        .iter()
        .map(|cell| cell.ch)
        .collect()
}

fn path_hello(supported: bool) -> FrameKind {
    hello_with(if supported {
        ServerCapabilities::new()
            .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::PathQuery]))
    } else {
        ServerCapabilities::new()
    })
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
    ready_session(path_hello(supported), Vec::new(), panes).await
}

/// A 20x3 connection past `hello`, attached to `panes` (the first focused)
/// with `sessions` listed, every pane bootstrapped empty and ATTACH_READY.
async fn ready_session(
    hello: FrameKind,
    sessions: Vec<SessionInfo>,
    panes: Vec<ResourceId>,
) -> Connection {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 20, 3);
    session.on_frame(hello);
    let resources = panes
        .iter()
        .cloned()
        .map(|id| ResourceInfo::new(id, WindowId::new(1), 20, 3))
        .collect();
    session.on_frame(attached_snapshot(
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), panes[0].clone())
            .with_sessions(sessions)
            .with_resources(resources),
    ));
    for (serial, id) in (1..).zip(panes) {
        let pane = Gen::on(id, serial);
        session.on_frame(pane.begin(RAW, 20, 3, 0));
        session.on_frame(pane.ready());
    }
    session.on_frame(ATTACH_READY);
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
        result: SpawnResult::Ok(id.clone()),
    });
    match decode_one(&outcome.send[0]) {
        FrameKind::Command {
            request_id,
            command: Command::AttachResource { terminal_id, .. },
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
    let mut outcomes = Gen::on(id, serial).publish(session, RAW, (9, 3), text);
    outcomes.pop().expect("READY outcome")
}

/// Split the focused pane into `id` and commit it: spawn, attach,
/// bootstrap, and the attach's acknowledgement.
fn commit_split(session: &mut Connection, axis: &str, id: ResourceId, serial: u64) {
    let spawn = requested_spawn(session, axis);
    let attach = announce_split(session, spawn, id.clone());
    bootstrap_split(session, id, serial, b"pane");
    session.on_frame(reply(attach, CommandResult::Ok));
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
    reply(
        request_id,
        CommandResult::OkWith(CommandValue::State(snapshot)),
    )
}
