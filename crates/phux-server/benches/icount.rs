//! Instruction counts for the hot paths the `perf-icount` job guards.
//!
//! Each function is one callgrind region. Setup runs outside that region.
//! Scenarios:
//!
//! - Frame encode and decode: one `RESOURCE_OUTPUT` through the live-path
//!   writer and the client `split_frame` + `decode_shared` read.
//! - Agent append and replay: validate, stamp, retain, and copy the ring.
//! - Client-kernel history slice and resync: the session kernel, with a
//!   stand-in engine so the count is the kernel rather than a second
//!   libghostty.
//! - Organization commit: one `ResourceMove` through `ServerState` into a
//!   SQLite store holding 5,000 resources in 200 sessions and a receipt
//!   window at its cap (CONTRACTS §08).
//!
//! The engine's own scenarios are `phux-terminal`'s `terminal_icount` bench;
//! both report to `benchmarks/icount-baseline.json`.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    missing_docs,
    reason = "callgrind harness"
)]

use std::hint::black_box;

use bytes::{Bytes, BytesMut};
use iai_callgrind::{library_benchmark, library_benchmark_group, main};
use phux_client_core::engine::{
    BootstrapProgress, CanonicalGeometry, EngineAdapter, EngineDamage, EngineEffect,
    EngineEffectBuffer, HistoryApplyOutcome,
};
use phux_client_core::session::{EffectBuffer, KernelInput, SessionKernel};
use phux_protocol::caps::{
    BootstrapProfile, BootstrapStreamProfile, EngineCodec, EngineFeatureSet,
};
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::framing;
use phux_protocol::{BootstrapId, ResourceId, StreamId};
use phux_server::org_scale::{ScaleOrg, ScaleSpec};
use phux_server::resource::agent_session::{self, AgentSessionActor};

const VT_COLS: u16 = 80;
const VT_ROWS: u16 = 24;
const FRAME_BYTES: usize = 4 * 1024;
const HISTORY_BYTES: usize = 8 * 1024;
const AGENT_RECORDS: usize = 32;

struct EncodeReady {
    frame: FrameKind,
    buf: BytesMut,
}

struct DecodeReady {
    socket: BytesMut,
    wire: Vec<u8>,
}

struct AgentReady {
    actor: AgentSessionActor,
    payload: Bytes,
}

struct HistoryReady {
    kernel: SessionKernel<IcountAdapter>,
    terminal: ResourceId,
    stream: StreamId,
    bootstrap: BootstrapId,
    effects: EffectBuffer,
    page: Vec<u8>,
}

#[derive(Debug, Default)]
struct IcountReplica {
    bytes: usize,
}

#[derive(Debug, Default)]
struct IcountAdapter;

#[derive(Debug)]
struct IcountError;

impl std::fmt::Display for IcountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("icount adapter")
    }
}

impl std::error::Error for IcountError {}

impl EngineAdapter for IcountAdapter {
    type Error = IcountError;
    type Replica = IcountReplica;

    fn start_replica(
        &mut self,
        _profile: BootstrapStreamProfile,
        _geometry: CanonicalGeometry,
    ) -> Result<Self::Replica, Self::Error> {
        Ok(IcountReplica::default())
    }

    fn apply_bootstrap_chunk(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        _effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        replica.bytes = replica.bytes.saturating_add(payload.len());
        Ok(BootstrapProgress::Ready)
    }

    fn finish_bootstrap(
        &mut self,
        _replica: &mut Self::Replica,
        _effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        Ok(BootstrapProgress::Ready)
    }

    fn apply_history_page(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        declared_rows: u32,
        effects: &mut EngineEffectBuffer,
    ) -> Result<HistoryApplyOutcome, Self::Error> {
        replica.bytes = replica.bytes.saturating_add(payload.len());
        effects.push(EngineEffect::Damage(EngineDamage::Full));
        Ok(HistoryApplyOutcome {
            progress: BootstrapProgress::Ready,
            retained_rows: declared_rows as usize,
            authenticated_rows: declared_rows as usize,
        })
    }

