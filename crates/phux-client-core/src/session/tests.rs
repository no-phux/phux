use phux_protocol::caps::{
    BootstrapCapabilities, ClientCapabilities, EngineCodec, EngineFeatureSet,
    select_bootstrap_profile,
};
use phux_protocol::input::{InputEvent, focus::FocusEvent};
use phux_protocol::wire::frame::{
    AgentEvent, CloseReason, ControlAction, ResourceLifecycle, TombstoneReason,
};
use phux_protocol::{
    BootstrapId, BootstrapProfile, BootstrapStreamProfile, ResourceId, ResourceKind, StreamId,
};

use super::agent_stream::{AgentRecordMode, AgentSessionStatus};
use super::{
    AgentStreamInfo, EffectBuffer, HistoryRejectionReason, HistoryUnavailableReason,
    InputBlockReason, InputEligibility, KernelAction, KernelDamage, KernelDamageKind, KernelEffect,
    KernelError, KernelInput, KernelJob, KernelSend, KernelStatus, MAX_BOOTSTRAP_STAGING_BYTES,
    MAX_BOOTSTRAP_STAGING_CHUNKS, MAX_BOOTSTRAP_STAGING_IDLE_MS, SessionKernel, TombstoneRecord,
};
use crate::engine::{
    BootstrapProgress, CanonicalGeometry, EngineAdapter, EngineDamage, EngineEffect,
    EngineEffectBuffer, EngineJob, EngineStatus, HistoryApplyOutcome,
};
use crate::history::HistoryLoadState;

use super::test_rig::*;

/// The session rig over the synthesized [`FakeAdapter`].
type Rig<E = FakeAdapter> = super::test_rig::Rig<E>;

#[cfg(feature = "native-engine")]
use crate::engine::ghostty::GhosttyAdapter;
#[cfg(feature = "native-engine")]
use crate::engine::ghostty::snapshot::SnapshotCaptureLimits;
#[cfg(feature = "native-engine")]
use phux_protocol::wire::frame::FrameKind;

#[cfg(feature = "native-engine")]
mod native_source;

const READY_MARKER: &[u8] = b"<SYNTHESIZED_VT_V1_READY>";

#[derive(Debug, Clone, Copy)]
enum ReadyMode {
    ChunkFirst,
    ProtocolFirst,
}

#[derive(Debug, thiserror::Error)]
enum FakeError {
    #[error("fake adapter only accepts synthesized VT profiles")]
    UnsupportedProfile,
    #[error("fake adapter mutated before failing")]
    MutatedThenFailed,
}

struct FakeAdapter {
    ready_mode: ReadyMode,
}

struct FakeReplica {
    geometry: CanonicalGeometry,
    transcript: Vec<u8>,
    finish_effects: bool,
    reported_staging_bytes: usize,
}

impl EngineAdapter for FakeAdapter {
    type Replica = FakeReplica;
    type Error = FakeError;

    fn start_replica(
        &mut self,
        profile: BootstrapStreamProfile,
        geometry: CanonicalGeometry,
    ) -> Result<Self::Replica, Self::Error> {
        if !matches!(
            profile,
            BootstrapStreamProfile::SynthesizedVtRaw
                | BootstrapStreamProfile::SynthesizedVtStateSync
                | BootstrapStreamProfile::NativeState {
                    codec: EngineCodec::LibghosttySnapshotV1,
                }
        ) {
            return Err(FakeError::UnsupportedProfile);
        }
        Ok(FakeReplica {
            geometry,
            transcript: Vec::new(),
            finish_effects: false,
            reported_staging_bytes: 0,
        })
    }

    fn apply_bootstrap_chunk(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        replica.transcript.extend_from_slice(payload);
        if payload == b"mutate-then-error" {
            effects.push(EngineEffect::Damage(EngineDamage::Full));
            return Err(FakeError::MutatedThenFailed);
        }
        if payload == b"bootstrap-effects" {
            replica.finish_effects = true;
            effects.push(EngineEffect::Damage(EngineDamage::Full));
            effects.push(EngineEffect::Status(EngineStatus::Title(
                "bootstrap-staging".to_owned(),
            )));
            effects.push(EngineEffect::Job(EngineJob::Wakeup));
        } else {
            effects.push(EngineEffect::Damage(EngineDamage::Full));
        }
        if matches!(self.ready_mode, ReadyMode::ChunkFirst)
            && replica.transcript.ends_with(READY_MARKER)
        {
            Ok(BootstrapProgress::Ready)
        } else {
            Ok(BootstrapProgress::Pending)
        }
    }

    fn bootstrap_staging_bytes(&self, replica: &Self::Replica) -> usize {
        replica
            .transcript
            .capacity()
            .saturating_add(replica.reported_staging_bytes)
    }

    fn finish_bootstrap(
        &mut self,
        replica: &mut Self::Replica,
        effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        if replica.transcript.ends_with(b"<FINISH_ERROR>") {
            replica.transcript.extend_from_slice(b"-mutated");
            return Err(FakeError::MutatedThenFailed);
        }
        if replica.finish_effects {
            effects.push(EngineEffect::Damage(EngineDamage::Full));
            effects.push(EngineEffect::Status(EngineStatus::Title(
                "bootstrap-finished".to_owned(),
            )));
            effects.push(EngineEffect::Job(EngineJob::Wakeup));
        }
        if !replica.transcript.ends_with(READY_MARKER) {
            return Ok(BootstrapProgress::Pending);
        }
        Ok(match self.ready_mode {
            ReadyMode::ChunkFirst => BootstrapProgress::Finished,
            ReadyMode::ProtocolFirst => BootstrapProgress::Ready,
        })
    }

    fn apply_history_page(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        declared_rows: u32,
        effects: &mut EngineEffectBuffer,
    ) -> Result<HistoryApplyOutcome, Self::Error> {
        if payload == b"history-error" {
            effects.push(EngineEffect::Damage(EngineDamage::Full));
            return Err(FakeError::MutatedThenFailed);
        }
        replica.transcript.extend_from_slice(payload);
        if payload == b"history-effects" {
            effects.push(EngineEffect::Status(EngineStatus::Title(
                "history-imported".to_owned(),
            )));
        }
        let finished = payload == b"history-finish";
        Ok(HistoryApplyOutcome {
            progress: if finished {
                BootstrapProgress::Finished
            } else {
                BootstrapProgress::Ready
            },
            retained_rows: if payload == b"history-not-retained" {
                0
            } else {
                declared_rows as usize
            },
            authenticated_rows: declared_rows as usize,
        })
    }

