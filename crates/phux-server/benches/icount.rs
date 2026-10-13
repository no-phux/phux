//! Instruction counts for the hot paths the `perf-icount` job guards.
//!
//! Each function is one callgrind region. Setup runs outside that region.
//! Scenarios:
//!
//! - VT apply: libghostty parses a mixed text and SGR stream.
//! - Frame encode and decode: one `RESOURCE_OUTPUT` through the live-path
//!   writer and the client `split_frame` + `decode_shared` read.
//! - Agent append and replay: validate, stamp, retain, and copy the ring.
//! - Client-kernel history slice and resync: the session kernel, with a
//!   stand-in engine so the count is the kernel rather than a second
//!   libghostty.
//! - PTY batch handoff: the burst copy the actor performs for one wakeup.

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
use libghostty_vt::Terminal as GhosttyTerminal;
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
use phux_server::resource::agent_session::{self, AgentSessionActor};
use phux_server::resource::terminal::icount_join_pty_burst;

const VT_COLS: u16 = 80;
const VT_ROWS: u16 = 24;
const FRAME_BYTES: usize = 4 * 1024;
const HISTORY_BYTES: usize = 8 * 1024;
const PTY_CHUNKS: usize = 47;
const PTY_CHUNK_BYTES: usize = 1024;
const AGENT_RECORDS: usize = 32;

struct VtReady {
    terminal: GhosttyTerminal<'static, 'static>,
    bytes: Vec<u8>,
}

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

fn vt_bytes() -> Vec<u8> {
    let line = b"\x1b[38;5;2mthe quick brown fox jumps over the lazy dog\x1b[0m\r\n";
    let mut out = Vec::with_capacity(8192);
    while out.len() < 8192 {
        out.extend_from_slice(line);
    }
    out
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

fn setup_vt() -> VtReady {
    let mut terminal = GhosttyTerminal::new(VT_COLS, VT_ROWS).expect("terminal");
    // One warm write so the measured call is steady-state parsing.
    terminal.vt_write(b"\x1b[H");
    VtReady {
        terminal,
        bytes: vt_bytes(),
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

fn setup_pty() -> Vec<Bytes> {
    (0..PTY_CHUNKS)
        .map(|index| Bytes::from(vec![u8::try_from(index).unwrap_or(0); PTY_CHUNK_BYTES]))
        .collect()
}

#[library_benchmark]
#[bench::apply(setup = setup_vt)]
fn vt_apply(mut ready: VtReady) -> usize {
    ready.terminal.vt_write(black_box(&ready.bytes));
    black_box(&ready.terminal);
    ready.bytes.len()
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
#[bench::join(setup = setup_pty)]
fn pty_batch_handoff(chunks: Vec<Bytes>) -> usize {
    black_box(icount_join_pty_burst(black_box(chunks)).len())
}

library_benchmark_group!(
    name = hot_path;
    benchmarks = vt_apply, frame_encode, frame_decode, agent_append_replay, history_slice, history_resync, pty_batch_handoff
);

main!(library_benchmark_groups = hot_path);