    fn apply_output(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<(), Self::Error> {
        replica.bytes = replica.bytes.saturating_add(payload.len());
        effects.push(EngineEffect::Damage(EngineDamage::Full));
        Ok(())
    }
}

const fn stream() -> StreamId {
    StreamId::new(3).expect("stream")
}

const fn generation() -> BootstrapId {
    BootstrapId::new(9).expect("generation")
}

fn output_frame(payload: &Bytes) -> FrameKind {
    FrameKind::ResourceOutput {
        terminal_id: ResourceId::local(7),
        stream_id: stream(),
        bootstrap_id: generation(),
        seq: 123_456,
        bytes: payload.clone(),
    }
}

fn agent_payload() -> Bytes {
    use std::fmt::Write as _;
    let mut out = String::new();
    let text = "x".repeat(200);
    for n in 0..AGENT_RECORDS {
        writeln!(
            out,
            "{{\"type\":\"prompt\",\"data\":{{\"n\":{n},\"text\":\"{text}\"}}}}"
        )
        .expect("payload");
    }
    Bytes::from(out)
}

const fn native_profile() -> BootstrapProfile {
    BootstrapProfile::NativeState {
        codec: EngineCodec::LibghosttySnapshotV1,
        features: EngineFeatureSet::required_native(),
    }
}

const fn stream_profile() -> BootstrapStreamProfile {
    BootstrapStreamProfile::NativeState {
        codec: EngineCodec::LibghosttySnapshotV1,
    }
}

fn ready_kernel() -> HistoryReady {
    let terminal = ResourceId::local(7);
    let stream = StreamId::new(11).expect("stream");
    let bootstrap = BootstrapId::new(13).expect("bootstrap");
    let mut kernel = SessionKernel::new(IcountAdapter, native_profile());
    let mut effects = EffectBuffer::with_capacity(8);
    kernel
        .update(
            KernelInput::BootstrapBegin {
                terminal_id: &terminal,
                stream_id: stream,
                bootstrap_id: bootstrap,
                profile: stream_profile(),
                geometry: CanonicalGeometry::new(VT_COLS, VT_ROWS).expect("geometry"),
                base_seq: 0,
            },
            &mut effects,
        )
        .expect("bootstrap begin");
    kernel
        .update(
            KernelInput::BootstrapChunk {
                terminal_id: &terminal,
                stream_id: stream,
                bootstrap_id: bootstrap,
                chunk_seq: 0,
                payload: b"authenticated-ready",
            },
            &mut effects,
        )
        .expect("bootstrap chunk");
    kernel
        .update(
            KernelInput::BootstrapReady {
                terminal_id: &terminal,
                stream_id: stream,
                bootstrap_id: bootstrap,
                history_cursor: Some(b"cursor-0"),
            },
            &mut effects,
        )
        .expect("bootstrap ready");
    HistoryReady {
        kernel,
        terminal,
        stream,
        bootstrap,
        effects,
        page: vec![b'h'; HISTORY_BYTES],
    }
}

fn setup_encode() -> EncodeReady {
    let payload = Bytes::from(vec![b'x'; FRAME_BYTES]);
    EncodeReady {
        frame: output_frame(&payload),
        buf: BytesMut::with_capacity(FRAME_BYTES + 256),
    }
}

fn setup_decode() -> DecodeReady {
    let payload = Bytes::from(vec![b'x'; FRAME_BYTES]);
    let mut wire = BytesMut::new();
    output_frame(&payload).encode(&mut wire);
    DecodeReady {
        socket: BytesMut::with_capacity(wire.len() + 64),
        wire: wire.to_vec(),
    }
}

fn setup_agent() -> AgentReady {
    AgentReady {
        actor: agent_session::icount_actor(),
        payload: agent_payload(),
    }
}

fn setup_history() -> HistoryReady {
    ready_kernel()
}

struct OrgReady {
    _dir: tempfile::TempDir,
    org: ScaleOrg,
    command: phux_core::Command,
}

fn org_move(org: &ScaleOrg, operation: u8) -> phux_core::Command {
    let (from, to) = (org.sessions[0], org.sessions[1]);
    let state = &org.state;
    let rev = |key| state.org().session(&key).expect("session").revision;
    phux_core::Command {
        operation: phux_core::OperationId::from_bytes([operation; 16]),
        op: phux_core::Op::ResourceMove {
            resource: phux_server::org_scale::members(state, from)[0],
            from,
            from_expected: rev(from),
            to,
            to_expected: rev(to),
        },
    }
}

fn setup_org() -> OrgReady {
    let dir = tempfile::tempdir().expect("tempdir");
    let now = phux_server::state::now_stamp();
    let mut org =
        phux_server::org_scale::seed(&dir.path().join("v1.sqlite3"), ScaleSpec::CONTRACT, now)
            .expect("seed");
    // One warm commit: prunes the expired receipts, fills the statement
    // cache, and leaves the receipt window at its cap.
    let warm = org_move(&org, 0xa1);
    org.state
        .commit_command(&warm, &phux_core::Actor::owner())
        .expect("warm commit");
    let command = org_move(&org, 0xa2);
    OrgReady {
        _dir: dir,
        org,
        command,
    }
}

#[library_benchmark]
#[bench::encode(setup = setup_encode)]
fn frame_encode(mut ready: EncodeReady) -> usize {
    ready.buf.clear();
    ready.frame.encode(&mut ready.buf);
    black_box(ready.buf.len())
}

#[library_benchmark]
#[bench::decode(setup = setup_decode)]
fn frame_decode(mut ready: DecodeReady) -> usize {
    ready.socket.clear();
    ready.socket.extend_from_slice(&ready.wire);
    let frame = framing::split_frame(&mut ready.socket)
        .expect("framing")
        .expect("one frame")
        .freeze();
    let (decoded, _) = FrameKind::decode_shared(&frame, None).expect("decode");
    let FrameKind::ResourceOutput { bytes, .. } = decoded else {
        panic!("resource output");
    };
    black_box(bytes.len())
}

#[library_benchmark]
#[bench::append(setup = setup_agent)]
fn agent_append_replay(mut ready: AgentReady) -> usize {
    agent_session::icount_append_and_replay(&mut ready.actor, black_box(ready.payload))
        .expect("append")
}

#[library_benchmark]
#[bench::slice(setup = setup_history)]
fn history_slice(mut ready: HistoryReady) -> usize {
    ready
        .kernel
        .update(
            KernelInput::HistoryPage {
                terminal_id: &ready.terminal,
                stream_id: ready.stream,
                bootstrap_id: ready.bootstrap,
                page_seq: 1,
                rows: 256,
                payload: black_box(&ready.page),
                cursor: b"cursor-0",
                next_cursor: Some(b"cursor-1"),
            },
            &mut ready.effects,
        )
        .expect("history page");
    black_box(ready.effects.as_slice().len())
}

#[library_benchmark]
#[bench::resync(setup = setup_history)]
fn history_resync(mut ready: HistoryReady) -> usize {
    let replacement = BootstrapId::new(14).expect("replacement");
    ready
        .kernel
        .update(
            KernelInput::BootstrapBegin {
                terminal_id: &ready.terminal,
                stream_id: ready.stream,
                bootstrap_id: replacement,
                profile: stream_profile(),
                geometry: CanonicalGeometry::new(VT_COLS, VT_ROWS).expect("geometry"),
                base_seq: 1,
            },
            &mut ready.effects,
        )
        .expect("resync begin");
    ready
        .kernel
        .update(
            KernelInput::BootstrapChunk {
                terminal_id: &ready.terminal,
                stream_id: ready.stream,
                bootstrap_id: replacement,
                chunk_seq: 0,
                payload: black_box(b"replacement-ready"),
            },
            &mut ready.effects,
        )
        .expect("resync chunk");
    ready
        .kernel
        .update(
            KernelInput::BootstrapReady {
                terminal_id: &ready.terminal,
                stream_id: ready.stream,
                bootstrap_id: replacement,
                history_cursor: Some(b"replacement-cursor"),
            },
            &mut ready.effects,
        )
        .expect("resync ready");
    black_box(ready.effects.as_slice().len())
}

#[library_benchmark]
#[bench::commit(setup = setup_org)]
fn org_commit(mut ready: OrgReady) -> OrgReady {
    let commit = ready
        .org
        .state
        .commit_command(black_box(&ready.command), &phux_core::Actor::owner())
        .expect("commit");
    black_box(commit);
    // Returned, so tearing down the seeded state is not counted.
    ready
}

library_benchmark_group!(
    name = hot_path;
    benchmarks = frame_encode, frame_decode, agent_append_replay, history_slice, history_resync, org_commit
);

main!(library_benchmark_groups = hot_path);