    fn apply_output(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<(), Self::Error> {
        replica.transcript.extend_from_slice(payload);
        if payload == b"mutate-then-error" {
            effects.push(EngineEffect::Damage(EngineDamage::Full));
            return Err(FakeError::MutatedThenFailed);
        }
        if payload == b"effects" {
            effects.push(EngineEffect::Damage(EngineDamage::Rows {
                first: 2,
                last: 4,
            }));
            effects.push(EngineEffect::Status(EngineStatus::Bell));
            effects.push(EngineEffect::Job(EngineJob::Wakeup));
        } else {
            effects.push(EngineEffect::Damage(EngineDamage::Full));
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
enum NativeRecord {
    Bootstrap(Vec<u8>),
    Ready,
    Live(Vec<u8>),
    History(Vec<u8>),
}

struct RecordingNativeReplica {
    records: Vec<NativeRecord>,
    chunk_count: usize,
}

struct RecordingNativeAdapter;

impl EngineAdapter for RecordingNativeAdapter {
    type Replica = RecordingNativeReplica;
    type Error = FakeError;

    fn start_replica(
        &mut self,
        profile: BootstrapStreamProfile,
        _geometry: CanonicalGeometry,
    ) -> Result<Self::Replica, Self::Error> {
        if !matches!(
            profile,
            BootstrapStreamProfile::NativeState {
                codec: EngineCodec::LibghosttySnapshotV1,
            }
        ) {
            return Err(FakeError::UnsupportedProfile);
        }
        Ok(RecordingNativeReplica {
            records: Vec::new(),
            chunk_count: 0,
        })
    }

    fn apply_bootstrap_chunk(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        _effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        replica
            .records
            .push(NativeRecord::Bootstrap(payload.to_vec()));
        replica.chunk_count += 1;
        Ok(if replica.chunk_count == 2 {
            BootstrapProgress::Ready
        } else {
            BootstrapProgress::Pending
        })
    }

    fn finish_bootstrap(
        &mut self,
        replica: &mut Self::Replica,
        _effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        replica.records.push(NativeRecord::Ready);
        Ok(BootstrapProgress::Finished)
    }

    fn apply_history_page(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        declared_rows: u32,
        _effects: &mut EngineEffectBuffer,
    ) -> Result<HistoryApplyOutcome, Self::Error> {
        replica
            .records
            .push(NativeRecord::History(payload.to_vec()));
        Ok(HistoryApplyOutcome {
            progress: if payload == b"finish" {
                BootstrapProgress::Finished
            } else {
                BootstrapProgress::Ready
            },
            retained_rows: declared_rows as usize,
            authenticated_rows: declared_rows as usize,
        })
    }

    fn apply_output(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        _effects: &mut EngineEffectBuffer,
    ) -> Result<(), Self::Error> {
        replica.records.push(NativeRecord::Live(payload.to_vec()));
        Ok(())
    }
}

const fn event_input<'a>(terminal_id: &'a ResourceId, event: &'a AgentEvent) -> KernelInput<'a> {
    KernelInput::Event { terminal_id, event }
}

fn kernel(mode: ReadyMode) -> SessionKernel<FakeAdapter> {
    kernel_with_profile(mode, BootstrapProfile::SynthesizedVtRaw)
}

fn kernel_with_profile(mode: ReadyMode, profile: BootstrapProfile) -> SessionKernel<FakeAdapter> {
    SessionKernel::new(FakeAdapter { ready_mode: mode }, profile)
}

const NATIVE_STREAM: BootstrapStreamProfile = BootstrapStreamProfile::NativeState {
    codec: EngineCodec::LibghosttySnapshotV1,
};

const NATIVE: BootstrapProfile = BootstrapProfile::NativeState {
    codec: EngineCodec::LibghosttySnapshotV1,
    features: EngineFeatureSet::required_native(),
};

impl Rig<FakeAdapter> {
    fn new(mode: ReadyMode) -> Self {
        Self::of(kernel(mode))
    }
}

impl Rig<FakeAdapter> {
    fn begin(&mut self, g: &Gen, base_seq: u64) {
        self.ok(g.begin(base_seq));
    }

    /// The synthesized transcript, fragmented across four chunks.
    fn push_ready_transcript(&mut self, g: &Gen) {
        for (chunk_seq, payload) in (0_u32..).zip(ready_chunks()) {
            self.ok(g.chunk(chunk_seq, payload));
        }
    }

    fn publish(&mut self, g: &Gen, base_seq: u64) {
        self.begin(g, base_seq);
        self.push_ready_transcript(g);
        self.ok(g.ready(None));
    }

    fn publish_with_history(&mut self, g: &Gen, base_seq: u64, cursor: &[u8]) {
        self.begin(g, base_seq);
        self.push_ready_transcript(g);
        self.ok(g.ready(Some(cursor)));
    }

    fn transcript(&self, id: &ResourceId) -> &[u8] {
        &self.k.published(id).unwrap().engine().transcript
    }

    fn staging_bytes(&mut self, id: &ResourceId) -> &mut usize {
        &mut self
            .k
            .lanes
            .terminal_mut(id)
            .and_then(|state| state.staging.as_mut())
            .expect("staging")
            .accepted_bytes
    }
}

fn ready_chunks() -> [&'static [u8]; 4] {
    [
        b"\x1b[2Jsynthe",
        b"sized-vt-v1",
        &READY_MARKER[..9],
        &READY_MARKER[9..],
    ]
}

#[allow(clippy::unnecessary_wraps, reason = "compared with `tombstone()`")]
fn codec_failure(last_valid_seq: u64) -> Option<TombstoneRecord> {
    Some(TombstoneRecord {
        reason: TombstoneReason::CodecFailure,
        last_valid_seq,
    })
}

const fn subscribe_all() -> KernelEffect {
    KernelEffect::Send(KernelSend::SubscribeEvents {
        terminal: None,
        after_seq: None,
    })
}

fn damage(terminal_id: &ResourceId, kind: KernelDamageKind) -> KernelEffect {
    KernelEffect::Damage(KernelDamage {
        terminal_id: terminal_id.clone(),
        kind,
    })
}

fn exited(terminal_id: &ResourceId) -> KernelEffect {
    KernelEffect::Status(KernelStatus::Exited {
        terminal_id: terminal_id.clone(),
        exit_status: None,
        signal: None,
        reason: CloseReason::Unknown,
    })
}

fn requests_history(effects: &EffectBuffer, expected: &[u8]) -> bool {
    any_effect!(effects, KernelEffect::Send(KernelSend::HistoryRequest { cursor, .. }) if cursor == expected)
}

#[test]
fn release_terminal_preserves_initial_attach_inventory_and_barrier() {
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    let id = terminal(1);
    rig.ok(attach_started(10, std::slice::from_ref(&id)));
    assert!(!rig.k.release_terminal(&id));
    assert!(rig.k.active_attach_contains(&id));
    rig.ok(closed(&id));
    assert!(!rig.k.release_terminal(&id));
    assert!(rig.k.closed.contains(&id));
    rig.ok(attach_ready(10));
    rig.k.release_active_attach();
    assert!(rig.k.release_terminal(&id));
    assert!(!rig.k.closed.contains(&id));
}

#[test]
fn explicit_detach_cannot_bypass_initial_barrier_but_withdraws_released_participation() {
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    let g = gen_of(1, 1, 1);
    rig.ok(attach_started(10, std::slice::from_ref(&g.id)));
    assert!(!rig.k.detach_terminal(&g.id));
    assert!(rig.k.active_attach_contains(&g.id));
    assert!(rig.up(attach_ready(10)).is_err());
    rig.publish(&g, 0);
    rig.ok(attach_ready(10));
    assert!(!rig.k.release_terminal(&g.id));
    rig.fx.clear();
    assert!(rig.k.detach_terminal(&g.id));
    assert!(!rig.k.active_attach_contains(&g.id));
    assert!(rig.k.lanes.is_empty());
    assert!(rig.k.closed.is_empty());
    for n in 2..=512 {
        let next = gen_of(n, n.into(), 1);
        rig.publish(&next, 0);
        assert!(rig.k.detach_terminal(&next.id));
        assert!(rig.k.lanes.is_empty());
        assert!(rig.k.closed.is_empty());
        rig.fx.clear();
    }
}

#[test]
fn release_terminal_reclaims_churn_and_allows_explicit_subscription_replacement() {
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    for n in 1..=512 {
        let g = gen_of(n, 1, 1);
        rig.publish(&g, 0);
        assert!(rig.k.release_terminal(&g.id));
        assert!(rig.k.lanes.is_empty());
        assert!(rig.k.closed.is_empty());
        // Same ID can acquire a new explicitly admitted subscription.
        rig.begin(&gen_of(n, 2, 1), 0);
        rig.ok(closed(&g.id));
        assert!(rig.k.closed.contains(&g.id));
        assert!(rig.k.release_terminal(&g.id));
        assert!(rig.k.lanes.is_empty());
        assert!(rig.k.closed.is_empty());
        rig.fx.clear();
    }
}

#[test]
fn close_transfers_the_final_replica_without_reopening_the_terminal() {
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    let g = gen_of(1, 7, 3);
    rig.publish_with_history(&g, 0, b"cursor-0");
    rig.ok(g.page(b"cursor-0", Some(b"cursor-1"), b"finished-history", 2));
    rig.k.set_retain_replica_on_close(&g.id, true);

    rig.ok(closed(&g.id));

    assert!(rig.k.published(&g.id).is_none());
    assert!(matches!(
        rig.k.input_eligibility(&g.id),
        InputEligibility::Ineligible(InputBlockReason::Closed)
    ));
    let replica = rig.k.take_closed_replica(&g.id).expect("final replica");
    assert_eq!(replica.key().stream_id, g.s);
    assert_eq!(replica.key().bootstrap_id, g.b);
    assert_eq!(replica.geometry(), geometry());
    assert!(replica.engine().transcript.ends_with(b"finished-history"));
    assert_eq!(replica.history().status().materialized_rows, 2);
    assert!(rig.k.take_closed_replica(&g.id).is_none());

    // Without retention the final replica is dropped.
    let other = gen_of(2, 7, 3);
    rig.publish(&other, 0);
    rig.ok(closed(&other.id));
    assert!(rig.k.take_closed_replica(&other.id).is_none());
}

#[test]
fn bootstrap_staging_rejects_many_legal_chunks_before_excess_allocation() {
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    let g = gen_of(1, 1, 1);
    rig.begin(&g, 7);
    let limit = u32::try_from(MAX_BOOTSTRAP_STAGING_CHUNKS).unwrap();
    for chunk_seq in 0..limit {
        rig.up(g.chunk(chunk_seq, &[]))
            .expect("individually legal bounded chunk");
    }
    let error = rig
        .up(g.chunk(limit, &[]))
        .expect_err("aggregate chunk ceiling must terminate staging");
    assert!(matches!(error, KernelError::BootstrapStagingLimitExceeded));
    assert!(rig.k.staging(&g.id).is_none());
    assert_eq!(rig.tomb(&g), codec_failure(7));
}

#[test]
fn bootstrap_staging_byte_ceiling_is_connection_wide() {
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    let first = gen_of(1, 1, 1);
    let second = gen_of(2, 1, 1);
    rig.begin(&first, 7);
    rig.begin(&second, 9);
    *rig.staging_bytes(&first.id) = MAX_BOOTSTRAP_STAGING_BYTES;

    let error = rig
        .up(second.chunk(0, b"x"))
        .expect_err("another generation may not exceed the connection budget");
    assert!(matches!(error, KernelError::BootstrapStagingLimitExceeded));
    assert!(rig.k.staging(&first.id).is_some());
    assert!(rig.k.staging(&second.id).is_none());
    assert_eq!(rig.tomb(&second), codec_failure(9));
}

#[test]
fn bootstrap_staging_ceiling_counts_engine_and_pending_effect_capacity() {
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    let g = gen_of(1, 1, 1);
    rig.begin(&g, 9);
    rig.k
        .lanes
        .terminal_mut(&g.id)
        .and_then(|state| state.staging.as_mut())
        .expect("staging")
        .engine
        .reported_staging_bytes = MAX_BOOTSTRAP_STAGING_BYTES;

    let error = rig
        .up(g.chunk(0, b"bootstrap-effects"))
        .expect_err("engine and pending effects count against retained capacity");
    assert!(matches!(error, KernelError::BootstrapStagingLimitExceeded));
    assert!(rig.k.staging(&g.id).is_none());
}

#[test]
fn bootstrap_staging_lifetime_is_checked_before_adapter_input_and_at_protocol_ready() {
    for mode in [ReadyMode::ChunkFirst, ReadyMode::ProtocolFirst] {
        let mut rig = Rig::new(mode);
        let g = gen_of(1, 1, 1);
        rig.begin(&g, 11);
        let late = MAX_BOOTSTRAP_STAGING_IDLE_MS + 1;
        let input = match mode {
            ReadyMode::ChunkFirst => g.chunk(0, b"not-applied"),
            ReadyMode::ProtocolFirst => g.ready(None),
        };
        let error = rig
            .k
            .update_at(late, input, &mut rig.fx)
            .expect_err("expired staging must fail");
        assert!(matches!(error, KernelError::BootstrapStagingLimitExceeded));
        assert!(rig.k.staging(&g.id).is_none());
        assert_eq!(rig.tomb(&g), codec_failure(11));
    }
}

#[test]
fn progressive_native_ready_requires_cursor_until_finish_is_authenticated() {
    let mut rig = Rig::of(kernel_with_profile(ReadyMode::ProtocolFirst, NATIVE));
    let g = gen_of(1, 1, 1);
    rig.up(g.begin_as(NATIVE_STREAM, 13))
        .expect("begin progressive native bootstrap");
    rig.up(g.chunk(0, READY_MARKER)).expect("engine READY");
    let error = rig
        .up(g.ready(None))
        .expect_err("v1 must receive and authenticate FINISH through history");
    assert!(matches!(
        error,
        KernelError::HistoryCompletionMismatch {
            progress: BootstrapProgress::Ready,
            has_more: false,
        }
    ));
    assert!(rig.k.staging(&g.id).is_none());
    assert_eq!(rig.tomb(&g), codec_failure(13));
}

/// Encode `frame` on the wire, decode it back, and feed the decoded frame.
#[cfg(feature = "native-engine")]
fn apply_decoded_native_frame(rig: &mut Rig<GhosttyAdapter>, frame: &FrameKind) {
    let mut encoded = (&[][..]).into();
    frame.encode(&mut encoded);
    let (decoded, tail) = FrameKind::decode(&encoded).expect("wire decode");
    assert!(tail.is_empty());
    let result = match decoded {
        FrameKind::BootstrapBegin {
            terminal_id,
            stream_id,
            bootstrap_id,
            profile,
            cols,
            rows,
            base_seq,
        } => {
            let geometry = CanonicalGeometry::new(cols, rows).expect("wire geometry");
            let input = begin_input(
                &terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                geometry,
                base_seq,
            );
            rig.up(input)
        }
        FrameKind::BootstrapChunk {
            terminal_id,
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload,
        } => rig.up(chunk(
            &terminal_id,
            stream_id,
            bootstrap_id,
            chunk_seq,
            &payload,
        )),
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor,
        } => rig.up(ready(
            &terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor.as_deref(),
        )),
        FrameKind::ResourceOutput {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
            bytes,
        } => rig.up(output(&terminal_id, stream_id, bootstrap_id, seq, &bytes)),
        FrameKind::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            page_seq,
            cursor,
            next_cursor,
            payload,
            rows,
        } => rig.up(KernelInput::HistoryPage {
            terminal_id: &terminal_id,
            stream_id,
            bootstrap_id,
            page_seq,
            rows,
            payload: &payload,
            cursor: &cursor,
            next_cursor: next_cursor.as_deref(),
        }),
        other => panic!("unexpected native test frame: {other:?}"),
    };
    result.expect("wire frame applies");
}

#[cfg(feature = "native-engine")]
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the acceptance test keeps real capture, wire framing, publication, and interleaving in one end-to-end proof"
)]
fn real_native_wire_kernel_path_publishes_then_imports_multiple_pages() {
    let capture = native_source::capture(SnapshotCaptureLimits {
        max_bytes: 64 * 1024 * 1024,
        max_record_bytes: 64 * 1024,
        max_records: 10_000,
        max_rows: 10_000,
    });
    let ready = &capture.bytes[..capture.prefix_records.last().unwrap().end];
    let units: Vec<_> = capture
        .history
        .iter()
        .map(|unit| {
            (
                capture.bytes.slice(unit.range.clone()),
                u32::try_from(unit.rows).expect("rows"),
                unit.finish,
            )
        })
        .collect();
    assert!(units.len() >= 3, "two pages plus FINISH are required");

    let Gen {
        id: terminal_id,
        s: stream_id,
        b: bootstrap_id,
    } = gen_of(91, 92, 93);
    let limits = phux_protocol::BootstrapLimits::default();
    let mut rig = Rig::of(SessionKernel::new(GhosttyAdapter::new(limits), NATIVE));
    apply_decoded_native_frame(
        &mut rig,
        &FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile: NATIVE_STREAM,
            cols: 200,
            rows: 3,
            base_seq: 0,
        },
    );
    for (chunk_seq, payload) in ready.chunks(64 * 1024).enumerate() {
        apply_decoded_native_frame(
            &mut rig,
            &FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: u32::try_from(chunk_seq).expect("chunk sequence"),
                payload: payload.to_vec().into(),
            },
        );
    }
    let cursor = b"real-native-cursor".to_vec();
    apply_decoded_native_frame(
        &mut rig,
        &FrameKind::BootstrapReady {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: Some(cursor.clone().into()),
        },
    );
    assert!(
        rig.k.published(&terminal_id).is_some(),
        "READY publishes before history"
    );

    for (index, (payload, rows, finish)) in units.iter().enumerate() {
        let _ = rig.k.prefetch_history(&terminal_id, 0, &mut rig.fx);
        if index == 1 {
            apply_decoded_native_frame(
                &mut rig,
                &FrameKind::ResourceOutput {
                    terminal_id: terminal_id.clone(),
                    stream_id,
                    bootstrap_id,
                    seq: 1,
                    bytes: b"live-between-pages\r\n".to_vec().into(),
                },
            );
        }
        apply_decoded_native_frame(
            &mut rig,
            &FrameKind::HistoryPage {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                page_seq: u64::try_from(index + 1).expect("page sequence"),
                cursor: cursor.clone().into(),
                next_cursor: (!finish).then(|| cursor.clone().into()),
                payload: payload.clone(),
                rows: *rows,
            },
        );
    }
    assert_eq!(rig.history_state(&terminal_id), HistoryLoadState::Complete);
}

