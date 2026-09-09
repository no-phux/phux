use phux_protocol::caps::{
    BootstrapCapabilities, ClientCapabilities, EngineCodec, EngineFeatureSet,
    select_bootstrap_profile,
};
use phux_protocol::input::{InputEvent, focus::FocusEvent};
use phux_protocol::wire::frame::TombstoneReason;
use phux_protocol::{
    BootstrapId, BootstrapProfile, BootstrapStreamProfile, ResourceKind, StreamId, TerminalId,
};

use super::agent_stream::AgentSessionStatus;
use super::{
    EffectBuffer, HistoryRejectionReason, HistoryUnavailableReason, InputBlockReason,
    InputEligibility, KernelAction, KernelDamage, KernelDamageKind, KernelEffect, KernelError,
    KernelInput, KernelJob, KernelSend, KernelStatus, SessionKernel, TombstoneRecord,
};
use crate::engine::{
    BootstrapProgress, CanonicalGeometry, EngineAdapter, EngineDamage, EngineEffect,
    EngineEffectBuffer, EngineJob, EngineSend, EngineStatus, HistoryApplyOutcome,
};
use crate::history::HistoryLoadState;

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
        ) {
            return Err(FakeError::UnsupportedProfile);
        }
        Ok(FakeReplica {
            geometry,
            transcript: Vec::new(),
            finish_effects: false,
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
            effects.push(EngineEffect::Send(EngineSend::PtyWrite(
                b"suppressed-bootstrap-reply".to_vec(),
            )));
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
            effects.push(EngineEffect::Send(EngineSend::PtyWrite(
                b"suppressed-finish-reply".to_vec(),
            )));
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
        Ok(HistoryApplyOutcome {
            progress: BootstrapProgress::Ready,
            retained: payload != b"history-not-retained",
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
            effects.push(EngineEffect::Send(EngineSend::PtyWrite(b"reply".to_vec())));
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
                codec: EngineCodec::LibghosttyCheckpointV2,
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
            retained: true,
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

fn terminal(raw: u32) -> TerminalId {
    TerminalId::local(raw)
}

fn stream(raw: u64) -> StreamId {
    let Some(id) = StreamId::new(raw) else {
        panic!("test stream id must be non-zero");
    };
    id
}

fn bootstrap(raw: u64) -> BootstrapId {
    let Some(id) = BootstrapId::new(raw) else {
        panic!("test bootstrap id must be non-zero");
    };
    id
}

fn geometry() -> CanonicalGeometry {
    let Some(geometry) = CanonicalGeometry::new(80, 24) else {
        panic!("test geometry must be non-empty");
    };
    geometry
}

fn kernel(mode: ReadyMode) -> SessionKernel<FakeAdapter> {
    kernel_with_profile(mode, BootstrapProfile::SynthesizedVtRaw)
}

fn kernel_with_profile(mode: ReadyMode, profile: BootstrapProfile) -> SessionKernel<FakeAdapter> {
    SessionKernel::new(FakeAdapter { ready_mode: mode }, profile)
}

#[test]
fn release_terminal_preserves_initial_attach_inventory_and_barrier() {
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let id = terminal(1);
    let mut effects = EffectBuffer::new();
    kernel
        .update(
            KernelInput::AttachStarted {
                attach_id: 10,
                terminals: std::slice::from_ref(&id),
            },
            &mut effects,
        )
        .unwrap();
    assert!(!kernel.release_terminal(&id));
    assert!(kernel.active_attach_contains(&id));
    kernel
        .update(
            KernelInput::TerminalClosed { terminal_id: &id },
            &mut effects,
        )
        .unwrap();
    assert!(!kernel.release_terminal(&id));
    assert!(kernel.closed.contains(&id));
    kernel
        .update(KernelInput::AttachReady { attach_id: 10 }, &mut effects)
        .unwrap();
    kernel.release_active_attach();
    assert!(kernel.release_terminal(&id));
    assert!(!kernel.closed.contains(&id));
}

#[test]
fn explicit_detach_cannot_bypass_initial_barrier_but_withdraws_released_participation() {
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let id = terminal(1);
    let mut effects = EffectBuffer::new();
    kernel
        .update(
            KernelInput::AttachStarted {
                attach_id: 10,
                terminals: std::slice::from_ref(&id),
            },
            &mut effects,
        )
        .unwrap();
    assert!(!kernel.detach_terminal(&id));
    assert!(kernel.active_attach_contains(&id));
    assert!(
        kernel
            .update(KernelInput::AttachReady { attach_id: 10 }, &mut effects)
            .is_err()
    );
    publish_direct(&mut kernel, &id, stream(1), bootstrap(1), 0, &mut effects);
    kernel
        .update(KernelInput::AttachReady { attach_id: 10 }, &mut effects)
        .unwrap();
    assert!(!kernel.release_terminal(&id));
    effects.clear();
    assert!(kernel.detach_terminal(&id));
    assert!(!kernel.active_attach_contains(&id));
    assert!(kernel.terminals.is_empty());
    assert!(kernel.closed.is_empty());
    for n in 2..=512 {
        let next = terminal(n);
        publish_direct(
            &mut kernel,
            &next,
            stream(n.into()),
            bootstrap(1),
            0,
            &mut effects,
        );
        assert!(kernel.detach_terminal(&next));
        assert!(kernel.terminals.is_empty());
        assert!(kernel.closed.is_empty());
        effects.clear();
    }
}

#[test]
fn release_terminal_reclaims_churn_and_allows_explicit_subscription_replacement() {
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    for n in 1..=512 {
        let id = terminal(n);
        publish_direct(&mut kernel, &id, stream(1), bootstrap(1), 0, &mut effects);
        assert!(kernel.release_terminal(&id));
        assert!(kernel.terminals.is_empty());
        assert!(kernel.closed.is_empty());
        // Same ID can acquire a new explicitly admitted subscription.
        begin(&mut kernel, &id, stream(2), bootstrap(1), 0, &mut effects);
        kernel
            .update(
                KernelInput::TerminalClosed { terminal_id: &id },
                &mut effects,
            )
            .unwrap();
        assert!(kernel.closed.contains(&id));
        assert!(kernel.release_terminal(&id));
        assert!(kernel.terminals.is_empty());
        assert!(kernel.closed.is_empty());
        effects.clear();
    }
}

fn begin(
    kernel: &mut SessionKernel<FakeAdapter>,
    terminal_id: &TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    base_seq: u64,
    effects: &mut EffectBuffer,
) {
    kernel
        .update(
            KernelInput::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                geometry: geometry(),
                base_seq,
            },
            effects,
        )
        .unwrap();
}

fn push_ready_transcript(
    kernel: &mut SessionKernel<FakeAdapter>,
    terminal_id: &TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    effects: &mut EffectBuffer,
) {
    let chunks: [&[u8]; 4] = [
        b"\x1b[2Jsynthe",
        b"sized-vt-v1",
        &READY_MARKER[..9],
        &READY_MARKER[9..],
    ];
    for (chunk_seq, payload) in (0_u32..).zip(chunks) {
        kernel
            .update(
                KernelInput::BootstrapChunk {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    chunk_seq,
                    payload,
                },
                effects,
            )
            .unwrap();
    }
}

fn protocol_ready(
    kernel: &mut SessionKernel<FakeAdapter>,
    terminal_id: &TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    effects: &mut EffectBuffer,
) {
    kernel
        .update(
            KernelInput::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
            effects,
        )
        .unwrap();
}

fn publish_direct(
    kernel: &mut SessionKernel<FakeAdapter>,
    terminal_id: &TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    base_seq: u64,
    effects: &mut EffectBuffer,
) {
    begin(
        kernel,
        terminal_id,
        stream_id,
        bootstrap_id,
        base_seq,
        effects,
    );
    push_ready_transcript(kernel, terminal_id, stream_id, bootstrap_id, effects);
    protocol_ready(kernel, terminal_id, stream_id, bootstrap_id, effects);
}

fn publish_direct_with_history(
    kernel: &mut SessionKernel<FakeAdapter>,
    terminal_id: &TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    base_seq: u64,
    cursor: &[u8],
    effects: &mut EffectBuffer,
) {
    begin(
        kernel,
        terminal_id,
        stream_id,
        bootstrap_id,
        base_seq,
        effects,
    );
    push_ready_transcript(kernel, terminal_id, stream_id, bootstrap_id, effects);
    kernel
        .update(
            KernelInput::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: Some(cursor),
            },
            effects,
        )
        .unwrap();
}

#[test]
fn dual_ready_orders_and_fragmentation_hold_first_damage() {
    for mode in [ReadyMode::ChunkFirst, ReadyMode::ProtocolFirst] {
        let terminal_id = terminal(1);
        let stream_id = stream(11);
        let bootstrap_id = bootstrap(21);
        let mut kernel = kernel(mode);
        let mut effects = EffectBuffer::new();

        kernel
            .update(
                KernelInput::AttachStarted {
                    attach_id: 7,
                    terminals: std::slice::from_ref(&terminal_id),
                },
                &mut effects,
            )
            .unwrap();
        begin(
            &mut kernel,
            &terminal_id,
            stream_id,
            bootstrap_id,
            100,
            &mut effects,
        );
        push_ready_transcript(
            &mut kernel,
            &terminal_id,
            stream_id,
            bootstrap_id,
            &mut effects,
        );

        let staging = kernel.staging(&terminal_id).unwrap();
        assert_eq!(staging.geometry(), geometry());
        assert_eq!(staging.engine().geometry, geometry());
        assert_eq!(
            staging.engine_ready(),
            matches!(mode, ReadyMode::ChunkFirst)
        );
        assert!(!staging.protocol_ready());
        assert!(kernel.published(&terminal_id).is_none());
        assert!(effects.is_empty());

        protocol_ready(
            &mut kernel,
            &terminal_id,
            stream_id,
            bootstrap_id,
            &mut effects,
        );
        let published = kernel.published(&terminal_id).unwrap();
        assert_eq!(published.geometry(), geometry());
        assert_eq!(published.engine().geometry, geometry());
        assert_eq!(published.last_seq(), 100);
        assert_eq!(
            published.key().profile,
            BootstrapStreamProfile::SynthesizedVtRaw
        );
        assert!(published.engine().transcript.ends_with(READY_MARKER));
        assert!(effects.is_empty(), "publication is behind ATTACH_READY");

        kernel
            .update(KernelInput::AttachReady { attach_id: 7 }, &mut effects)
            .unwrap();
        assert_eq!(
            effects.as_slice(),
            &[KernelEffect::Damage(KernelDamage {
                terminal_id: terminal_id.clone(),
                kind: KernelDamageKind::Full,
            })]
        );
    }
}

#[test]
fn chunk_sequence_rejects_duplicates_and_gaps_without_applying() {
    let terminal_id = terminal(2);
    let stream_id = stream(12);
    let bootstrap_id = bootstrap(22);
    let mut kernel = kernel(ReadyMode::ProtocolFirst);
    let mut effects = EffectBuffer::new();
    begin(
        &mut kernel,
        &terminal_id,
        stream_id,
        bootstrap_id,
        0,
        &mut effects,
    );

    kernel
        .update(
            KernelInput::BootstrapChunk {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: b"first",
            },
            &mut effects,
        )
        .unwrap();
    let duplicate = kernel.update(
        KernelInput::BootstrapChunk {
            terminal_id: &terminal_id,
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: b"duplicate",
        },
        &mut effects,
    );
    assert!(matches!(
        duplicate,
        Err(KernelError::DuplicateChunk {
            expected: 1,
            actual: 0
        })
    ));
    let gap = kernel.update(
        KernelInput::BootstrapChunk {
            terminal_id: &terminal_id,
            stream_id,
            bootstrap_id,
            chunk_seq: 2,
            payload: b"gap",
        },
        &mut effects,
    );
    assert!(matches!(
        gap,
        Err(KernelError::ChunkGap {
            expected: 1,
            actual: 2
        })
    ));
    assert_eq!(
        kernel.staging(&terminal_id).unwrap().engine().transcript,
        b"first"
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn raw_sequence_ids_and_tombstones_are_exact() {
    let terminal_id = terminal(3);
    let stream_id = stream(13);
    let bootstrap_id = bootstrap(23);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    publish_direct(
        &mut kernel,
        &terminal_id,
        stream_id,
        bootstrap_id,
        41,
        &mut effects,
    );

    kernel
        .update(
            KernelInput::TerminalOutput {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                seq: 42,
                payload: b"first-live",
            },
            &mut effects,
        )
        .unwrap();
    assert_eq!(kernel.published(&terminal_id).unwrap().last_seq(), 42);
    let applied = kernel
        .published(&terminal_id)
        .unwrap()
        .engine()
        .transcript
        .clone();

    let duplicate = kernel.update(
        KernelInput::TerminalOutput {
            terminal_id: &terminal_id,
            stream_id,
            bootstrap_id,
            seq: 42,
            payload: b"duplicate",
        },
        &mut effects,
    );
    assert!(matches!(
        duplicate,
        Err(KernelError::DuplicateSequence {
            expected: 43,
            actual: 42
        })
    ));
    let gap = kernel.update(
        KernelInput::TerminalOutput {
            terminal_id: &terminal_id,
            stream_id,
            bootstrap_id,
            seq: 44,
            payload: b"gap",
        },
        &mut effects,
    );
    assert!(matches!(
        gap,
        Err(KernelError::SequenceGap {
            expected: 43,
            actual: 44
        })
    ));
    let wrong_stream = kernel.update(
        KernelInput::TerminalOutput {
            terminal_id: &terminal_id,
            stream_id: stream(999),
            bootstrap_id,
            seq: 43,
            payload: b"wrong-stream",
        },
        &mut effects,
    );
    assert!(matches!(
        wrong_stream,
        Err(KernelError::GenerationMismatch { .. })
    ));
    let wrong_bootstrap = kernel.update(
        KernelInput::TerminalOutput {
            terminal_id: &terminal_id,
            stream_id,
            bootstrap_id: bootstrap(999),
            seq: 43,
            payload: b"wrong-bootstrap",
        },
        &mut effects,
    );
    assert!(matches!(
        wrong_bootstrap,
        Err(KernelError::GenerationMismatch { .. })
    ));
    let wrong_terminal_id = terminal(999);
    let wrong_terminal = kernel.update(
        KernelInput::TerminalOutput {
            terminal_id: &wrong_terminal_id,
            stream_id,
            bootstrap_id,
            seq: 43,
            payload: b"wrong-terminal",
        },
        &mut effects,
    );
    assert!(matches!(
        wrong_terminal,
        Err(KernelError::UnknownTerminal(_))
    ));
    assert_eq!(
        kernel.published(&terminal_id).unwrap().engine().transcript,
        applied
    );

    kernel
        .update(
            KernelInput::Tombstone {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                reason: TombstoneReason::OutboundGap,
                last_valid_seq: 42,
            },
            &mut effects,
        )
        .unwrap();
    assert_eq!(
        kernel.published(&terminal_id).unwrap().engine().transcript,
        applied
    );
    assert_eq!(
        kernel
            .tombstone(&terminal_id, stream_id, bootstrap_id)
            .unwrap()
            .last_valid_seq,
        42
    );
    assert_eq!(
        kernel.input_eligibility(&terminal_id),
        InputEligibility::Ineligible(InputBlockReason::FrozenReplica)
    );
    let stale = kernel.update(
        KernelInput::TerminalOutput {
            terminal_id: &terminal_id,
            stream_id,
            bootstrap_id,
            seq: 43,
            payload: b"stale",
        },
        &mut effects,
    );
    assert!(matches!(stale, Err(KernelError::RetiredGeneration { .. })));
    let replacement = bootstrap(24);
    begin(
        &mut kernel,
        &terminal_id,
        stream_id,
        replacement,
        42,
        &mut effects,
    );
    push_ready_transcript(
        &mut kernel,
        &terminal_id,
        stream_id,
        replacement,
        &mut effects,
    );
    assert_eq!(
        kernel.published(&terminal_id).unwrap().key().bootstrap_id,
        bootstrap_id,
        "frozen view remains visible while replacement stages"
    );
    protocol_ready(
        &mut kernel,
        &terminal_id,
        stream_id,
        replacement,
        &mut effects,
    );
    assert_eq!(
        kernel.published(&terminal_id).unwrap().key().bootstrap_id,
        replacement
    );
    assert_eq!(
        kernel.tombstone(&terminal_id, stream_id, bootstrap_id),
        Some(TombstoneRecord {
            reason: TombstoneReason::OutboundGap,
            last_valid_seq: 42,
        }),
        "replacement publication must not overwrite the authoritative tombstone"
    );
}

#[test]
fn published_history_is_generation_bound_and_interleaves_without_advancing_live_seq() {
    let terminal_id = terminal(30);
    let stream_id = stream(130);
    let bootstrap_id = bootstrap(230);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    publish_direct_with_history(
        &mut kernel,
        &terminal_id,
        stream_id,
        bootstrap_id,
        40,
        b"cursor-0",
        &mut effects,
    );

    kernel
        .update(
            KernelInput::HistoryPage {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                cursor: b"cursor-0",
                next_cursor: Some(b"cursor-1"),
                payload: b"history-one",
                page_seq: 1,
                rows: 0,
            },
            &mut effects,
        )
        .unwrap();
    assert!(effects.as_slice().iter().any(|effect| matches!(
        effect,
        KernelEffect::Send(KernelSend::HistoryRequest { cursor, .. })
            if cursor == b"cursor-1"
    )));
    assert_eq!(kernel.published(&terminal_id).unwrap().last_seq(), 40);

    kernel
        .update(
            KernelInput::TerminalOutput {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                seq: 41,
                payload: b"live-between-history",
            },
            &mut effects,
        )
        .unwrap();
    assert!(!kernel.prefetch_history(&terminal_id, 0, &mut effects));
    kernel
        .update(
            KernelInput::HistoryPage {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                cursor: b"cursor-1",
                next_cursor: Some(b"cursor-2"),
                payload: b"history-two",
                page_seq: 1,
                rows: 0,
            },
            &mut effects,
        )
        .unwrap();
    let published = kernel.published(&terminal_id).unwrap();
    assert_eq!(published.last_seq(), 41);
    assert!(
        published
            .engine()
            .transcript
            .ends_with(b"history-onelive-between-historyhistory-two")
    );

    let before_wrong_generation = published.engine().transcript.clone();
    let wrong_generation = kernel.update(
        KernelInput::HistoryPage {
            terminal_id: &terminal_id,
            stream_id,
            bootstrap_id: bootstrap(999),
            cursor: b"wrong-cursor",
            next_cursor: Some(b"wrong-next"),
            payload: b"wrong-generation",
            page_seq: 1,
            rows: 0,
        },
        &mut effects,
    );
    assert!(matches!(
        wrong_generation,
        Err(KernelError::GenerationMismatch { .. })
    ));
    assert_eq!(
        kernel.published(&terminal_id).unwrap().engine().transcript,
        before_wrong_generation
    );
}

#[test]
fn delayed_history_tombstone_for_an_advanced_cursor_is_ignored() {
    let terminal_id = terminal(31);
    let stream_id = stream(131);
    let bootstrap_id = bootstrap(231);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    publish_direct_with_history(
        &mut kernel,
        &terminal_id,
        stream_id,
        bootstrap_id,
        40,
        b"cursor-0",
        &mut effects,
    );
    effects.clear();

    kernel
        .update(
            KernelInput::HistoryPage {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                cursor: b"cursor-0",
                next_cursor: Some(b"cursor-1"),
                payload: b"history-one",
                page_seq: 1,
                rows: 0,
            },
            &mut effects,
        )
        .unwrap();
    assert_eq!(
        kernel.history_cache(&terminal_id).unwrap().status().state,
        HistoryLoadState::Loading,
        "the first page advances the outstanding request to cursor-1"
    );
    effects.clear();

    let delayed = kernel.update(
        KernelInput::HistoryTombstone {
            terminal_id: &terminal_id,
            stream_id,
            bootstrap_id,
            cursor: b"cursor-0",
            reason: HistoryUnavailableReason::Expired,
        },
        &mut effects,
    );
    assert!(
        delayed.is_ok(),
        "a late tombstone for cursor-0 must not disconnect the live pane: {delayed:?}"
    );
    assert_eq!(
        kernel.history_cache(&terminal_id).unwrap().status().state,
        HistoryLoadState::Loading,
        "the newer cursor remains usable"
    );
    assert!(
        effects.as_slice().is_empty(),
        "ignoring an obsolete cursor must not change visible history state"
    );
}

#[test]
fn retained_history_limit_and_prefetch_threshold_are_explicit() {
    let terminal_id = terminal(35);
    let stream_id = stream(135);
    let bootstrap_id = bootstrap(235);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    publish_direct_with_history(
        &mut kernel,
        &terminal_id,
        stream_id,
        bootstrap_id,
        40,
        b"cursor-0",
        &mut effects,
    );
    effects.clear();
    kernel
        .update(
            KernelInput::HistoryPage {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                cursor: b"cursor-0",
                next_cursor: Some(b"cursor-1"),
                payload: b"history-page",
                page_seq: 1,
                rows: 128,
            },
            &mut effects,
        )
        .unwrap();
    assert!(effects.as_slice().iter().all(|effect| !matches!(
        effect,
        KernelEffect::Send(KernelSend::HistoryRequest { .. })
    )));
    assert!(kernel.prefetch_history(&terminal_id, 0, &mut effects));

    let limited = terminal(36);
    publish_direct_with_history(
        &mut kernel,
        &limited,
        stream_id,
        bootstrap_id,
        50,
        b"limited-0",
        &mut effects,
    );
    effects.clear();
    kernel
        .update(
            KernelInput::HistoryPage {
                terminal_id: &limited,
                stream_id,
                bootstrap_id,
                cursor: b"limited-0",
                next_cursor: Some(b"limited-1"),
                payload: b"history-not-retained",
                page_seq: 1,
                rows: 1,
            },
            &mut effects,
        )
        .unwrap();
    assert!(effects.as_slice().iter().any(|effect| matches!(
        effect,
        KernelEffect::Status(KernelStatus::HistoryUnavailable {
            reason: HistoryUnavailableReason::Limit,
            ..
        })
    )));
    assert_eq!(
        kernel.history_cache(&limited).unwrap().status().state,
        HistoryLoadState::Tombstoned
    );
}

#[test]
fn history_engine_failure_invalidates_only_history_and_live_output_continues() {
    let terminal_id = terminal(31);
    let stream_id = stream(131);
    let bootstrap_id = bootstrap(231);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    publish_direct_with_history(
        &mut kernel,
        &terminal_id,
        stream_id,
        bootstrap_id,
        73,
        b"cursor-0",
        &mut effects,
    );
    effects.clear();

    let failed = kernel.update(
        KernelInput::HistoryPage {
            terminal_id: &terminal_id,
            stream_id,
            bootstrap_id,
            page_seq: 1,
            rows: 0,
            cursor: b"cursor-0",
            next_cursor: Some(b"cursor-1"),
            payload: b"history-error",
        },
        &mut effects,
    );
    assert!(matches!(
        failed,
        Err(KernelError::Engine(FakeError::MutatedThenFailed))
    ));
    assert!(
        kernel
            .tombstone(&terminal_id, stream_id, bootstrap_id)
            .is_none()
    );
    assert!(
        !kernel
            .published(&terminal_id)
            .unwrap()
            .engine()
            .transcript
            .ends_with(b"history-error"),
        "history import errors are transactional"
    );
    assert!(matches!(
        kernel.input_eligibility(&terminal_id),
        InputEligibility::Eligible {
            stream_id: actual_stream,
            bootstrap_id: actual_bootstrap,
        } if actual_stream == stream_id && actual_bootstrap == bootstrap_id
    ));
    assert!(effects.as_slice().iter().any(|effect| matches!(
        effect,
        KernelEffect::Status(KernelStatus::History { status, .. })
            if status.state == HistoryLoadState::Tombstoned
    )));
    assert!(effects.as_slice().iter().all(|effect| !matches!(
        effect,
        KernelEffect::Status(KernelStatus::ResyncRequired { .. })
            | KernelEffect::Damage(KernelDamage {
                kind: KernelDamageKind::Removed,
                ..
            })
    )));

    effects.clear();
    kernel
        .update(
            KernelInput::TerminalOutput {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                seq: 74,
                payload: b"live-after-history-error",
            },
            &mut effects,
        )
        .unwrap();
    assert!(
        kernel
            .published(&terminal_id)
            .unwrap()
            .engine()
            .transcript
            .ends_with(b"live-after-history-error")
    );
}

#[test]
fn oversized_history_rejection_ends_history_without_retiring_live_generation() {
    let terminal_id = terminal(32);
    let stream_id = stream(132);
    let bootstrap_id = bootstrap(232);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    publish_direct_with_history(
        &mut kernel,
        &terminal_id,
        stream_id,
        bootstrap_id,
        80,
        b"cursor-0",
        &mut effects,
    );

    effects.clear();
    kernel
        .update(
            KernelInput::HistoryRejected {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                cursor: b"cursor-0",
                reason: HistoryRejectionReason::TooSmall,
                required_bytes: u32::MAX,
                required_rows: 1,
            },
            &mut effects,
        )
        .unwrap();
    assert!(effects.as_slice().iter().any(|effect| matches!(
        effect,
        KernelEffect::Status(KernelStatus::History { status, .. })
            if status.state == HistoryLoadState::Tombstoned
                && status.next_cursor.is_none()
    )));
    assert!(effects.as_slice().iter().any(|effect| matches!(
        effect,
        KernelEffect::Status(KernelStatus::HistoryUnavailable {
            reason: HistoryUnavailableReason::Limit,
            ..
        })
    )));
    assert!(effects.as_slice().iter().all(|effect| !matches!(
        effect,
        KernelEffect::Send(KernelSend::HistoryRequest { .. })
    )));

    kernel
        .update(
            KernelInput::TerminalOutput {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                seq: 81,
                payload: b"live-after-limit",
            },
            &mut effects,
        )
        .unwrap();
    assert_eq!(kernel.published(&terminal_id).unwrap().last_seq(), 81);
}

#[test]
#[allow(clippy::too_many_lines)]
fn replacement_is_atomic_and_old_view_remains_live_until_swap() {
    let terminal_id = terminal(4);
    let stream_id = stream(14);
    let old_bootstrap = bootstrap(24);
    let new_bootstrap = bootstrap(25);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    publish_direct(
        &mut kernel,
        &terminal_id,
        stream_id,
        old_bootstrap,
        0,
        &mut effects,
    );
    let old_view = kernel
        .published(&terminal_id)
        .unwrap()
        .engine()
        .transcript
        .clone();

    begin(
        &mut kernel,
        &terminal_id,
        stream_id,
        new_bootstrap,
        10,
        &mut effects,
    );
    kernel
        .update(
            KernelInput::BootstrapChunk {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id: new_bootstrap,
                chunk_seq: 0,
                payload: b"replacement-prefix",
            },
            &mut effects,
        )
        .unwrap();
    assert_eq!(
        kernel.published(&terminal_id).unwrap().engine().transcript,
        old_view
    );

    kernel
        .update(
            KernelInput::TerminalOutput {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id: old_bootstrap,
                seq: 1,
                payload: b"old-still-live",
            },
            &mut effects,
        )
        .unwrap();
    kernel
        .update(
            KernelInput::BootstrapChunk {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id: new_bootstrap,
                chunk_seq: 1,
                payload: READY_MARKER,
            },
            &mut effects,
        )
        .unwrap();
    protocol_ready(
        &mut kernel,
        &terminal_id,
        stream_id,
        new_bootstrap,
        &mut effects,
    );
    assert_eq!(
        kernel.published(&terminal_id).unwrap().key().bootstrap_id,
        new_bootstrap
    );
    assert_eq!(kernel.published(&terminal_id).unwrap().last_seq(), 10);
    assert!(
        kernel
            .published(&terminal_id)
            .unwrap()
            .engine()
            .transcript
            .starts_with(b"replacement-prefix")
    );
    assert!(
        kernel
            .tombstone(&terminal_id, stream_id, old_bootstrap)
            .is_some()
    );

    let stale = kernel.update(
        KernelInput::TerminalOutput {
            terminal_id: &terminal_id,
            stream_id,
            bootstrap_id: old_bootstrap,
            seq: 2,
            payload: b"stale-old",
        },
        &mut effects,
    );
    assert!(matches!(stale, Err(KernelError::RetiredGeneration { .. })));
}

#[test]
fn two_pane_attach_barrier_accepts_one_ready_and_one_close() {
    let ready_terminal = terminal(5);
    let closed_terminal = terminal(6);
    let stream_id = stream(15);
    let bootstrap_id = bootstrap(26);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    kernel
        .update(
            KernelInput::AttachStarted {
                attach_id: 8,
                terminals: &[ready_terminal.clone(), closed_terminal.clone()],
            },
            &mut effects,
        )
        .unwrap();
    publish_direct(
        &mut kernel,
        &ready_terminal,
        stream_id,
        bootstrap_id,
        4,
        &mut effects,
    );
    assert!(effects.is_empty());
    kernel
        .update(
            KernelInput::TerminalClosed {
                terminal_id: &closed_terminal,
            },
            &mut effects,
        )
        .unwrap();
    assert!(effects.is_empty());

    kernel
        .update(KernelInput::AttachReady { attach_id: 8 }, &mut effects)
        .unwrap();
    assert_eq!(
        effects.as_slice(),
        &[KernelEffect::Damage(KernelDamage {
            terminal_id: ready_terminal.clone(),
            kind: KernelDamageKind::Full,
        })]
    );
    assert!(matches!(
        kernel.input_eligibility(&ready_terminal),
        InputEligibility::Eligible { .. }
    ));
    assert_eq!(
        kernel.input_eligibility(&closed_terminal),
        InputEligibility::Ineligible(InputBlockReason::Closed)
    );
}

#[test]
fn selected_synth_profile_is_explicit_and_enforced() {
    let terminal_id = terminal(7);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    assert_eq!(
        kernel.selected_profile(),
        BootstrapProfile::SynthesizedVtRaw
    );

    let mismatched = kernel.update(
        KernelInput::BootstrapBegin {
            terminal_id: &terminal_id,
            stream_id: stream(16),
            bootstrap_id: bootstrap(27),
            profile: BootstrapStreamProfile::SynthesizedVtStateSync,
            geometry: geometry(),
            base_seq: 0,
        },
        &mut effects,
    );
    assert!(matches!(
        mismatched,
        Err(KernelError::ProfileMismatch {
            selected: BootstrapProfile::SynthesizedVtRaw,
            incoming: BootstrapStreamProfile::SynthesizedVtStateSync,
        })
    ));
    assert!(kernel.staging(&terminal_id).is_none());
}

#[test]
fn host_boundary_rejects_profile_and_pre_ready_data_with_typed_errors() {
    let terminal_id = terminal(70);
    let stream_id = stream(71);
    let bootstrap_id = bootstrap(72);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    kernel
        .update(
            KernelInput::AttachStarted {
                attach_id: 73,
                terminals: std::slice::from_ref(&terminal_id),
            },
            &mut effects,
        )
        .unwrap();

    let wrong_profile = kernel.update(
        KernelInput::BootstrapBegin {
            terminal_id: &terminal_id,
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtStateSync,
            geometry: geometry(),
            base_seq: 100,
        },
        &mut effects,
    );
    assert!(matches!(
        wrong_profile,
        Err(KernelError::ProfileMismatch {
            selected: BootstrapProfile::SynthesizedVtRaw,
            incoming: BootstrapStreamProfile::SynthesizedVtStateSync,
        })
    ));
    assert!(kernel.staging(&terminal_id).is_none());

    begin(
        &mut kernel,
        &terminal_id,
        stream_id,
        bootstrap_id,
        100,
        &mut effects,
    );
    let staging_before = kernel.staging(&terminal_id).unwrap();
    let key_before = staging_before.key().clone();
    let geometry_before = staging_before.geometry();
    let engine_ready_before = staging_before.engine_ready();
    let protocol_ready_before = staging_before.protocol_ready();
    assert!(staging_before.engine().transcript.is_empty());
    assert!(effects.is_empty());
    let live_before_ready = kernel.update(
        KernelInput::TerminalOutput {
            terminal_id: &terminal_id,
            stream_id,
            bootstrap_id,
            seq: 101,
            payload: b"\xfflive-before-ready",
        },
        &mut effects,
    );
    assert!(matches!(
        live_before_ready,
        Err(KernelError::GenerationMismatch { .. })
    ));
    assert!(effects.is_empty());
    let history_before_ready = kernel.update(
        KernelInput::HistoryPage {
            terminal_id: &terminal_id,
            stream_id,
            bootstrap_id,
            cursor: b"cursor-0",
            next_cursor: Some(b"cursor-1"),
            payload: b"\xfefuture-history-before-ready",
            page_seq: 1,
            rows: 0,
        },
        &mut effects,
    );
    assert!(matches!(
        history_before_ready,
        Err(KernelError::GenerationMismatch { .. })
    ));
    assert!(effects.is_empty());
    let staging_after = kernel.staging(&terminal_id).unwrap();
    assert_eq!(staging_after.key(), &key_before);
    assert_eq!(staging_after.geometry(), geometry_before);
    assert_eq!(staging_after.engine_ready(), engine_ready_before);
    assert_eq!(staging_after.protocol_ready(), protocol_ready_before);
    assert!(staging_after.engine().transcript.is_empty());
    assert!(kernel.published(&terminal_id).is_none());
    assert!(kernel.staging(&terminal_id).is_some());
}

#[test]
#[allow(clippy::too_many_lines)]
fn selected_native_host_preserves_opaque_bytes_and_lifecycle_order() {
    let advertised = BootstrapCapabilities::new().with_native(
        EngineCodec::LibghosttyCheckpointV2,
        EngineFeatureSet::required_native(),
    );
    let client = ClientCapabilities::new().with_bootstrap(advertised);
    let (selected, _) = select_bootstrap_profile(&client, &advertised).unwrap();
    assert_eq!(
        selected,
        BootstrapProfile::NativeState {
            codec: EngineCodec::LibghosttyCheckpointV2,
            features: EngineFeatureSet::required_native(),
        }
    );
    let profile = BootstrapStreamProfile::NativeState {
        codec: EngineCodec::LibghosttyCheckpointV2,
    };
    let terminal_id = terminal(80);
    let stream_id = stream(81);
    let bootstrap_id = bootstrap(82);
    let mut kernel = SessionKernel::new(RecordingNativeAdapter, selected);
    let mut effects = EffectBuffer::new();
    kernel
        .update(
            KernelInput::BootstrapBegin {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                geometry: geometry(),
                base_seq: 900,
            },
            &mut effects,
        )
        .unwrap();
    assert!(effects.is_empty());

    let checkpoint_a: &[u8] = b"\xff\0checkpoint-a\x80";
    let checkpoint_b: &[u8] = b"\xfecheckpoint-b\0\xfd";
    for (chunk_seq, payload) in [(0, checkpoint_a), (1, checkpoint_b)] {
        kernel
            .update(
                KernelInput::BootstrapChunk {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    chunk_seq,
                    payload,
                },
                &mut effects,
            )
            .unwrap();
        assert!(effects.is_empty());
    }
    kernel
        .update(
            KernelInput::BootstrapReady {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: Some(b"cursor-0"),
            },
            &mut effects,
        )
        .unwrap();
    let published_key = kernel.published(&terminal_id).unwrap().key().clone();
    assert_eq!(published_key.terminal_id, terminal_id);
    assert_eq!(published_key.stream_id, stream_id);
    assert_eq!(published_key.bootstrap_id, bootstrap_id);
    assert!(effects.as_slice().iter().any(|effect| matches!(
        effect,
        KernelEffect::Send(KernelSend::HistoryRequest { cursor, .. })
            if cursor == b"cursor-0"
    )));
    assert!(effects.as_slice().iter().any(|effect| matches!(
        effect,
        KernelEffect::Damage(KernelDamage {
            terminal_id: damaged,
            kind: KernelDamageKind::Full,
        }) if damaged == &terminal_id
    )));

    let live_a: &[u8] = b"\x80live-a\xff";
    let history: &[u8] = b"\0\xfehistory-future\x81";
    let live_b: &[u8] = b"\xfdlive-b\0";
    kernel
        .update(
            KernelInput::TerminalOutput {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                seq: 901,
                payload: live_a,
            },
            &mut effects,
        )
        .unwrap();
    assert!(effects.is_empty());
    kernel
        .update(
            KernelInput::HistoryPage {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                cursor: b"cursor-0",
                next_cursor: Some(b"cursor-1"),
                payload: history,
                page_seq: 1,
                rows: 0,
            },
            &mut effects,
        )
        .unwrap();
    assert!(effects.as_slice().iter().any(|effect| matches!(
        effect,
        KernelEffect::Status(KernelStatus::History { status, .. })
            if status.state == HistoryLoadState::Loading
    )));
    kernel
        .update(
            KernelInput::TerminalOutput {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                seq: 902,
                payload: live_b,
            },
            &mut effects,
        )
        .unwrap();
    assert!(effects.is_empty());

    assert_eq!(
        kernel.published(&terminal_id).unwrap().engine().records,
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
fn published_recording_native(
    terminal_id: &TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
) -> SessionKernel<RecordingNativeAdapter> {
    let mut kernel = SessionKernel::new(
        RecordingNativeAdapter,
        BootstrapProfile::NativeState {
            codec: EngineCodec::LibghosttyCheckpointV2,
            features: EngineFeatureSet::required_native(),
        },
    );
    let mut effects = EffectBuffer::new();
    kernel
        .update(
            KernelInput::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::NativeState {
                    codec: EngineCodec::LibghosttyCheckpointV2,
                },
                geometry: geometry(),
                base_seq: 10,
            },
            &mut effects,
        )
        .unwrap();
    for (chunk_seq, payload) in [
        (0, b"checkpoint-a".as_slice()),
        (1, b"checkpoint-b".as_slice()),
    ] {
        kernel
            .update(
                KernelInput::BootstrapChunk {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    chunk_seq,
                    payload,
                },
                &mut effects,
            )
            .unwrap();
    }
    kernel
        .update(
            KernelInput::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: Some(b"cursor-0"),
            },
            &mut effects,
        )
        .unwrap();
    kernel
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
        let terminal_id = terminal(83);
        let stream_id = stream(84);
        let bootstrap_id = bootstrap(85);
        let mut kernel = published_recording_native(&terminal_id, stream_id, bootstrap_id);
        let mut effects = EffectBuffer::new();
        let result = kernel.update(
            KernelInput::HistoryPage {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                cursor: b"cursor-0",
                next_cursor: has_more.then_some(b"cursor-1".as_slice()),
                payload,
                page_seq: 1,
                rows: 0,
            },
            &mut effects,
        );
        assert!(matches!(
            result,
            Err(KernelError::HistoryCompletionMismatch {
                progress,
                has_more: actual_has_more,
            }) if progress == expected_progress && actual_has_more == has_more
        ));
        assert!(
            kernel.published(&terminal_id).is_some(),
            "history mismatch retains the last renderable replica"
        );
        assert!(
            kernel
                .tombstone(&terminal_id, stream_id, bootstrap_id)
                .is_none()
        );
        assert!(matches!(
            kernel.input_eligibility(&terminal_id),
            InputEligibility::Eligible { .. }
        ));
        assert!(effects.as_slice().iter().any(|effect| matches!(
            effect,
            KernelEffect::Status(KernelStatus::History { status, .. })
                if status.state == HistoryLoadState::Tombstoned
        )));
        assert!(effects.as_slice().iter().all(|effect| !matches!(
            effect,
            KernelEffect::Status(KernelStatus::ResyncRequired { .. })
        )));
    }
}

#[test]
fn engine_effects_are_drained_after_apply_in_order() {
    let terminal_id = terminal(8);
    let stream_id = stream(17);
    let bootstrap_id = bootstrap(28);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    publish_direct(
        &mut kernel,
        &terminal_id,
        stream_id,
        bootstrap_id,
        0,
        &mut effects,
    );
    let published_key = kernel.published(&terminal_id).unwrap().key().clone();

    kernel
        .update(
            KernelInput::TerminalOutput {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                seq: 1,
                payload: b"effects",
            },
            &mut effects,
        )
        .unwrap();
    assert_eq!(
        effects.as_slice(),
        &[
            KernelEffect::Send(KernelSend::PtyWrite {
                terminal_id: terminal_id.clone(),
                bytes: b"reply".to_vec(),
            }),
            KernelEffect::Damage(KernelDamage {
                terminal_id: terminal_id.clone(),
                kind: KernelDamageKind::Rows { first: 2, last: 4 },
            }),
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

    kernel
        .update(
            KernelInput::TerminalOutput {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                seq: 2,
                payload: b"plain",
            },
            &mut effects,
        )
        .unwrap();
    assert_eq!(effects.len(), 1, "prior engine effects were fully drained");
}

#[test]
fn input_gate_requires_published_replica_and_attach_ready() {
    let terminal_id = terminal(9);
    let stream_id = stream(18);
    let bootstrap_id = bootstrap(29);
    let event = InputEvent::Focus(FocusEvent::Gained);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    kernel
        .update(
            KernelInput::AttachStarted {
                attach_id: 9,
                terminals: std::slice::from_ref(&terminal_id),
            },
            &mut effects,
        )
        .unwrap();

    assert_eq!(
        kernel.input_eligibility(&terminal_id),
        InputEligibility::Ineligible(InputBlockReason::AwaitingReplica)
    );
    let before_replica = kernel.update(
        KernelInput::Action(KernelAction::Input {
            terminal_id: &terminal_id,
            event: &event,
        }),
        &mut effects,
    );
    assert!(matches!(
        before_replica,
        Err(KernelError::InputIneligible {
            reason: InputBlockReason::AwaitingReplica,
            ..
        })
    ));

    publish_direct(
        &mut kernel,
        &terminal_id,
        stream_id,
        bootstrap_id,
        0,
        &mut effects,
    );
    assert_eq!(
        kernel.input_eligibility(&terminal_id),
        InputEligibility::Ineligible(InputBlockReason::AwaitingAttachReady)
    );
    let before_barrier = kernel.update(
        KernelInput::Action(KernelAction::Input {
            terminal_id: &terminal_id,
            event: &event,
        }),
        &mut effects,
    );
    assert!(matches!(
        before_barrier,
        Err(KernelError::InputIneligible {
            reason: InputBlockReason::AwaitingAttachReady,
            ..
        })
    ));

    kernel
        .update(KernelInput::AttachReady { attach_id: 9 }, &mut effects)
        .unwrap();
    assert_eq!(
        kernel.input_eligibility(&terminal_id),
        InputEligibility::Eligible {
            stream_id,
            bootstrap_id,
        }
    );
    kernel
        .update(
            KernelInput::Action(KernelAction::Input {
                terminal_id: &terminal_id,
                event: &event,
            }),
            &mut effects,
        )
        .unwrap();
    assert_eq!(
        effects.as_slice(),
        &[KernelEffect::Send(KernelSend::Input {
            terminal_id: terminal_id.clone(),
            event,
        })]
    );
}

#[test]
fn effect_buffer_reuses_high_water_capacity() {
    let terminal_id = terminal(10);
    let stream_id = stream(19);
    let bootstrap_id = bootstrap(30);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::with_capacity(4);
    let initial_capacity = effects.capacity();
    publish_direct(
        &mut kernel,
        &terminal_id,
        stream_id,
        bootstrap_id,
        0,
        &mut effects,
    );
    kernel
        .update(
            KernelInput::TerminalOutput {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                seq: 1,
                payload: b"effects",
            },
            &mut effects,
        )
        .unwrap();
    assert_eq!(effects.len(), 4);
    assert_eq!(effects.capacity(), initial_capacity);
    let drained = effects.take();
    assert!(effects.is_empty());
    effects.restore_allocation(drained);
    assert!(effects.is_empty());
    assert_eq!(effects.capacity(), initial_capacity);

    kernel
        .update(
            KernelInput::TerminalOutput {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                seq: 2,
                payload: b"plain",
            },
            &mut effects,
        )
        .unwrap();
    assert_eq!(effects.len(), 1);
    assert_eq!(effects.capacity(), initial_capacity);
    effects.clear();
    assert_eq!(effects.capacity(), initial_capacity);
}

#[test]
fn state_sync_ack_is_generation_bound_and_raw_has_no_ack() {
    let terminal_id = terminal(11);
    let stream_id = stream(20);
    let bootstrap_id = bootstrap(31);
    let mut state_sync_kernel = kernel_with_profile(
        ReadyMode::ChunkFirst,
        BootstrapProfile::SynthesizedVtStateSync,
    );
    let mut effects = EffectBuffer::new();
    state_sync_kernel
        .update(
            KernelInput::BootstrapBegin {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::SynthesizedVtStateSync,
                geometry: geometry(),
                base_seq: 0,
            },
            &mut effects,
        )
        .unwrap();
    push_ready_transcript(
        &mut state_sync_kernel,
        &terminal_id,
        stream_id,
        bootstrap_id,
        &mut effects,
    );
    protocol_ready(
        &mut state_sync_kernel,
        &terminal_id,
        stream_id,
        bootstrap_id,
        &mut effects,
    );

    state_sync_kernel
        .update(
            KernelInput::TerminalOutput {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                seq: 1,
                payload: b"state-sync",
            },
            &mut effects,
        )
        .unwrap();
    assert_eq!(
        effects.as_slice(),
        &[
            KernelEffect::Damage(KernelDamage {
                terminal_id: terminal_id.clone(),
                kind: KernelDamageKind::Full,
            }),
            KernelEffect::Send(KernelSend::FrameAck {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq: 1,
            }),
        ]
    );

    let raw_terminal = terminal(12);
    let raw_stream = stream(21);
    let raw_bootstrap = bootstrap(32);
    let mut raw_kernel = kernel(ReadyMode::ChunkFirst);
    publish_direct(
        &mut raw_kernel,
        &raw_terminal,
        raw_stream,
        raw_bootstrap,
        0,
        &mut effects,
    );
    raw_kernel
        .update(
            KernelInput::TerminalOutput {
                terminal_id: &raw_terminal,
                stream_id: raw_stream,
                bootstrap_id: raw_bootstrap,
                seq: 1,
                payload: b"raw",
            },
            &mut effects,
        )
        .unwrap();
    assert!(
        effects
            .as_slice()
            .iter()
            .all(|effect| !matches!(effect, KernelEffect::Send(KernelSend::FrameAck { .. })))
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn mutating_adapter_errors_retire_staging_and_published_replicas() {
    let mut kernel = kernel(ReadyMode::ProtocolFirst);
    let mut effects = EffectBuffer::new();

    let chunk_terminal = terminal(13);
    let chunk_stream = stream(22);
    let chunk_bootstrap = bootstrap(33);
    begin(
        &mut kernel,
        &chunk_terminal,
        chunk_stream,
        chunk_bootstrap,
        0,
        &mut effects,
    );
    let chunk_error = kernel.update(
        KernelInput::BootstrapChunk {
            terminal_id: &chunk_terminal,
            stream_id: chunk_stream,
            bootstrap_id: chunk_bootstrap,
            chunk_seq: 0,
            payload: b"mutate-then-error",
        },
        &mut effects,
    );
    assert!(matches!(
        chunk_error,
        Err(KernelError::Engine(FakeError::MutatedThenFailed))
    ));
    assert!(kernel.staging(&chunk_terminal).is_none());
    assert!(
        kernel
            .tombstone(&chunk_terminal, chunk_stream, chunk_bootstrap)
            .is_some()
    );
    assert_eq!(
        effects.as_slice(),
        &[KernelEffect::Status(KernelStatus::ResyncRequired {
            terminal_id: chunk_terminal.clone(),
            stream_id: chunk_stream,
            bootstrap_id: chunk_bootstrap,
            reason: TombstoneReason::CodecFailure,
        })]
    );
    let chunk_retry = kernel.update(
        KernelInput::BootstrapChunk {
            terminal_id: &chunk_terminal,
            stream_id: chunk_stream,
            bootstrap_id: chunk_bootstrap,
            chunk_seq: 0,
            payload: b"retry",
        },
        &mut effects,
    );
    assert!(matches!(
        chunk_retry,
        Err(KernelError::RetiredGeneration { .. })
    ));

    let finish_terminal = terminal(14);
    let finish_stream = stream(23);
    let finish_bootstrap = bootstrap(34);
    begin(
        &mut kernel,
        &finish_terminal,
        finish_stream,
        finish_bootstrap,
        0,
        &mut effects,
    );
    kernel
        .update(
            KernelInput::BootstrapChunk {
                terminal_id: &finish_terminal,
                stream_id: finish_stream,
                bootstrap_id: finish_bootstrap,
                chunk_seq: 0,
                payload: b"<FINISH_ERROR>",
            },
            &mut effects,
        )
        .unwrap();
    let finish_error = kernel.update(
        KernelInput::BootstrapReady {
            terminal_id: &finish_terminal,
            stream_id: finish_stream,
            bootstrap_id: finish_bootstrap,
            history_cursor: None,
        },
        &mut effects,
    );
    assert!(matches!(
        finish_error,
        Err(KernelError::Engine(FakeError::MutatedThenFailed))
    ));
    assert!(kernel.staging(&finish_terminal).is_none());
    assert!(
        kernel
            .tombstone(&finish_terminal, finish_stream, finish_bootstrap)
            .is_some()
    );
    let finish_retry = kernel.update(
        KernelInput::BootstrapReady {
            terminal_id: &finish_terminal,
            stream_id: finish_stream,
            bootstrap_id: finish_bootstrap,
            history_cursor: None,
        },
        &mut effects,
    );
    assert!(matches!(
        finish_retry,
        Err(KernelError::RetiredGeneration { .. })
    ));

    let live_terminal = terminal(15);
    let live_stream = stream(24);
    let live_bootstrap = bootstrap(35);
    publish_direct(
        &mut kernel,
        &live_terminal,
        live_stream,
        live_bootstrap,
        0,
        &mut effects,
    );
    let live_error = kernel.update(
        KernelInput::TerminalOutput {
            terminal_id: &live_terminal,
            stream_id: live_stream,
            bootstrap_id: live_bootstrap,
            seq: 1,
            payload: b"mutate-then-error",
        },
        &mut effects,
    );
    assert!(matches!(
        live_error,
        Err(KernelError::Engine(FakeError::MutatedThenFailed))
    ));
    assert!(kernel.published(&live_terminal).is_none());
    assert!(
        kernel
            .tombstone(&live_terminal, live_stream, live_bootstrap)
            .is_some()
    );
    assert_eq!(
        effects.as_slice(),
        &[
            KernelEffect::Status(KernelStatus::ResyncRequired {
                terminal_id: live_terminal.clone(),
                stream_id: live_stream,
                bootstrap_id: live_bootstrap,
                reason: TombstoneReason::CodecFailure,
            }),
            KernelEffect::Damage(KernelDamage {
                terminal_id: live_terminal.clone(),
                kind: KernelDamageKind::Removed,
            }),
        ]
    );
    let live_retry = kernel.update(
        KernelInput::TerminalOutput {
            terminal_id: &live_terminal,
            stream_id: live_stream,
            bootstrap_id: live_bootstrap,
            seq: 1,
            payload: b"retry",
        },
        &mut effects,
    );
    assert!(matches!(
        live_retry,
        Err(KernelError::RetiredGeneration { .. })
    ));
}

#[test]
fn bootstrap_effects_wait_for_publication_and_suppress_send_and_damage() {
    let terminal_id = terminal(16);
    let stream_id = stream(25);
    let bootstrap_id = bootstrap(36);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    begin(
        &mut kernel,
        &terminal_id,
        stream_id,
        bootstrap_id,
        0,
        &mut effects,
    );
    kernel
        .update(
            KernelInput::BootstrapChunk {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: b"bootstrap-effects",
            },
            &mut effects,
        )
        .unwrap();
    assert!(effects.is_empty());

    kernel
        .update(
            KernelInput::BootstrapChunk {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq: 1,
                payload: READY_MARKER,
            },
            &mut effects,
        )
        .unwrap();
    assert!(effects.is_empty());
    protocol_ready(
        &mut kernel,
        &terminal_id,
        stream_id,
        bootstrap_id,
        &mut effects,
    );
    let published_key = kernel.published(&terminal_id).unwrap().key().clone();
    assert_eq!(
        effects.as_slice(),
        &[
            KernelEffect::Status(KernelStatus::Engine {
                key: published_key.clone(),
                status: EngineStatus::Title("bootstrap-staging".to_owned()),
            }),
            KernelEffect::Job(KernelJob {
                key: published_key.clone(),
                job: EngineJob::Wakeup,
            }),
            KernelEffect::Status(KernelStatus::Engine {
                key: published_key.clone(),
                status: EngineStatus::Title("bootstrap-finished".to_owned()),
            }),
            KernelEffect::Job(KernelJob {
                key: published_key,
                job: EngineJob::Wakeup,
            }),
            KernelEffect::Damage(KernelDamage {
                terminal_id,
                kind: KernelDamageKind::Full,
            }),
        ]
    );
}

#[test]
fn replacement_attach_close_flushes_pending_removal_at_barrier() {
    let terminal_id = terminal(17);
    let stream_id = stream(26);
    let bootstrap_id = bootstrap(37);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    kernel
        .update(
            KernelInput::AttachStarted {
                attach_id: 40,
                terminals: std::slice::from_ref(&terminal_id),
            },
            &mut effects,
        )
        .unwrap();
    publish_direct(
        &mut kernel,
        &terminal_id,
        stream_id,
        bootstrap_id,
        0,
        &mut effects,
    );
    kernel
        .update(KernelInput::AttachReady { attach_id: 40 }, &mut effects)
        .unwrap();

    kernel
        .update(
            KernelInput::AttachStarted {
                attach_id: 41,
                terminals: std::slice::from_ref(&terminal_id),
            },
            &mut effects,
        )
        .unwrap();
    assert!(kernel.published(&terminal_id).is_some());
    kernel
        .update(
            KernelInput::TerminalClosed {
                terminal_id: &terminal_id,
            },
            &mut effects,
        )
        .unwrap();
    assert!(effects.is_empty());
    kernel
        .update(KernelInput::AttachReady { attach_id: 41 }, &mut effects)
        .unwrap();
    assert_eq!(
        effects.as_slice(),
        &[KernelEffect::Damage(KernelDamage {
            terminal_id,
            kind: KernelDamageKind::Removed,
        })]
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn replacement_effects_are_hidden_until_swap_and_discarded_on_retirement() {
    let terminal_id = terminal(18);
    let stream_id = stream(27);
    let published_bootstrap = bootstrap(38);
    let cancelled_bootstrap = bootstrap(39);
    let replacement_bootstrap = bootstrap(40);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    publish_direct(
        &mut kernel,
        &terminal_id,
        stream_id,
        published_bootstrap,
        0,
        &mut effects,
    );
    let published_key = kernel.published(&terminal_id).unwrap().key().clone();

    begin(
        &mut kernel,
        &terminal_id,
        stream_id,
        cancelled_bootstrap,
        10,
        &mut effects,
    );
    kernel
        .update(
            KernelInput::BootstrapChunk {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id: cancelled_bootstrap,
                chunk_seq: 0,
                payload: b"bootstrap-effects",
            },
            &mut effects,
        )
        .unwrap();
    assert!(effects.is_empty());
    assert_eq!(
        kernel.published(&terminal_id).unwrap().key(),
        &published_key
    );

    kernel
        .update(
            KernelInput::Tombstone {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id: cancelled_bootstrap,
                reason: TombstoneReason::CodecFailure,
                last_valid_seq: 10,
            },
            &mut effects,
        )
        .unwrap();
    assert!(effects.is_empty(), "retired staging effects are discarded");
    assert_eq!(
        kernel.published(&terminal_id).unwrap().key(),
        &published_key
    );

    begin(
        &mut kernel,
        &terminal_id,
        stream_id,
        replacement_bootstrap,
        20,
        &mut effects,
    );
    kernel
        .update(
            KernelInput::BootstrapChunk {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id: replacement_bootstrap,
                chunk_seq: 0,
                payload: b"bootstrap-effects",
            },
            &mut effects,
        )
        .unwrap();
    assert!(effects.is_empty());
    kernel
        .update(
            KernelInput::BootstrapChunk {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id: replacement_bootstrap,
                chunk_seq: 1,
                payload: READY_MARKER,
            },
            &mut effects,
        )
        .unwrap();
    assert!(effects.is_empty());
    protocol_ready(
        &mut kernel,
        &terminal_id,
        stream_id,
        replacement_bootstrap,
        &mut effects,
    );
    let replacement_key = kernel.published(&terminal_id).unwrap().key().clone();
    assert_eq!(replacement_key.bootstrap_id, replacement_bootstrap);
    assert_eq!(
        effects.as_slice(),
        &[
            KernelEffect::Status(KernelStatus::Engine {
                key: replacement_key.clone(),
                status: EngineStatus::Title("bootstrap-staging".to_owned()),
            }),
            KernelEffect::Job(KernelJob {
                key: replacement_key.clone(),
                job: EngineJob::Wakeup,
            }),
            KernelEffect::Status(KernelStatus::Engine {
                key: replacement_key.clone(),
                status: EngineStatus::Title("bootstrap-finished".to_owned()),
            }),
            KernelEffect::Job(KernelJob {
                key: replacement_key,
                job: EngineJob::Wakeup,
            }),
            KernelEffect::Damage(KernelDamage {
                terminal_id,
                kind: KernelDamageKind::Full,
            }),
        ]
    );
}

/// phux-994s: the walk-identity token a frontend hands its render pool packs
/// the kernel's own generation identity `(stream_id, bootstrap_id)`
/// losslessly, so distinct generations always yield distinct tokens and a
/// republish on the same stream changes the token.
#[test]
fn generation_token_is_lossless_and_changes_per_generation() {
    let key = |stream: u64, bootstrap: u64| super::ReplicaKey {
        terminal_id: TerminalId::local(1),
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

/// phux-l96p.10: the gap resync's replacement generation is adopted whole.
///
/// After a broadcast gap the server republishes the pane as a *new* bootstrap
/// generation whose `base_seq` is the actor's current raw sequence — far
/// beyond anything the old generation reached, because the whole point is that
/// the frames in between were dropped. The kernel must take that base as the
/// new origin and accept `base + 1` as the very next live frame; treating the
/// jump as a `SequenceGap` is what turned a recoverable gap into
/// `session kernel rejected terminal_output: live sequence gap at N;
/// expected M` and a dead pane.
///
/// Both server shapes are covered, because the two bootstrap profiles differ:
/// the native path tombstones the old generation first, the synthesized path
/// republishes without one. In both, output still addressed to the old
/// generation must be *ignored* (`RetiredGeneration`), never a sequence error.
#[test]
fn gap_resync_replacement_generation_resets_the_live_sequence() {
    for tombstone_first in [true, false] {
        let terminal_id = terminal(7);
        let stream_id = stream(17);
        let old = bootstrap(27);
        let replacement = bootstrap(28);
        let mut kernel = kernel(ReadyMode::ChunkFirst);
        let mut effects = EffectBuffer::new();

        publish_direct(&mut kernel, &terminal_id, stream_id, old, 41, &mut effects);
        kernel
            .update(
                KernelInput::TerminalOutput {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id: old,
                    seq: 42,
                    payload: b"before-the-gap",
                },
                &mut effects,
            )
            .unwrap();

        if tombstone_first {
            kernel
                .update(
                    KernelInput::Tombstone {
                        terminal_id: &terminal_id,
                        stream_id,
                        bootstrap_id: old,
                        reason: TombstoneReason::OutboundGap,
                        last_valid_seq: 42,
                    },
                    &mut effects,
                )
                .unwrap();
        }

        // The dropped window: the actor is thousands of sequences further on.
        let resync_base = 9_000;
        publish_direct(
            &mut kernel,
            &terminal_id,
            stream_id,
            replacement,
            resync_base,
            &mut effects,
        );
        assert_eq!(
            kernel.published(&terminal_id).unwrap().key().bootstrap_id,
            replacement,
            "tombstone_first={tombstone_first}: replacement must become the published generation",
        );
        assert_eq!(
            kernel.published(&terminal_id).unwrap().last_seq(),
            resync_base,
            "tombstone_first={tombstone_first}: the replacement's base_seq is the new origin",
        );

        kernel
            .update(
                KernelInput::TerminalOutput {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id: replacement,
                    seq: resync_base + 1,
                    payload: b"after-the-gap",
                },
                &mut effects,
            )
            .unwrap_or_else(|err| {
                panic!("tombstone_first={tombstone_first}: first frame after the resync was rejected: {err}")
            });
        assert_eq!(
            kernel.published(&terminal_id).unwrap().last_seq(),
            resync_base + 1
        );

        // A frame the pump had already queued against the old generation is
        // ignorable, not fatal: `engine_route` folds `RetiredGeneration` into
        // "ignored", and any other error would detach the client.
        let stale = kernel.update(
            KernelInput::TerminalOutput {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id: old,
                seq: 43,
                payload: b"stale",
            },
            &mut effects,
        );
        assert!(
            matches!(stale, Err(KernelError::RetiredGeneration { .. })),
            "tombstone_first={tombstone_first}: stale generation output must be ignored, got {stale:?}",
        );
    }
}

// ---------------------------------------------------------------------------
// AgentSession resources: kind tracking and the typed record log.
// ---------------------------------------------------------------------------

const AGENT_PROFILE: BootstrapStreamProfile = BootstrapStreamProfile::AgentEventsJsonlV1;

fn agent_line(seq: u64, kind: &str, data: &str) -> String {
    format!(
        "{{\"seq\":{seq},\"ts_ms\":{},\"type\":\"{kind}\",\"data\":{data}}}\n",
        seq * 10
    )
}

fn declare_agent(
    kernel: &mut SessionKernel<FakeAdapter>,
    agent: &TerminalId,
    parent: &TerminalId,
    effects: &mut EffectBuffer,
) {
    kernel
        .update(
            KernelInput::AgentSessionDeclared(super::AgentSessionDeclaration {
                terminal_id: agent,
                parent: Some(parent),
                provider: Some("claude"),
                native_id: Some("s-1"),
                state: Some("working"),
            }),
            effects,
        )
        .unwrap();
}

fn agent_begin(
    kernel: &mut SessionKernel<FakeAdapter>,
    agent: &TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    base_seq: u64,
    effects: &mut EffectBuffer,
) -> Result<(), KernelError<FakeError>> {
    kernel.update(
        KernelInput::BootstrapBegin {
            terminal_id: agent,
            stream_id,
            bootstrap_id,
            profile: AGENT_PROFILE,
            // An AgentSession has no grid; the wire encodes that as 0x0.
            geometry: CanonicalGeometry { cols: 0, rows: 0 },
            base_seq,
        },
        effects,
    )
}

fn agent_records_effect(effects: &EffectBuffer) -> Option<(TerminalId, Vec<u64>)> {
    effects.as_slice().iter().find_map(|effect| match effect {
        KernelEffect::AgentRecords {
            terminal_id,
            records,
        } => Some((
            terminal_id.clone(),
            records.iter().map(|record| record.seq).collect(),
        )),
        _ => None,
    })
}

#[test]
#[allow(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    reason = "one stream lifecycle end to end: declare, bootstrap, live, close"
)]
fn agent_session_bootstraps_into_a_typed_log_and_never_a_replica() {
    let parent = terminal(1);
    let agent = terminal(2);
    let stream_id = stream(3);
    let bootstrap_id = bootstrap(4);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    kernel
        .update(
            KernelInput::AttachStarted {
                attach_id: 1,
                terminals: std::slice::from_ref(&parent),
            },
            &mut effects,
        )
        .unwrap();
    declare_agent(&mut kernel, &agent, &parent, &mut effects);
    assert_eq!(
        kernel.resource_kind(&agent),
        Some(ResourceKind::AgentSession)
    );
    assert_eq!(kernel.resource_kind(&parent), Some(ResourceKind::Terminal));
    let view = kernel.agent_session(&agent).expect("declared");
    assert_eq!(view.parent, Some(&parent));
    assert_eq!(view.state.provider.as_deref(), Some("claude"));
    assert_eq!(view.state.status, AgentSessionStatus::Working);
    assert!(!view.published);
    assert_eq!(
        kernel.input_eligibility(&agent),
        InputEligibility::Ineligible(InputBlockReason::NotATerminal)
    );

    // A declared AgentSession is not an attach participant: the barrier
    // releases on the parent alone.
    publish_direct(
        &mut kernel,
        &parent,
        stream(10),
        bootstrap(11),
        0,
        &mut effects,
    );
    kernel
        .update(KernelInput::AttachReady { attach_id: 1 }, &mut effects)
        .unwrap();

    agent_begin(
        &mut kernel,
        &agent,
        stream_id,
        bootstrap_id,
        5,
        &mut effects,
    )
    .unwrap();
    let chunk = format!(
        "{}{}",
        agent_line(
            1,
            "session_start",
            r#"{"provider":"claude","native_id":"s-9"}"#
        ),
        agent_line(2, "ask", r#"{"question":"deploy?"}"#)
    );
    kernel
        .update(
            KernelInput::BootstrapChunk {
                terminal_id: &agent,
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: chunk.as_bytes(),
            },
            &mut effects,
        )
        .unwrap();
    assert!(effects.is_empty(), "staging emits nothing until READY");
    kernel
        .update(
            KernelInput::BootstrapReady {
                terminal_id: &agent,
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
            &mut effects,
        )
        .unwrap();
    assert_eq!(
        agent_records_effect(&effects),
        Some((agent.clone(), vec![1, 2])),
        "publication hands the frontend every retained record"
    );
    assert!(
        !effects
            .as_slice()
            .iter()
            .any(|effect| matches!(effect, KernelEffect::Damage(_))),
        "an agent stream never damages a grid"
    );
    assert!(
        kernel.published(&agent).is_none(),
        "no replica for a record stream"
    );
    let view = kernel.agent_session(&agent).expect("published");
    assert!(view.published);
    assert_eq!(
        view.state.native_id.as_deref(),
        Some("s-9"),
        "records refine identity"
    );
    assert_eq!(view.state.status, AgentSessionStatus::Blocked);
    assert_eq!(view.log.len(), 2);

    // Live output appends and re-derives.
    let live = agent_line(3, "stop", "{}");
    kernel
        .update(
            KernelInput::TerminalOutput {
                terminal_id: &agent,
                stream_id,
                bootstrap_id,
                seq: 6,
                payload: live.as_bytes(),
            },
            &mut effects,
        )
        .unwrap();
    assert_eq!(
        agent_records_effect(&effects),
        Some((agent.clone(), vec![3]))
    );
    let view = kernel.agent_session(&agent).expect("live");
    assert_eq!(view.state.status, AgentSessionStatus::Done);
    assert_eq!(view.log.last().map(|record| record.seq), Some(3));

    // Sequencing stays exact.
    assert!(matches!(
        kernel.update(
            KernelInput::TerminalOutput {
                terminal_id: &agent,
                stream_id,
                bootstrap_id,
                seq: 8,
                payload: live.as_bytes(),
            },
            &mut effects,
        ),
        Err(KernelError::SequenceGap {
            expected: 7,
            actual: 8
        })
    ));

    // Closing the session drops the view; its kind is still known.
    kernel
        .update(
            KernelInput::TerminalClosed {
                terminal_id: &agent,
            },
            &mut effects,
        )
        .unwrap();
    assert!(
        effects.is_empty(),
        "closing a record stream removes no grid"
    );
    assert!(kernel.agent_session(&agent).is_none());
    assert_eq!(
        kernel.resource_kind(&agent),
        Some(ResourceKind::AgentSession)
    );
    assert!(
        kernel.published(&parent).is_some(),
        "the parent is untouched"
    );
}

#[test]
fn agent_profile_skips_the_geometry_check_but_terminals_still_need_one() {
    let agent = terminal(2);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    // No declaration at all: the profile alone fixes the kind.
    agent_begin(
        &mut kernel,
        &agent,
        stream(1),
        bootstrap(1),
        0,
        &mut effects,
    )
    .unwrap();
    assert_eq!(
        kernel.resource_kind(&agent),
        Some(ResourceKind::AgentSession)
    );
    let zero = kernel.update(
        KernelInput::BootstrapBegin {
            terminal_id: &terminal(3),
            stream_id: stream(2),
            bootstrap_id: bootstrap(2),
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            geometry: CanonicalGeometry { cols: 0, rows: 0 },
            base_seq: 0,
        },
        &mut effects,
    );
    assert!(matches!(
        zero,
        Err(KernelError::InvalidGeometry { cols: 0, rows: 0 })
    ));
}

#[test]
fn kind_mismatches_are_rejected_in_both_directions() {
    let parent = terminal(1);
    let agent = terminal(2);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    kernel
        .update(
            KernelInput::AttachStarted {
                attach_id: 1,
                terminals: std::slice::from_ref(&parent),
            },
            &mut effects,
        )
        .unwrap();
    declare_agent(&mut kernel, &agent, &parent, &mut effects);

    // A terminal-profile bootstrap on a declared AgentSession.
    let wrong = kernel.update(
        KernelInput::BootstrapBegin {
            terminal_id: &agent,
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            geometry: geometry(),
            base_seq: 0,
        },
        &mut effects,
    );
    assert!(matches!(
        wrong,
        Err(KernelError::KindMismatch {
            declared: ResourceKind::AgentSession,
            incoming: ResourceKind::Terminal,
            ..
        })
    ));
    // An agent-profile bootstrap on an attach-inventory Terminal.
    let wrong = agent_begin(
        &mut kernel,
        &parent,
        stream(2),
        bootstrap(2),
        0,
        &mut effects,
    );
    assert!(matches!(
        wrong,
        Err(KernelError::KindMismatch {
            declared: ResourceKind::Terminal,
            incoming: ResourceKind::AgentSession,
            ..
        })
    ));
    // Declaring an attach-inventory Terminal as an AgentSession.
    let wrong = kernel.update(
        KernelInput::AgentSessionDeclared(super::AgentSessionDeclaration {
            terminal_id: &parent,
            parent: None,
            provider: None,
            native_id: None,
            state: None,
        }),
        &mut effects,
    );
    assert!(matches!(wrong, Err(KernelError::KindMismatch { .. })));
}

#[test]
fn malformed_agent_records_retire_only_that_generation() {
    let agent = terminal(2);
    let stream_id = stream(3);
    let bootstrap_id = bootstrap(4);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    agent_begin(
        &mut kernel,
        &agent,
        stream_id,
        bootstrap_id,
        0,
        &mut effects,
    )
    .unwrap();
    let bad = kernel.update(
        KernelInput::BootstrapChunk {
            terminal_id: &agent,
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: b"{\"seq\":1,\"ts_ms\":1}\n",
        },
        &mut effects,
    );
    assert!(matches!(bad, Err(KernelError::AgentRecord(_))));
    assert!(effects.as_slice().iter().any(|effect| matches!(
        effect,
        KernelEffect::Status(KernelStatus::ResyncRequired {
            reason: TombstoneReason::CodecFailure,
            ..
        })
    )));
    assert_eq!(
        kernel.tombstone(&agent, stream_id, bootstrap_id),
        None,
        "agent generations retire inside the stream, not the terminal table"
    );
    assert!(matches!(
        kernel.update(
            KernelInput::BootstrapReady {
                terminal_id: &agent,
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
            &mut effects,
        ),
        Err(KernelError::RetiredGeneration { .. })
    ));
    // A replacement generation is accepted and publishes cleanly.
    let next = bootstrap(5);
    agent_begin(&mut kernel, &agent, stream_id, next, 0, &mut effects).unwrap();
    kernel
        .update(
            KernelInput::BootstrapReady {
                terminal_id: &agent,
                stream_id,
                bootstrap_id: next,
                history_cursor: None,
            },
            &mut effects,
        )
        .unwrap();
    assert_eq!(
        agent_records_effect(&effects),
        Some((agent.clone(), vec![]))
    );
    assert!(
        kernel
            .agent_session(&agent)
            .is_some_and(|view| view.published)
    );
}

#[test]
fn a_republished_agent_stream_rebuilds_state_from_its_retained_records() {
    let parent = terminal(1);
    let agent = terminal(2);
    let stream_id = stream(3);
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    declare_agent(&mut kernel, &agent, &parent, &mut effects);
    for (generation, kind) in [(1, "ask"), (2, "stop")] {
        let bootstrap_id = bootstrap(generation);
        agent_begin(
            &mut kernel,
            &agent,
            stream_id,
            bootstrap_id,
            0,
            &mut effects,
        )
        .unwrap();
        let chunk = agent_line(1, kind, "{}");
        kernel
            .update(
                KernelInput::BootstrapChunk {
                    terminal_id: &agent,
                    stream_id,
                    bootstrap_id,
                    chunk_seq: 0,
                    payload: chunk.as_bytes(),
                },
                &mut effects,
            )
            .unwrap();
        kernel
            .update(
                KernelInput::BootstrapReady {
                    terminal_id: &agent,
                    stream_id,
                    bootstrap_id,
                    history_cursor: None,
                },
                &mut effects,
            )
            .unwrap();
    }
    let view = kernel.agent_session(&agent).expect("published");
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
    assert!(
        kernel.tombstone(&agent, stream_id, bootstrap(1)).is_none()
            && kernel
                .update(
                    KernelInput::TerminalOutput {
                        terminal_id: &agent,
                        stream_id,
                        bootstrap_id: bootstrap(1),
                        seq: 1,
                        payload: b"",
                    },
                    &mut effects,
                )
                .is_err(),
        "the replaced generation is retired"
    );
}