#[test]
fn silent_bootstrap_staging_expires_on_driver_tick() {
    let mut rig = Rig::new(ReadyMode::ProtocolFirst);
    let first = gen_of(1, 1, 1);
    let second = gen_of(2, 1, 1);
    rig.begin(&first, 7);
    rig.begin(&second, 9);

    assert_eq!(
        rig.k
            .expire_bootstrap_staging(MAX_BOOTSTRAP_STAGING_IDLE_MS, &mut rig.fx),
        0,
        "the exact lifetime remains valid"
    );
    assert_eq!(
        rig.k
            .expire_bootstrap_staging(MAX_BOOTSTRAP_STAGING_IDLE_MS + 1, &mut rig.fx),
        2
    );
    assert!(rig.k.staging(&first.id).is_none());
    assert!(rig.k.staging(&second.id).is_none());
    let resyncs = rig.fx.as_slice().iter().filter(|effect| {
        matches!(
            effect,
            KernelEffect::Status(KernelStatus::ResyncRequired { .. })
        )
    });
    assert_eq!(resyncs.count(), 2);
}

#[test]
fn dual_ready_orders_and_fragmentation_hold_first_damage() {
    for mode in [ReadyMode::ChunkFirst, ReadyMode::ProtocolFirst] {
        let g = gen_of(1, 11, 21);
        let mut rig = Rig::new(mode);

        rig.ok(attach_started(7, std::slice::from_ref(&g.id)));
        rig.begin(&g, 100);
        rig.push_ready_transcript(&g);

        let staging = rig.k.staging(&g.id).unwrap();
        assert_eq!(staging.geometry(), geometry());
        assert_eq!(staging.engine().geometry, geometry());
        assert_eq!(
            staging.engine_ready(),
            matches!(mode, ReadyMode::ChunkFirst)
        );
        assert!(!staging.protocol_ready());
        assert!(rig.k.published(&g.id).is_none());
        assert!(rig.fx.is_empty());

        rig.ok(g.ready(None));
        let published = rig.k.published(&g.id).unwrap();
        assert_eq!(published.geometry(), geometry());
        assert_eq!(published.engine().geometry, geometry());
        assert_eq!(published.last_seq(), 100);
        assert_eq!(
            published.key().profile,
            BootstrapStreamProfile::SynthesizedVtRaw
        );
        assert!(published.engine().transcript.ends_with(READY_MARKER));
        assert!(rig.fx.is_empty(), "publication is behind ATTACH_READY");

        rig.ok(attach_ready(7));
        assert_eq!(
            rig.fx.as_slice(),
            &[subscribe_all(), damage(&g.id, KernelDamageKind::Full)]
        );
    }
}

#[test]
fn chunk_sequence_rejects_duplicates_and_gaps_without_applying() {
    let g = gen_of(2, 12, 22);
    let mut rig = Rig::new(ReadyMode::ProtocolFirst);
    rig.begin(&g, 0);

    rig.ok(g.chunk(0, b"first"));
    assert!(matches!(
        rig.up(g.chunk(0, b"duplicate")),
        Err(KernelError::DuplicateChunk {
            expected: 1,
            actual: 0
        })
    ));
    assert!(matches!(
        rig.up(g.chunk(2, b"gap")),
        Err(KernelError::ChunkGap {
            expected: 1,
            actual: 2
        })
    ));
    assert_eq!(rig.k.staging(&g.id).unwrap().engine().transcript, b"first");
}

#[test]
fn raw_sequence_ids_and_tombstones_are_exact() {
    let g = gen_of(3, 13, 23);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.publish(&g, 41);

    rig.ok(g.output(42, b"first-live"));
    assert_eq!(rig.k.published(&g.id).unwrap().last_seq(), 42);
    let applied = rig.transcript(&g.id).to_vec();

    assert!(matches!(
        rig.up(g.output(42, b"duplicate")),
        Err(KernelError::DuplicateSequence {
            expected: 43,
            actual: 42
        })
    ));
    assert!(matches!(
        rig.up(g.output(44, b"gap")),
        Err(KernelError::SequenceGap {
            expected: 43,
            actual: 44
        })
    ));
    let wrong_stream = Gen {
        s: stream(999),
        ..g.clone()
    };
    assert!(matches!(
        rig.up(wrong_stream.output(43, b"wrong-stream")),
        Err(KernelError::GenerationMismatch { .. })
    ));
    assert!(matches!(
        rig.up(g.with_bootstrap(999).output(43, b"wrong-bootstrap")),
        Err(KernelError::GenerationMismatch { .. })
    ));
    let wrong_terminal = Gen {
        id: terminal(999),
        ..g.clone()
    };
    assert!(matches!(
        rig.up(wrong_terminal.output(43, b"wrong-terminal")),
        Err(KernelError::UnknownTerminal(_))
    ));
    assert_eq!(rig.transcript(&g.id), applied);

    rig.ok(g.tombstone(TombstoneReason::OutboundGap, 42));
    assert_eq!(rig.transcript(&g.id), applied);
    assert_eq!(rig.tomb(&g).unwrap().last_valid_seq, 42);
    assert_eq!(
        rig.k.input_eligibility(&g.id),
        InputEligibility::Ineligible(InputBlockReason::FrozenReplica)
    );
    assert!(matches!(
        rig.up(g.output(43, b"stale")),
        Err(KernelError::RetiredGeneration { .. })
    ));
    let replacement = g.with_bootstrap(24);
    rig.begin(&replacement, 42);
    rig.push_ready_transcript(&replacement);
    assert_eq!(
        rig.k.published(&g.id).unwrap().key().bootstrap_id,
        g.b,
        "frozen view remains visible while replacement stages"
    );
    rig.ok(replacement.ready(None));
    assert_eq!(
        rig.k.published(&g.id).unwrap().key().bootstrap_id,
        replacement.b
    );
    assert_eq!(
        rig.tomb(&g),
        Some(TombstoneRecord {
            reason: TombstoneReason::OutboundGap,
            last_valid_seq: 42,
        }),
        "replacement publication must not overwrite the authoritative tombstone"
    );
}

#[test]
fn published_history_is_generation_bound_and_interleaves_without_advancing_live_seq() {
    let g = gen_of(30, 130, 230);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.publish_with_history(&g, 40, b"cursor-0");

    rig.ok(g.page(b"cursor-0", Some(b"cursor-1"), b"history-one", 0));
    assert!(requests_history(&rig.fx, b"cursor-1"));
    assert_eq!(rig.k.published(&g.id).unwrap().last_seq(), 40);

    rig.ok(g.output(41, b"live-between-history"));
    assert!(!rig.k.prefetch_history(&g.id, 0, &mut rig.fx));
    rig.ok(g.page(b"cursor-1", Some(b"cursor-2"), b"history-two", 0));
    assert_eq!(rig.k.published(&g.id).unwrap().last_seq(), 41);
    assert!(
        rig.transcript(&g.id)
            .ends_with(b"history-onelive-between-historyhistory-two")
    );

    let before_wrong_generation = rig.transcript(&g.id).to_vec();
    let wrong = g.with_bootstrap(999);
    let wrong_generation =
        rig.up(wrong.page(b"wrong-cursor", Some(b"wrong-next"), b"wrong-generation", 0));
    assert!(matches!(
        wrong_generation,
        Err(KernelError::GenerationMismatch { .. })
    ));
    assert_eq!(rig.transcript(&g.id), before_wrong_generation);
}

#[test]
fn delayed_history_tombstone_for_an_advanced_cursor_is_ignored() {
    let g = gen_of(31, 131, 231);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.publish_with_history(&g, 40, b"cursor-0");
    rig.fx.clear();

    rig.ok(g.page(b"cursor-0", Some(b"cursor-1"), b"history-one", 0));
    assert_eq!(
        rig.history_state(&g.id),
        HistoryLoadState::Loading,
        "the first page advances the outstanding request to cursor-1"
    );
    rig.fx.clear();

    let delayed = rig.up(KernelInput::HistoryTombstone {
        terminal_id: &g.id,
        stream_id: g.s,
        bootstrap_id: g.b,
        cursor: b"cursor-0",
        reason: HistoryUnavailableReason::Expired,
    });
    assert!(
        delayed.is_ok(),
        "a late tombstone for cursor-0 must not disconnect the live pane: {delayed:?}"
    );
    assert_eq!(
        rig.history_state(&g.id),
        HistoryLoadState::Loading,
        "the newer cursor remains usable"
    );
    assert!(
        rig.fx.is_empty(),
        "ignoring an obsolete cursor must not change visible history state"
    );
}

#[test]
fn retained_history_limit_and_prefetch_threshold_are_explicit() {
    let g = gen_of(35, 135, 235);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.publish_with_history(&g, 40, b"cursor-0");
    rig.fx.clear();
    rig.ok(g.page(b"cursor-0", Some(b"cursor-1"), b"history-page", 128));
    assert!(!any_effect!(
        rig.fx,
        KernelEffect::Send(KernelSend::HistoryRequest { .. })
    ));
    assert!(rig.k.prefetch_history(&g.id, 0, &mut rig.fx));

    let limited = Gen {
        id: terminal(36),
        ..g
    };
    rig.publish_with_history(&limited, 50, b"limited-0");
    rig.fx.clear();
    rig.ok(limited.page(b"limited-0", Some(b"limited-1"), b"history-not-retained", 1));
    assert!(requests_history(&rig.fx, b"limited-1"));
    assert_eq!(rig.history_state(&limited.id), HistoryLoadState::Loading);
    rig.fx.clear();
    rig.ok(limited.page(b"limited-1", None, b"finish", 0));
    let status = rig.k.history_cache(&limited.id).unwrap().status();
    assert_eq!(status.state, HistoryLoadState::Complete);
    assert_eq!(status.materialized_rows, 0);
}

#[test]
fn busy_history_retries_are_automatic_and_bounded() {
    let g = gen_of(37, 137, 237);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.publish_with_history(&g, 50, b"busy-cursor");

    for retry in 0..=64 {
        rig.fx.clear();
        rig.ok(g.rejected(b"busy-cursor", HistoryRejectionReason::Busy, 1, 1));
        assert_eq!(requests_history(&rig.fx, b"busy-cursor"), retry < 64);
    }
    assert_eq!(rig.history_state(&g.id), HistoryLoadState::Tombstoned);
}

#[test]
fn grown_history_limits_survive_busy_then_reach_authenticated_finish() {
    let g = gen_of(38, 138, 238);
    let mut rig = Rig::of(SessionKernel::with_history_config(
        FakeAdapter {
            ready_mode: ReadyMode::ChunkFirst,
        },
        NATIVE,
        crate::history::HistoryCacheConfig {
            prefetch_rows: 4_096,
            ..crate::history::HistoryCacheConfig::default()
        },
    ));
    rig.ok(g.begin_as(NATIVE_STREAM, 50));
    rig.push_ready_transcript(&g);
    rig.ok(g.ready(Some(b"grown-cursor")));

    let grown_bytes = 2 * 1024 * 1024;
    let grown_rows = 2_048;
    let requests = |effects: &EffectBuffer, bytes: u32, rows: u32| {
        any_effect!(effects, KernelEffect::Send(KernelSend::HistoryRequest {
            max_bytes,
            max_rows,
            ..
        }) if *max_bytes == bytes && *max_rows == rows)
    };
    for reason in [
        HistoryRejectionReason::TooSmall,
        HistoryRejectionReason::Busy,
    ] {
        rig.fx.clear();
        rig.ok(g.rejected(b"grown-cursor", reason, grown_bytes, grown_rows));
        assert!(requests(&rig.fx, grown_bytes, grown_rows));
    }

    rig.fx.clear();
    rig.ok(g.page(
        b"grown-cursor",
        Some(b"grown-cursor"),
        b"history-page",
        grown_rows,
    ));
    assert!(requests(&rig.fx, 1024 * 1024, 1024));
    rig.ok(KernelInput::HistoryPage {
        terminal_id: &g.id,
        stream_id: g.s,
        bootstrap_id: g.b,
        cursor: b"grown-cursor",
        next_cursor: None,
        payload: b"history-finish",
        page_seq: 2,
        rows: 0,
    });
    assert_eq!(rig.history_state(&g.id), HistoryLoadState::Complete);
}

#[test]
fn history_engine_failure_invalidates_only_history_and_live_output_continues() {
    let g = gen_of(31, 131, 231);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.publish_with_history(&g, 73, b"cursor-0");
    rig.fx.clear();

    let failed = rig.up(g.page(b"cursor-0", Some(b"cursor-1"), b"history-error", 0));
    assert!(matches!(
        failed,
        Err(KernelError::Engine(FakeError::MutatedThenFailed))
    ));
    assert!(rig.tomb(&g).is_none());
    assert!(
        !rig.transcript(&g.id).ends_with(b"history-error"),
        "history import errors are transactional"
    );
    assert!(matches!(
        rig.k.input_eligibility(&g.id),
        InputEligibility::Eligible {
            stream_id: actual_stream,
            bootstrap_id: actual_bootstrap,
        } if actual_stream == g.s && actual_bootstrap == g.b
    ));
    assert!(any_effect!(
        rig.fx,
        KernelEffect::Status(KernelStatus::History { status, .. })
            if status.state == HistoryLoadState::Tombstoned
    ));
    assert!(!any_effect!(
        rig.fx,
        KernelEffect::Status(KernelStatus::ResyncRequired { .. })
            | KernelEffect::Damage(KernelDamage {
                kind: KernelDamageKind::Removed,
                ..
            })
    ));

    rig.fx.clear();
    rig.ok(g.output(74, b"live-after-history-error"));
    assert!(rig.transcript(&g.id).ends_with(b"live-after-history-error"));
}

#[test]
fn oversized_history_rejection_ends_history_without_retiring_live_generation() {
    let g = gen_of(32, 132, 232);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.publish_with_history(&g, 80, b"cursor-0");

    rig.fx.clear();
    rig.ok(g.rejected(b"cursor-0", HistoryRejectionReason::TooSmall, u32::MAX, 1));
    assert!(any_effect!(
        rig.fx,
        KernelEffect::Status(KernelStatus::History { status, .. })
            if status.state == HistoryLoadState::Tombstoned && status.next_cursor.is_none()
    ));
    assert!(any_effect!(
        rig.fx,
        KernelEffect::Status(KernelStatus::HistoryUnavailable {
            reason: HistoryUnavailableReason::Limit,
            ..
        })
    ));
    assert!(!any_effect!(
        rig.fx,
        KernelEffect::Send(KernelSend::HistoryRequest { .. })
    ));

    rig.ok(g.output(81, b"live-after-limit"));
    assert_eq!(rig.k.published(&g.id).unwrap().last_seq(), 81);
}

#[test]
fn replacement_is_atomic_and_old_view_remains_live_until_swap() {
    let old = gen_of(4, 14, 24);
    let new = old.with_bootstrap(25);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.publish(&old, 0);
    let old_view = rig.transcript(&old.id).to_vec();

    rig.begin(&new, 10);
    rig.ok(new.chunk(0, b"replacement-prefix"));
    assert_eq!(rig.transcript(&old.id), old_view);

    rig.ok(old.output(1, b"old-still-live"));
    rig.ok(new.chunk(1, READY_MARKER));
    rig.ok(new.ready(None));
    let published = rig.k.published(&old.id).unwrap();
    assert_eq!(published.key().bootstrap_id, new.b);
    assert_eq!(published.last_seq(), 10);
    assert!(rig.transcript(&old.id).starts_with(b"replacement-prefix"));
    assert!(rig.tomb(&old).is_some());

    assert!(matches!(
        rig.up(old.output(2, b"stale-old")),
        Err(KernelError::RetiredGeneration { .. })
    ));
}

#[test]
fn two_pane_attach_barrier_accepts_one_ready_and_one_close() {
    let ready = gen_of(5, 15, 26);
    let closed_terminal = terminal(6);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.ok(attach_started(
        8,
        &[ready.id.clone(), closed_terminal.clone()],
    ));
    rig.publish(&ready, 4);
    assert!(rig.fx.is_empty());
    rig.ok(closed(&closed_terminal));
    assert_eq!(rig.fx.as_slice(), &[exited(&closed_terminal)]);

    rig.ok(attach_ready(8));
    assert_eq!(
        rig.fx.as_slice(),
        &[subscribe_all(), damage(&ready.id, KernelDamageKind::Full)]
    );
    assert!(matches!(
        rig.k.input_eligibility(&ready.id),
        InputEligibility::Eligible { .. }
    ));
    assert_eq!(
        rig.k.input_eligibility(&closed_terminal),
        InputEligibility::Ineligible(InputBlockReason::Closed)
    );
}

#[test]
fn host_boundary_rejects_profile_and_pre_ready_data_with_typed_errors() {
    let g = gen_of(70, 71, 72);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    assert_eq!(rig.k.selected_profile(), BootstrapProfile::SynthesizedVtRaw);
    rig.ok(attach_started(73, std::slice::from_ref(&g.id)));

    let wrong_profile = rig.up(g.begin_as(BootstrapStreamProfile::SynthesizedVtStateSync, 100));
    assert!(matches!(
        wrong_profile,
        Err(KernelError::ProfileMismatch {
            selected: BootstrapProfile::SynthesizedVtRaw,
            incoming: BootstrapStreamProfile::SynthesizedVtStateSync,
        })
    ));
    assert!(rig.k.staging(&g.id).is_none());

    rig.begin(&g, 100);
    let staging_before = rig.k.staging(&g.id).unwrap();
    let key_before = staging_before.key().clone();
    let geometry_before = staging_before.geometry();
    let engine_ready_before = staging_before.engine_ready();
    let protocol_ready_before = staging_before.protocol_ready();
    assert!(staging_before.engine().transcript.is_empty());
    assert!(rig.fx.is_empty());
    assert!(matches!(
        rig.up(g.output(101, b"\xfflive-before-ready")),
        Err(KernelError::GenerationMismatch { .. })
    ));
    assert!(rig.fx.is_empty());
    let history_before_ready = rig.up(g.page(
        b"cursor-0",
        Some(b"cursor-1"),
        b"\xfefuture-history-before-ready",
        0,
    ));
    assert!(matches!(
        history_before_ready,
        Err(KernelError::GenerationMismatch { .. })
    ));
    assert!(rig.fx.is_empty());
    let staging_after = rig.k.staging(&g.id).unwrap();
    assert_eq!(staging_after.key(), &key_before);
    assert_eq!(staging_after.geometry(), geometry_before);
    assert_eq!(staging_after.engine_ready(), engine_ready_before);
    assert_eq!(staging_after.protocol_ready(), protocol_ready_before);
    assert!(staging_after.engine().transcript.is_empty());
    assert!(rig.k.published(&g.id).is_none());
}

fn published_recording_native(g: &Gen) -> Rig<RecordingNativeAdapter> {
    let mut rig = Rig::of(SessionKernel::new(RecordingNativeAdapter, NATIVE));
    rig.ok(g.begin_as(NATIVE_STREAM, 10));
    rig.ok(g.chunk(0, b"checkpoint-a"));
    rig.ok(g.chunk(1, b"checkpoint-b"));
    rig.ok(g.ready(Some(b"cursor-0")));
    rig
}

#[test]
fn selected_native_host_preserves_opaque_bytes_and_lifecycle_order() {
    let advertised = BootstrapCapabilities::new().with_native(
        EngineCodec::LibghosttySnapshotV1,
        EngineFeatureSet::required_native(),
    );
    let client = ClientCapabilities::new().with_bootstrap(advertised);
    let (selected, _) = select_bootstrap_profile(&client, &advertised).unwrap();
    assert_eq!(selected, NATIVE);
    let g = gen_of(80, 81, 82);
    let mut rig = Rig::of(SessionKernel::new(RecordingNativeAdapter, selected));
    rig.ok(g.begin_as(NATIVE_STREAM, 900));
    assert!(rig.fx.is_empty());

    let checkpoint_a: &[u8] = b"\xff\0checkpoint-a\x80";
    let checkpoint_b: &[u8] = b"\xfecheckpoint-b\0\xfd";
    for (chunk_seq, payload) in [(0, checkpoint_a), (1, checkpoint_b)] {
        rig.ok(g.chunk(chunk_seq, payload));
        assert!(rig.fx.is_empty());
    }
    rig.ok(g.ready(Some(b"cursor-0")));
    let published_key = rig.k.published(&g.id).unwrap().key().clone();
    assert_eq!(published_key.terminal_id, g.id);
    assert_eq!(published_key.stream_id, g.s);
    assert_eq!(published_key.bootstrap_id, g.b);
    assert!(requests_history(&rig.fx, b"cursor-0"));
    assert!(any_effect!(
        rig.fx,
        KernelEffect::Damage(KernelDamage {
            terminal_id: damaged,
            kind: KernelDamageKind::Full,
        }) if damaged == &g.id
    ));

    let live_a: &[u8] = b"\x80live-a\xff";
    let history: &[u8] = b"\0\xfehistory-future\x81";
    let live_b: &[u8] = b"\xfdlive-b\0";
    rig.ok(g.output(901, live_a));
    assert!(rig.fx.is_empty());
    rig.ok(g.page(b"cursor-0", Some(b"cursor-1"), history, 0));
    assert!(any_effect!(
        rig.fx,
        KernelEffect::Status(KernelStatus::History { status, .. })
            if status.state == HistoryLoadState::Loading
    ));
    rig.ok(g.output(902, live_b));
    assert!(rig.fx.is_empty());

    assert_eq!(
        rig.k.published(&g.id).unwrap().engine().records,
        [
            NativeRecord::Bootstrap(checkpoint_a.to_vec()),
            NativeRecord::Bootstrap(checkpoint_b.to_vec()),
            NativeRecord::Ready,
            NativeRecord::Live(live_a.to_vec()),
            NativeRecord::History(history.to_vec()),
            NativeRecord::Live(live_b.to_vec()),
        ]
    );
}

#[test]
fn native_history_requires_finish_exactly_on_the_final_cursor_page() {
    for (payload, has_more, expected_progress) in [
        (
            b"history-without-finish".as_slice(),
            false,
            BootstrapProgress::Ready,
        ),
        (b"finish".as_slice(), true, BootstrapProgress::Finished),
    ] {
        let g = gen_of(83, 84, 85);
        let mut rig = published_recording_native(&g);
        rig.fx.clear();
        let next = has_more.then_some(b"cursor-1".as_slice());
        let result = rig.up(g.page(b"cursor-0", next, payload, 0));
        assert!(matches!(
            result,
            Err(KernelError::HistoryCompletionMismatch {
                progress,
                has_more: actual_has_more,
            }) if progress == expected_progress && actual_has_more == has_more
        ));
        assert!(
            rig.k.published(&g.id).is_some(),
            "history mismatch retains the last renderable replica"
        );
        assert!(rig.tomb(&g).is_none());
        assert!(matches!(
            rig.k.input_eligibility(&g.id),
            InputEligibility::Eligible { .. }
        ));
        assert!(any_effect!(
            rig.fx,
            KernelEffect::Status(KernelStatus::History { status, .. })
                if status.state == HistoryLoadState::Tombstoned
        ));
        assert!(!any_effect!(
            rig.fx,
            KernelEffect::Status(KernelStatus::ResyncRequired { .. })
        ));
    }
}

#[test]
fn engine_effects_are_drained_after_apply_in_order_into_a_reused_buffer() {
    let g = gen_of(8, 17, 28);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.fx = EffectBuffer::with_capacity(4);
    let initial_capacity = rig.fx.capacity();
    rig.publish(&g, 0);
    let published_key = rig.k.published(&g.id).unwrap().key().clone();

    rig.ok(g.output(1, b"effects"));
    assert_eq!(
        rig.fx.as_slice(),
        &[
            damage(&g.id, KernelDamageKind::Rows { first: 2, last: 4 }),
            KernelEffect::Status(KernelStatus::Engine {
                key: published_key.clone(),
                status: EngineStatus::Bell,
            }),
            KernelEffect::Job(KernelJob {
                key: published_key,
                job: EngineJob::Wakeup,
            }),
        ]
    );
    assert_eq!(rig.fx.capacity(), initial_capacity);

    rig.ok(g.output(2, b"plain"));
    assert_eq!(rig.fx.len(), 1, "prior engine effects were fully drained");
    assert_eq!(rig.fx.capacity(), initial_capacity);
    rig.fx.clear();
    assert_eq!(rig.fx.capacity(), initial_capacity);
}

#[test]
fn input_gate_requires_published_replica_and_attach_ready() {
    let g = gen_of(9, 18, 29);
    let event = InputEvent::Focus(FocusEvent::Gained);
    let input = || {
        KernelInput::Action(KernelAction::Input {
            terminal_id: &g.id,
            event: &event,
        })
    };
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.ok(attach_started(9, std::slice::from_ref(&g.id)));

    for (reason, publish) in [
        (InputBlockReason::AwaitingReplica, true),
        (InputBlockReason::AwaitingAttachReady, false),
    ] {
        assert_eq!(
            rig.k.input_eligibility(&g.id),
            InputEligibility::Ineligible(reason)
        );
        assert!(matches!(
            rig.up(input()),
            Err(KernelError::InputIneligible { reason: actual, .. }) if actual == reason
        ));
        if publish {
            rig.publish(&g, 0);
        }
    }

    rig.ok(attach_ready(9));
    assert_eq!(
        rig.k.input_eligibility(&g.id),
        InputEligibility::Eligible {
            stream_id: g.s,
            bootstrap_id: g.b,
        }
    );
    rig.ok(input());
    assert_eq!(
        rig.fx.as_slice(),
        &[KernelEffect::Send(KernelSend::Input {
            terminal_id: g.id.clone(),
            event,
        })]
    );
}

#[test]
fn state_sync_ack_is_generation_bound_and_raw_has_no_ack() {
    let g = gen_of(11, 20, 31);
    let mut rig = Rig::of(kernel_with_profile(
        ReadyMode::ChunkFirst,
        BootstrapProfile::SynthesizedVtStateSync,
    ));
    rig.ok(g.begin_as(BootstrapStreamProfile::SynthesizedVtStateSync, 0));
    rig.push_ready_transcript(&g);
    rig.ok(g.ready(None));

    rig.ok(g.output(1, b"state-sync"));
    assert_eq!(
        rig.fx.as_slice(),
        &[
            damage(&g.id, KernelDamageKind::Full),
            KernelEffect::Send(KernelSend::FrameAck {
                terminal_id: g.id.clone(),
                stream_id: g.s,
                bootstrap_id: g.b,
                seq: 1,
            }),
        ]
    );

    let raw = gen_of(12, 21, 32);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.publish(&raw, 0);
    rig.ok(raw.output(1, b"raw"));
    assert!(!any_effect!(
        rig.fx,
        KernelEffect::Send(KernelSend::FrameAck { .. })
    ));
}

#[test]
fn mutating_adapter_errors_retire_staging_and_published_replicas() {
    let mut rig = Rig::new(ReadyMode::ProtocolFirst);
    let resync = |g: &Gen| {
        KernelEffect::Status(KernelStatus::ResyncRequired {
            terminal_id: g.id.clone(),
            stream_id: g.s,
            bootstrap_id: g.b,
            reason: TombstoneReason::CodecFailure,
        })
    };

    let chunked = gen_of(13, 22, 33);
    rig.begin(&chunked, 0);
    assert!(matches!(
        rig.up(chunked.chunk(0, b"mutate-then-error")),
        Err(KernelError::Engine(FakeError::MutatedThenFailed))
    ));
    assert!(rig.k.staging(&chunked.id).is_none());
    assert!(rig.tomb(&chunked).is_some());
    assert_eq!(rig.fx.as_slice(), &[resync(&chunked)]);
    assert!(matches!(
        rig.up(chunked.chunk(0, b"retry")),
        Err(KernelError::RetiredGeneration { .. })
    ));

    let finish = gen_of(14, 23, 34);
    rig.begin(&finish, 0);
    rig.ok(finish.chunk(0, b"<FINISH_ERROR>"));
    assert!(matches!(
        rig.up(finish.ready(None)),
        Err(KernelError::Engine(FakeError::MutatedThenFailed))
    ));
    assert!(rig.k.staging(&finish.id).is_none());
    assert!(rig.tomb(&finish).is_some());
    assert!(matches!(
        rig.up(finish.ready(None)),
        Err(KernelError::RetiredGeneration { .. })
    ));

    let live = gen_of(15, 24, 35);
    rig.publish(&live, 0);
    assert!(matches!(
        rig.up(live.output(1, b"mutate-then-error")),
        Err(KernelError::Engine(FakeError::MutatedThenFailed))
    ));
    assert!(rig.k.published(&live.id).is_none());
    assert!(rig.tomb(&live).is_some());
    assert_eq!(
        rig.fx.as_slice(),
        &[resync(&live), damage(&live.id, KernelDamageKind::Removed)]
    );
    assert!(matches!(
        rig.up(live.output(1, b"retry")),
        Err(KernelError::RetiredGeneration { .. })
    ));
}

/// The effects a `bootstrap-effects` chunk and its finish leave for publication.
fn bootstrap_effects(key: &super::ReplicaKey) -> Vec<KernelEffect> {
    let title = |title: &str| {
        KernelEffect::Status(KernelStatus::Engine {
            key: key.clone(),
            status: EngineStatus::Title(title.to_owned()),
        })
    };
    let wakeup = || {
        KernelEffect::Job(KernelJob {
            key: key.clone(),
            job: EngineJob::Wakeup,
        })
    };
    vec![
        title("bootstrap-staging"),
        wakeup(),
        title("bootstrap-finished"),
        wakeup(),
        damage(&key.terminal_id, KernelDamageKind::Full),
    ]
}

#[test]
fn bootstrap_effects_wait_for_publication_and_suppress_send_and_damage() {
    let g = gen_of(16, 25, 36);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.begin(&g, 0);
    rig.ok(g.chunk(0, b"bootstrap-effects"));
    assert!(rig.fx.is_empty());
    rig.ok(g.chunk(1, READY_MARKER));
    assert!(rig.fx.is_empty());
    rig.ok(g.ready(None));
    let published_key = rig.k.published(&g.id).unwrap().key().clone();
    assert_eq!(rig.fx.as_slice(), bootstrap_effects(&published_key));
}

#[test]
fn replacement_attach_close_flushes_pending_removal_at_barrier() {
    let g = gen_of(17, 26, 37);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.ok(attach_started(40, std::slice::from_ref(&g.id)));
    rig.publish(&g, 0);
    rig.ok(attach_ready(40));

    rig.ok(attach_started(41, std::slice::from_ref(&g.id)));
    assert!(rig.k.published(&g.id).is_some());
    rig.ok(closed(&g.id));
    assert_eq!(rig.fx.as_slice(), &[exited(&g.id)]);
    rig.ok(attach_ready(41));
    assert_eq!(
        rig.fx.as_slice(),
        &[subscribe_all(), damage(&g.id, KernelDamageKind::Removed)]
    );
}

#[test]
fn replacement_effects_are_hidden_until_swap_and_discarded_on_retirement() {
    let published = gen_of(18, 27, 38);
    let cancelled = published.with_bootstrap(39);
    let replacement = published.with_bootstrap(40);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.publish(&published, 0);
    let published_key = rig.k.published(&published.id).unwrap().key().clone();

    rig.begin(&cancelled, 10);
    rig.ok(cancelled.chunk(0, b"bootstrap-effects"));
    assert!(rig.fx.is_empty());
    assert_eq!(
        rig.k.published(&published.id).unwrap().key(),
        &published_key
    );

    rig.ok(cancelled.tombstone(TombstoneReason::CodecFailure, 10));
    assert!(rig.fx.is_empty(), "retired staging effects are discarded");
    assert_eq!(
        rig.k.published(&published.id).unwrap().key(),
        &published_key
    );

    rig.begin(&replacement, 20);
    rig.ok(replacement.chunk(0, b"bootstrap-effects"));
    assert!(rig.fx.is_empty());
    rig.ok(replacement.chunk(1, READY_MARKER));
    assert!(rig.fx.is_empty());
    rig.ok(replacement.ready(None));
    let replacement_key = rig.k.published(&published.id).unwrap().key().clone();
    assert_eq!(replacement_key.bootstrap_id, replacement.b);
    assert_eq!(rig.fx.as_slice(), bootstrap_effects(&replacement_key));
}

/// phux-994s: the walk-identity token a frontend hands its render pool packs
/// the kernel's own generation identity `(stream_id, bootstrap_id)`
/// losslessly, so distinct generations always yield distinct tokens and a
/// republish on the same stream changes the token.
#[test]
fn generation_token_is_lossless_and_changes_per_generation() {
    let key = |stream: u64, bootstrap: u64| super::ReplicaKey {
        terminal_id: ResourceId::local(1),
        stream_id: StreamId::new(stream).expect("stream"),
        bootstrap_id: BootstrapId::new(bootstrap).expect("bootstrap"),
        profile: BootstrapStreamProfile::SynthesizedVtRaw,
    };

    assert_eq!(key(1, 2).generation_token(), (1_u128 << 64) | 2);
    // A republish (new bootstrap, same stream) changes the token.
    assert_ne!(key(1, 1).generation_token(), key(1, 2).generation_token());
    // The pair packs losslessly: swapped halves cannot collide.
    assert_ne!(key(1, 2).generation_token(), key(2, 1).generation_token());
    assert_ne!(
        key(1, u64::MAX).generation_token(),
        key(2, 1).generation_token()
    );
}

/// phux-l96p.10: a gap resync's replacement generation is adopted whole: its
/// `base_seq` (far past the dropped window) is the new origin and `base + 1`
/// the next live frame, never a `SequenceGap`. The native path tombstones
/// the old generation first, the synthesized path does not; in both, output
/// for the old generation is `RetiredGeneration`, which the host ignores.
#[test]
fn gap_resync_replacement_generation_resets_the_live_sequence() {
    for tombstone_first in [true, false] {
        let old = gen_of(7, 17, 27);
        let replacement = old.with_bootstrap(28);
        let mut rig = Rig::new(ReadyMode::ChunkFirst);

        rig.publish(&old, 41);
        rig.ok(old.output(42, b"before-the-gap"));
        if tombstone_first {
            rig.ok(old.tombstone(TombstoneReason::OutboundGap, 42));
        }

        let resync_base = 9_000;
        rig.publish(&replacement, resync_base);
        let published = rig.k.published(&old.id).unwrap();
        assert_eq!(
            published.key().bootstrap_id,
            replacement.b,
            "{tombstone_first}"
        );
        assert_eq!(published.last_seq(), resync_base, "{tombstone_first}");

        rig.up(replacement.output(resync_base + 1, b"after-the-gap"))
            .unwrap_or_else(|err| panic!("{tombstone_first}: first frame after resync: {err}"));
        assert_eq!(
            rig.k.published(&old.id).unwrap().last_seq(),
            resync_base + 1
        );

        let stale = rig.up(old.output(43, b"stale"));
        assert!(
            matches!(stale, Err(KernelError::RetiredGeneration { .. })),
            "{tombstone_first}: stale generation output must be ignored, got {stale:?}",
        );
    }
}

const AGENT_PROFILE: BootstrapStreamProfile = BootstrapStreamProfile::AgentEventsJsonlV1;

fn agent_line(seq: u64, kind: &str, data: &str) -> String {
    format!(
        "{{\"seq\":{seq},\"ts_ms\":{},\"type\":\"{kind}\",\"data\":{data}}}\n",
        seq * 10
    )
}

/// A generation of the agent these tests use, `AgentSession` 2.
fn agent_gen(s: u64, b: u64) -> Gen {
    gen_of(2, s, b)
}

impl Gen {
    const fn agent_begin(&self, base_seq: u64) -> KernelInput<'_> {
        self.begin_gridless(AGENT_PROFILE, base_seq)
    }

    const fn resume(&self, after_seq: u64, base_seq: u64) -> KernelInput<'_> {
        self.begin_gridless(
            BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq },
            base_seq,
        )
    }
}

type Publication = (ResourceId, Vec<u64>, AgentRecordMode, AgentStreamInfo);

const fn committed_agent(
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    last_seq: u64,
) -> AgentStreamInfo {
    AgentStreamInfo {
        stream_id,
        bootstrap_id,
        last_seq,
    }
}

/// The publication `g` should commit: `seqs` under `mode`, cursor at `last_seq`.
#[allow(clippy::unnecessary_wraps, reason = "compared with `publication()`")]
fn published(g: &Gen, seqs: &[u64], mode: AgentRecordMode, last_seq: u64) -> Option<Publication> {
    Some((
        g.id.clone(),
        seqs.to_vec(),
        mode,
        committed_agent(g.s, g.b, last_seq),
    ))
}

fn agent_publication(effects: &EffectBuffer) -> Option<Publication> {
    effects.as_slice().iter().find_map(|effect| {
        let KernelEffect::AgentPublication(publication) = effect else {
            return None;
        };
        let seqs = publication.records.iter().map(|record| record.seq);
        Some((
            publication.resource_id.clone(),
            seqs.collect(),
            publication.mode,
            publication.stream,
        ))
    })
}

fn batch_payload(records: &[(u64, &str)]) -> String {
    use std::fmt::Write;
    let mut payload = String::new();
    for (seq, kind) in records {
        writeln!(
            payload,
            "{{\"seq\":{seq},\"ts_ms\":1,\"type\":\"{kind}\",\"data\":{{}}}}"
        )
        .unwrap();
    }
    payload
}

impl Rig {
    fn declare_agent(&mut self, agent: &ResourceId, parent: &ResourceId) {
        self.ok(KernelInput::AgentSessionDeclared(
            super::AgentSessionDeclaration {
                terminal_id: agent,
                parent: Some(parent),
                provider: Some("claude"),
                native_id: Some("s-1"),
                state: Some("working"),
            },
        ));
    }

    fn publication(&self) -> Option<Publication> {
        agent_publication(&self.fx)
    }

    /// One live frame on agent generation (3, 4) carrying `records`.
    fn batch(&mut self, seq: u64, records: &[(u64, &str)]) -> Result<(), KernelError<FakeError>> {
        self.up(agent_gen(3, 4).output(seq, batch_payload(records).as_bytes()))
    }

    fn seqs(&self) -> Vec<u64> {
        let log = &self.k.agent_session(&terminal(2)).unwrap().log;
        log.records().map(|record| record.seq).collect()
    }

    fn status(&self) -> AgentSessionStatus {
        self.k.agent_session(&terminal(2)).unwrap().state.status
    }

    fn cursor(&self) -> Option<AgentStreamInfo> {
        self.k.agent_stream_info(&terminal(2))
    }
}

/// Agent 2 (parent 1) declared and published empty on generation (3, 4).
fn agent_rig(base_seq: u64) -> Rig {
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    let g = agent_gen(3, 4);
    rig.declare_agent(&g.id, &terminal(1));
    rig.ok(g.agent_begin(base_seq));
    rig.ok(g.ready(None));
    rig.fx.clear();
    rig
}

fn assert_agent_fragment_publication(fragments: &[&[u8]]) {
    let g = agent_gen(3, 4);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.ok(g.agent_begin(2));
    for (index, payload) in (0_u32..).zip(fragments) {
        rig.ok(g.chunk(index, payload));
        assert!(rig.fx.is_empty());
        let view = rig.k.agent_session(&g.id).unwrap();
        assert!(!view.published);
        assert!(view.log.is_empty());
    }
    rig.ok(g.ready(None));
    assert_eq!(
        rig.publication(),
        published(&g, &[1, 2], AgentRecordMode::Replace, 2)
    );
    let view = rig.k.agent_session(&g.id).unwrap();
    assert_eq!(view.state.status, AgentSessionStatus::Blocked);
    assert_eq!(
        view.log.last().unwrap().data_str("question"),
        Some("déployer 🦀?")
    );
}

#[test]
fn agent_bootstrap_records_accept_every_byte_split_and_empty_chunks() {
    let payload = format!(
        "{}{}",
        agent_line(1, "prompt", "{}"),
        agent_line(2, "ask", r#"{"question":"déployer 🦀?"}"#)
    );
    let bytes = payload.as_bytes();
    for split in 0..=bytes.len() {
        assert_agent_fragment_publication(&[&bytes[..split], b"", &bytes[split..]]);
    }
    for width in 1..=bytes.len() {
        assert_agent_fragment_publication(&bytes.chunks(width).collect::<Vec<_>>());
    }
}

#[test]
fn agent_bootstrap_accepts_exact_record_limit_including_split_lf() {
    let g = agent_gen(3, 4);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    let mut line = agent_line(1, "stop", "{}").into_bytes();
    line.pop();
    line.resize(super::agent_stream::MAX_RECORD_BYTES - 1, b' ');
    rig.ok(g.agent_begin(1));
    rig.ok(g.chunk(0, &line));
    assert_eq!(
        rig.k.lanes.agent(&g.id).unwrap().staged_record_count(),
        Some(0),
        "no decoding before LF"
    );
    rig.ok(g.chunk(1, b"\n"));
    rig.ok(g.ready(None));
    assert_eq!(
        rig.publication(),
        published(&g, &[1], AgentRecordMode::Replace, 1)
    );
}

/// Agent 2 published with record 1 ("stop"), staging generation (3, 5).
fn replacing_agent_rig(base_seq: u64) -> (Rig, Gen) {
    let mut rig = agent_rig(0);
    rig.batch(1, &[(1, "stop")]).unwrap();
    let g = agent_gen(3, 5);
    rig.ok(g.agent_begin(base_seq));
    (rig, g)
}

fn assert_agent_replacement_failure(rig: &Rig) {
    let view = rig.k.agent_session(&terminal(2)).unwrap();
    assert!(view.published);
    assert_eq!(view.state.status, AgentSessionStatus::Done);
    assert_eq!(view.log.len(), 1);
    assert_eq!(view.log.last().unwrap().seq, 1);
    assert!(rig.publication().is_none());
    assert!(
        any_effect!(rig.fx, KernelEffect::Status(KernelStatus::ResyncRequired {
            bootstrap_id, reason: TombstoneReason::CodecFailure, ..
        }) if *bootstrap_id == bootstrap(5))
    );
    let lane = rig.k.lanes.agent(&terminal(2)).unwrap();
    assert!(lane.staged_record_count().is_none());
}

fn padded_agent_wire_record(seq: u64, wire_bytes: usize) -> Vec<u8> {
    let mut line = agent_line(seq, "stop", "{}").into_bytes();
    line.pop();
    line.resize(wire_bytes - 1, b' ');
    line.push(b'\n');
    line
}

#[test]
fn agent_live_record_wire_limit_includes_lf_and_whitespace() {
    use super::agent_stream::{AgentRecordError, MAX_RECORD_BYTES};
    let g = agent_gen(3, 4);
    let mut rig = agent_rig(0);
    rig.ok(g.output(1, &padded_agent_wire_record(1, MAX_RECORD_BYTES)));
    assert_eq!(
        rig.publication(),
        published(&g, &[1], AgentRecordMode::Append, 1)
    );
    let oversize = padded_agent_wire_record(2, MAX_RECORD_BYTES + 1);
    assert!(matches!(
        rig.up(g.output(2, &oversize)),
        Err(KernelError::AgentRecord(AgentRecordError::Oversize(16_385)))
    ));
    assert!(rig.publication().is_none());
    assert_eq!(rig.seqs(), [1]);
}

#[test]
fn agent_bootstrap_rejects_oversize_before_lf_and_preserves_published_state() {
    for suffix in [b" ".as_slice(), b" \n".as_slice()] {
        let (mut rig, g) = replacing_agent_rig(2);
        rig.ok(g.chunk(0, &vec![b' '; super::agent_stream::MAX_RECORD_BYTES - 1]));
        assert!(matches!(
            rig.up(g.chunk(1, suffix)),
            Err(KernelError::AgentRecord(super::AgentRecordError::Oversize(
                16_385
            )))
        ));
        assert_agent_replacement_failure(&rig);
        assert!(matches!(
            rig.up(g.ready(None)),
            Err(KernelError::RetiredGeneration { .. })
        ));
    }
}

#[test]
fn agent_bootstrap_rejects_bad_records_without_replacing_published_state() {
    let complete = agent_line(2, "ask", "{}");
    // A trailing line that is still incomplete or invalid at READY.
    for tail in [
        b"{".as_slice(),
        complete.trim_end().as_bytes(),
        b" ",
        &[0xf0, 0x9f],
    ] {
        let (mut rig, g) = replacing_agent_rig(2);
        rig.ok(g.chunk(0, complete.as_bytes()));
        rig.ok(g.chunk(1, tail));
        assert!(matches!(
            rig.up(g.ready(None)),
            Err(KernelError::AgentRecord(super::AgentRecordError::Json(_)))
        ));
        assert_agent_replacement_failure(&rig);
    }
    // A malformed complete line fails at its chunk.
    let (mut rig, g) = replacing_agent_rig(2);
    let bytes = format!("{complete}{{bad json}}\n");
    assert!(matches!(
        rig.up(g.chunk(0, bytes.as_bytes())),
        Err(KernelError::AgentRecord(_))
    ));
    assert_agent_replacement_failure(&rig);
}

#[test]
fn agent_bootstrap_replacement_and_release_reset_partial_decoder() {
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    let first = agent_gen(3, 4);
    rig.ok(first.agent_begin(0));
    rig.ok(first.chunk(0, b"{partial"));
    let second = agent_gen(3, 5);
    rig.ok(second.agent_begin(1));
    rig.ok(second.chunk(0, agent_line(1, "stop", "{}").as_bytes()));
    rig.ok(second.ready(None));
    assert_eq!(
        rig.publication(),
        published(&second, &[1], AgentRecordMode::Replace, 1)
    );
    let third = agent_gen(3, 6);
    rig.ok(third.agent_begin(1));
    rig.ok(third.chunk(0, b"{partial"));
    assert!(rig.k.release_terminal(&third.id));
    rig.ok(third.agent_begin(0));
    rig.ok(third.ready(None));
    assert_eq!(
        rig.publication(),
        published(&third, &[], AgentRecordMode::Replace, 0)
    );
}

#[test]
#[allow(
    clippy::cognitive_complexity,
    reason = "one stream lifecycle end to end: declare, bootstrap, live, close"
)]
fn agent_session_bootstraps_into_a_typed_log_and_never_a_replica() {
    let parent = gen_of(1, 10, 11);
    let g = agent_gen(3, 4);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.ok(attach_started(1, std::slice::from_ref(&parent.id)));
    rig.declare_agent(&g.id, &parent.id);
    assert_eq!(rig.k.resource_kind(&g.id), Some(ResourceKind::AgentSession));
    assert_eq!(
        rig.k.resource_kind(&parent.id),
        Some(ResourceKind::Terminal)
    );
    let view = rig.k.agent_session(&g.id).expect("declared");
    assert_eq!(view.parent, Some(&parent.id));
    assert_eq!(view.state.provider.as_deref(), Some("claude"));
    assert_eq!(view.state.status, AgentSessionStatus::Working);
    assert!(!view.published);
    assert_eq!(
        rig.k.input_eligibility(&g.id),
        InputEligibility::Ineligible(InputBlockReason::NotATerminal)
    );

    // A declared AgentSession is not an attach participant: the barrier
    // releases on the parent alone.
    rig.publish(&parent, 0);
    rig.ok(attach_ready(1));

    rig.ok(g.agent_begin(5));
    let records = format!(
        "{}{}",
        agent_line(
            4,
            "session_start",
            r#"{"provider":"claude","native_id":"s-9"}"#
        ),
        agent_line(5, "ask", r#"{"question":"deploy?"}"#)
    );
    rig.ok(g.chunk(0, records.as_bytes()));
    assert!(rig.fx.is_empty(), "staging emits nothing until READY");
    rig.ok(g.ready(None));
    assert_eq!(
        rig.publication(),
        published(&g, &[4, 5], AgentRecordMode::Replace, 5),
        "publication hands the frontend every retained record"
    );
    assert!(
        !any_effect!(rig.fx, KernelEffect::Damage(_)),
        "an agent stream never damages a grid"
    );
    assert!(
        rig.k.published(&g.id).is_none(),
        "no replica for a record stream"
    );
    let view = rig.k.agent_session(&g.id).expect("published");
    assert!(view.published);
    assert_eq!(
        view.state.native_id.as_deref(),
        Some("s-9"),
        "records refine identity"
    );
    assert_eq!(view.state.status, AgentSessionStatus::Blocked);
    assert_eq!(view.log.len(), 2);

    // Live output appends and re-derives.
    rig.ok(g.output(6, agent_line(6, "stop", "{}").as_bytes()));
    assert_eq!(
        rig.publication(),
        published(&g, &[6], AgentRecordMode::Append, 6)
    );
    assert_eq!(rig.status(), AgentSessionStatus::Done);
    assert_eq!(rig.seqs().last(), Some(&6));

    // Sequencing stays exact.
    assert!(matches!(
        rig.up(g.output(8, agent_line(8, "stop", "{}").as_bytes())),
        Err(KernelError::SequenceGap {
            expected: 7,
            actual: 8
        })
    ));

    // Closing the session drops the view; its kind is still known.
    rig.ok(closed(&g.id));
    assert!(
        rig.fx.is_empty(),
        "closing a record stream removes no grid, and an AgentSession's exit \
         status is not a Terminal process exit"
    );
    assert!(rig.k.agent_session(&g.id).is_none());
    assert_eq!(rig.k.resource_kind(&g.id), Some(ResourceKind::AgentSession));
    assert!(
        rig.k.published(&parent.id).is_some(),
        "the parent is untouched"
    );
}

#[test]
fn agent_profile_skips_the_geometry_check_but_terminals_still_need_one() {
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    // No declaration at all: the profile alone fixes the kind.
    rig.ok(agent_gen(1, 1).agent_begin(0));
    assert_eq!(
        rig.k.resource_kind(&terminal(2)),
        Some(ResourceKind::AgentSession)
    );
    let zero = rig.up(begin_input(
        &terminal(3),
        stream(2),
        bootstrap(2),
        BootstrapStreamProfile::SynthesizedVtRaw,
        CanonicalGeometry { cols: 0, rows: 0 },
        0,
    ));
    assert!(matches!(
        zero,
        Err(KernelError::InvalidGeometry { cols: 0, rows: 0 })
    ));
}

#[test]
fn kind_mismatches_are_rejected_in_both_directions() {
    let parent = gen_of(1, 2, 2);
    let agent = agent_gen(1, 1);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.ok(attach_started(1, std::slice::from_ref(&parent.id)));
    rig.declare_agent(&agent.id, &parent.id);

    // A terminal-profile bootstrap on a declared AgentSession.
    assert!(matches!(
        rig.up(agent.begin(0)),
        Err(KernelError::KindMismatch {
            declared: ResourceKind::AgentSession,
            incoming: ResourceKind::Terminal,
            ..
        })
    ));
    // An agent-profile bootstrap on an attach-inventory Terminal.
    assert!(matches!(
        rig.up(parent.agent_begin(0)),
        Err(KernelError::KindMismatch {
            declared: ResourceKind::Terminal,
            incoming: ResourceKind::AgentSession,
            ..
        })
    ));
    // Declaring an attach-inventory Terminal as an AgentSession.
    let wrong = rig.up(KernelInput::AgentSessionDeclared(
        super::AgentSessionDeclaration {
            terminal_id: &parent.id,
            parent: None,
            provider: None,
            native_id: None,
            state: None,
        },
    ));
    assert!(matches!(wrong, Err(KernelError::KindMismatch { .. })));
}

#[test]
fn malformed_agent_records_retire_only_that_generation() {
    let g = agent_gen(3, 4);
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.ok(g.agent_begin(0));
    let bad = rig.up(g.chunk(0, b"{\"seq\":1,\"ts_ms\":1}\n"));
    assert!(matches!(bad, Err(KernelError::AgentRecord(_))));
    assert!(any_effect!(
        rig.fx,
        KernelEffect::Status(KernelStatus::ResyncRequired {
            reason: TombstoneReason::CodecFailure,
            ..
        })
    ));
    assert_eq!(
        rig.tomb(&g),
        None,
        "agent generations retire inside the stream, not the terminal table"
    );
    assert!(matches!(
        rig.up(g.ready(None)),
        Err(KernelError::RetiredGeneration { .. })
    ));
    // A replacement generation is accepted and publishes cleanly.
    let next = g.with_bootstrap(5);
    rig.ok(next.agent_begin(0));
    rig.ok(next.ready(None));
    assert_eq!(
        rig.publication(),
        published(&next, &[], AgentRecordMode::Replace, 0)
    );
    assert!(
        rig.k
            .agent_session(&g.id)
            .is_some_and(|view| view.published)
    );
}

#[test]
fn a_republished_agent_stream_rebuilds_state_from_its_retained_records() {
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    rig.declare_agent(&terminal(2), &terminal(1));
    for (generation, kind) in [(1, "ask"), (2, "stop")] {
        let g = agent_gen(3, generation);
        rig.ok(g.agent_begin(1));
        rig.ok(g.chunk(0, agent_line(1, kind, "{}").as_bytes()));
        rig.ok(g.ready(None));
    }
    let view = rig.k.agent_session(&terminal(2)).expect("published");
    assert_eq!(view.state.status, AgentSessionStatus::Done);
    assert_eq!(
        view.log.len(),
        1,
        "the log is the retained stream, not a concatenation"
    );
    assert_eq!(
        view.state.provider.as_deref(),
        Some("claude"),
        "the declaration seeds every generation"
    );
    let replaced = agent_gen(3, 1);
    assert!(rig.tomb(&replaced).is_none());
    assert!(
        rig.up(replaced.output(1, b"")).is_err(),
        "the replaced generation is retired"
    );
}

#[test]
fn agent_live_batches_advance_by_the_last_record_sequence() {
    let g = agent_gen(3, 4);
    let mut rig = agent_rig(0);
    // The server stamps each record, then emits one frame with the last seq.
    rig.batch(3, &[(1, "prompt"), (2, "ask"), (3, "stop")])
        .unwrap();
    assert_eq!(
        rig.publication(),
        published(&g, &[1, 2, 3], AgentRecordMode::Append, 3)
    );
    assert_eq!(rig.status(), AgentSessionStatus::Done);
    assert_eq!(rig.seqs().len(), 3);
    rig.batch(5, &[(4, "prompt"), (5, "ask")]).unwrap();
    assert_eq!(
        rig.publication(),
        published(&g, &[4, 5], AgentRecordMode::Append, 5)
    );
    assert_eq!(rig.status(), AgentSessionStatus::Blocked);
}

#[test]
fn agent_live_batch_record_gaps_duplicates_and_envelope_mismatch_publish_nothing() {
    for (seq, records) in [
        (3, vec![(1, "ask"), (3, "stop")]),
        (2, vec![(1, "ask"), (1, "stop")]),
        (3, vec![(2, "ask"), (3, "stop")]),
        (1, vec![(1, "ask"), (2, "stop")]),
        (3, vec![(1, "ask"), (2, "stop")]),
        (1, vec![]),
    ] {
        let mut rig = agent_rig(0);
        assert!(
            rig.batch(seq, &records).is_err(),
            "invalid batch {records:?} with envelope {seq}"
        );
        assert!(rig.fx.is_empty(), "no partial publication on invalid batch");
        assert!(rig.seqs().is_empty());
        assert_eq!(rig.status(), AgentSessionStatus::Working);
        rig.batch(1, &[(1, "stop")]).unwrap();
        assert_eq!(rig.status(), AgentSessionStatus::Done);
    }
}

#[test]
fn agent_live_batches_preserve_duplicate_and_generation_fences() {
    let mut rig = agent_rig(0);
    let records = [(1, "prompt"), (2, "ask")];
    rig.batch(2, &records).unwrap();
    assert!(matches!(
        rig.batch(2, &records),
        Err(KernelError::DuplicateSequence { .. })
    ));
    assert!(rig.fx.is_empty());
    let next = batch_payload(&[(3, "prompt"), (4, "stop")]);
    assert!(matches!(
        rig.up(agent_gen(3, 99).output(4, next.as_bytes())),
        Err(KernelError::GenerationMismatch { .. })
    ));
    assert!(rig.fx.is_empty());
    assert_eq!(rig.status(), AgentSessionStatus::Blocked);
    rig.batch(4, &[(3, "prompt"), (4, "stop")]).unwrap();
}

#[test]
fn agent_live_batches_reach_sequence_exhaustion_without_wrapping() {
    let mut rig = agent_rig(u64::MAX - 2);
    rig.batch(u64::MAX, &[(u64::MAX - 1, "prompt"), (u64::MAX, "stop")])
        .unwrap();
    assert_eq!(
        rig.publication(),
        published(
            &agent_gen(3, 4),
            &[u64::MAX - 1, u64::MAX],
            AgentRecordMode::Append,
            u64::MAX
        )
    );
    assert!(matches!(
        rig.batch(0, &[(0, "ask")]),
        Err(KernelError::SequenceExhausted)
    ));
    assert!(rig.fx.is_empty());
    assert_eq!(rig.status(), AgentSessionStatus::Done);
}

#[test]
fn malformed_later_agent_batch_record_retires_without_partial_publication() {
    let mut rig = agent_rig(0);
    let payload = batch_payload(&[(1, "ask")]) + "{malformed}\n";
    let result = rig.up(agent_gen(3, 4).output(2, payload.as_bytes()));
    assert!(matches!(result, Err(KernelError::AgentRecord(_))));
    assert!(rig.publication().is_none());
    assert!(any_effect!(
        rig.fx,
        KernelEffect::Status(KernelStatus::ResyncRequired {
            reason: TombstoneReason::CodecFailure,
            ..
        })
    ));
    assert!(matches!(
        rig.batch(1, &[(1, "stop")]),
        Err(KernelError::RetiredGeneration { .. })
    ));
}

// PHA-406/PHA-276: `KernelInput::Event` folds a subscribed `AgentEvent` into
// the frontend status it produces.

#[test]
fn cwd_and_command_events_become_statuses_in_order() {
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    let id = terminal(1);
    rig.ok(attach_started(1, std::slice::from_ref(&id)));
    let cases = [
        (
            AgentEvent::CwdChanged {
                cwd: "/srv/app".to_owned(),
            },
            Some(KernelStatus::Cwd {
                terminal_id: id.clone(),
                cwd: "/srv/app".to_owned(),
            }),
        ),
        (
            AgentEvent::CommandStarted,
            Some(KernelStatus::CommandStarted {
                terminal_id: id.clone(),
            }),
        ),
        (
            AgentEvent::CommandFinished { exit_code: Some(1) },
            Some(KernelStatus::CommandFinished {
                terminal_id: id.clone(),
                exit_code: Some(1),
            }),
        ),
        // Kinds not surfaced as status are silent no-ops.
        (AgentEvent::Bell, None),
    ];
    for (event, status) in cases {
        rig.ok(event_input(&id, &event));
        let expected: Vec<_> = status.into_iter().map(KernelEffect::Status).collect();
        assert_eq!(rig.fx.as_slice(), expected.as_slice());
    }
}

#[test]
fn resource_closed_exit_and_signal_become_exited_status() {
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    let id = terminal(1);
    rig.ok(attach_started(1, std::slice::from_ref(&id)));
    rig.ok(KernelInput::ResourceClosed {
        terminal_id: &id,
        exit_status: Some(137),
        signal: Some(9),
        reason: CloseReason::Killed,
    });
    assert_eq!(
        rig.fx.as_slice(),
        &[KernelEffect::Status(KernelStatus::Exited {
            terminal_id: id.clone(),
            exit_status: Some(137),
            signal: Some(9),
            reason: CloseReason::Killed,
        })]
    );
}

/// A retained pane (D3) reports its exit once via `TerminalControl` and
/// stays open; later lease changes restamp `lifecycle: Exited` and the
/// eventual `RESOURCE_CLOSED` must not report it again.
#[test]
fn a_retained_exit_reports_status_once_without_closing_the_pane() {
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    let g = gen_of(1, 1, 1);
    rig.publish(&g, 0);
    rig.fx.clear();
    let control = |action, exit_status| AgentEvent::TerminalControl {
        lifecycle: ResourceLifecycle::Exited,
        exit_status,
        input_holder: None,
        action,
        actor: None,
    };

    rig.ok(event_input(&g.id, &control(ControlAction::Exited, Some(7))));
    assert_eq!(
        rig.fx.as_slice(),
        &[KernelEffect::Status(KernelStatus::Exited {
            terminal_id: g.id.clone(),
            exit_status: Some(7),
            signal: None,
            reason: CloseReason::Exited,
        })]
    );
    assert!(
        !rig.k.closed.contains(&g.id),
        "exit must not close the pane"
    );
    assert!(rig.k.published(&g.id).is_some(), "the replica survives");

    rig.ok(event_input(
        &g.id,
        &control(ControlAction::Interrupted, None),
    ));
    assert!(rig.fx.is_empty(), "a lease change must not reemit Exited");

    rig.ok(KernelInput::ResourceClosed {
        terminal_id: &g.id,
        exit_status: Some(7),
        signal: None,
        reason: CloseReason::Exited,
    });
    assert!(
        !any_effect!(rig.fx, KernelEffect::Status(KernelStatus::Exited { .. })),
        "the purge must not report Exited again"
    );
}

#[test]
fn event_for_an_untracked_or_already_closed_terminal_is_ignored() {
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    let event = AgentEvent::CwdChanged {
        cwd: "/tmp".to_owned(),
    };

    // Never attached, bootstrapped, or declared.
    rig.ok(event_input(&terminal(99), &event));
    assert!(
        rig.fx.is_empty(),
        "an event for a terminal this kernel never tracked must be a no-op"
    );

    // Closed, though `self.kinds` still knows its kind.
    let gone = terminal(1);
    rig.ok(attach_started(1, std::slice::from_ref(&gone)));
    rig.ok(closed(&gone));
    rig.fx.clear();
    rig.ok(event_input(&gone, &event));
    assert!(
        rig.fx.is_empty(),
        "an event for an already-closed terminal must be a no-op"
    );
}

fn controller_view() -> phux_protocol::native_agent::NativeView {
    use phux_protocol::native_agent::{NativeCapabilities, NativeQuestion, NativeView};
    NativeView {
        activation: "host-activation".into(),
        backend: "acp".into(),
        capabilities: NativeCapabilities {
            prompt: true.into(),
            interrupt: true.into(),
            questions: true.into(),
            follow_up: true.into(),
        },
        connected: true,
        turn_id: Some("actual-turn".into()),
        provider_session_id: Some("actual-provider-session".into()),
        questions: vec![NativeQuestion {
            id: "actual-question".into(),
            kind: "confirm".into(),
            title: "Confirm".into(),
            message: "Proceed?".into(),
            options: vec![],
            questions: vec![],
            fields: serde_json::json!({"provider_request": 17}),
        }],
    }
}

fn native_view_line(seq: u64, view: &phux_protocol::native_agent::NativeView) -> String {
    agent_line(
        seq,
        "provider_raw",
        &serde_json::json!({
            "schema": phux_protocol::native_agent::NATIVE_SCHEMA,
            "update": {"type": "view", "view": view}
        })
        .to_string(),
    )
}

/// [`agent_rig`] whose first live record is the controller view.
fn native_agent_rig() -> (Rig, phux_protocol::native_agent::NativeView) {
    let mut rig = agent_rig(0);
    let view = controller_view();
    rig.ok(agent_gen(3, 4).output(1, native_view_line(1, &view).as_bytes()));
    (rig, view)
}

#[test]
fn native_controls_require_a_full_controller_view_and_survive_log_eviction() {
    let g = agent_gen(3, 4);
    // Provider declaration, even one naming a native-capable provider, grants
    // no SDK authority.
    assert_eq!(agent_rig(0).k.native_agent_view(&g.id), None);
    let (mut rig, view) = native_agent_rig();
    assert_eq!(rig.k.native_agent_view(&g.id), Some(view.clone()));
    assert_eq!(rig.k.native_agent_view(&terminal(1)), None);
    let last = super::agent_stream::DEFAULT_LOG_CAPACITY as u64 + 2;
    let payload: String = (2..=last)
        .map(|seq| agent_line(seq, "provider_raw", "{}"))
        .collect();
    rig.ok(g.output(last, payload.as_bytes()));
    assert!(!rig.seqs().contains(&1));
    assert_eq!(rig.k.native_agent_view(&g.id), Some(view));
}

#[test]
fn native_view_replacement_publishes_atomically_and_never_infers_a_lost_activation() {
    let (mut rig, original) = native_agent_rig();
    let agent = terminal(2);
    let mut replacement = original.clone();
    replacement.activation = "replacement-activation".into();
    replacement.questions.clear();
    replacement.turn_id = None;
    replacement.connected = false;
    let next = agent_gen(5, 6);
    rig.ok(next.agent_begin(2));
    rig.ok(next.chunk(0, native_view_line(2, &replacement).as_bytes()));
    assert_eq!(rig.k.native_agent_view(&agent), Some(original));
    rig.ok(next.ready(None));
    assert_eq!(rig.k.native_agent_view(&agent), Some(replacement));
    // A fresh retained generation with no controller view must not preserve
    // the old activation, even when its resource/provider identity matches.
    let fresh = agent_gen(7, 8);
    rig.ok(fresh.agent_begin(2));
    rig.ok(fresh.ready(None));
    assert!(!rig.k.has_native_agent_view(&agent));
}

#[test]
fn malformed_reserved_native_view_retracts_controls_but_unrelated_raw_does_not() {
    let (mut rig, view) = native_agent_rig();
    let g = agent_gen(3, 4);
    let unrelated = agent_line(
        2,
        "provider_raw",
        r#"{"schema":"phux.transcript/v1","entry":{"text":"hi"}}"#,
    );
    rig.ok(g.output(2, unrelated.as_bytes()));
    assert_eq!(rig.k.native_agent_view(&g.id), Some(view));
    let malformed = agent_line(
        3,
        "provider_raw",
        r#"{"schema":"phux.native-agent/v1","update":{"type":"view","view":{"activation":"bogus"}}}"#,
    );
    rig.ok(g.output(3, malformed.as_bytes()));
    assert_eq!(rig.k.native_agent_view(&g.id), None);
}

#[test]
fn closing_native_source_or_parent_retains_a_readable_but_disconnected_view() {
    for source in [terminal(1), terminal(2)] {
        let (mut rig, view) = native_agent_rig();
        rig.ok(closed(&source));
        let retained = rig.k.native_agent_view(&terminal(2)).unwrap();
        assert_eq!(retained.activation, view.activation);
        assert_eq!(retained.backend, view.backend);
        assert!(!retained.connected);
        assert!(retained.questions.is_empty());
        assert_eq!(retained.turn_id, None);
        rig.k.release_incarnation();
        assert_eq!(rig.k.native_agent_view(&terminal(2)), None);
    }
}

mod agent_resume;
