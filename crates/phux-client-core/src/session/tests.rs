use phux_protocol::input::{InputEvent, focus::FocusEvent};
use phux_protocol::wire::frame::TombstoneReason;
use phux_protocol::{BootstrapId, BootstrapProfile, BootstrapStreamProfile, StreamId, TerminalId};

use super::{
    EffectBuffer, InputBlockReason, InputEligibility, KernelAction, KernelDamage, KernelDamageKind,
    KernelEffect, KernelError, KernelInput, KernelJob, KernelSend, KernelStatus, SessionKernel,
};
use crate::engine::{
    BootstrapProgress, CanonicalGeometry, EngineAdapter, EngineDamage, EngineEffect,
    EngineEffectBuffer, EngineJob, EngineSend, EngineStatus,
};

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
            effects.push(EngineEffect::Status(EngineStatus::Bell));
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

fn terminal(raw: u32) -> TerminalId {
    TerminalId::local(raw)
}

fn stream(raw: u64) -> StreamId {
    match StreamId::new(raw) {
        Some(id) => id,
        None => panic!("test stream id must be non-zero"),
    }
}

fn bootstrap(raw: u64) -> BootstrapId {
    match BootstrapId::new(raw) {
        Some(id) => id,
        None => panic!("test bootstrap id must be non-zero"),
    }
}

fn geometry() -> CanonicalGeometry {
    match CanonicalGeometry::new(80, 24) {
        Some(geometry) => geometry,
        None => panic!("test geometry must be non-empty"),
    }
}

fn kernel(mode: ReadyMode) -> SessionKernel<FakeAdapter> {
    kernel_with_profile(mode, BootstrapProfile::SynthesizedVtRaw)
}

fn kernel_with_profile(
    mode: ReadyMode,
    profile: BootstrapProfile,
) -> SessionKernel<FakeAdapter> {
    SessionKernel::new(FakeAdapter { ready_mode: mode }, profile)
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
    assert!(kernel.published(&terminal_id).is_none());
    assert_eq!(
        kernel
            .tombstone(&terminal_id, stream_id, bootstrap_id)
            .unwrap()
            .last_valid_seq,
        42
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
}

#[test]
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
                terminal_id: terminal_id.clone(),
                status: EngineStatus::Bell,
            }),
            KernelEffect::Job(KernelJob {
                terminal_id: terminal_id.clone(),
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
    let mut kernel = kernel_with_profile(
        ReadyMode::ChunkFirst,
        BootstrapProfile::SynthesizedVtStateSync,
    );
    let mut effects = EffectBuffer::new();
    kernel
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
        &mut kernel,
        &terminal_id,
        stream_id,
        bootstrap_id,
        &mut effects,
    );
    protocol_ready(
        &mut kernel,
        &terminal_id,
        stream_id,
        bootstrap_id,
        &mut effects,
    );

    kernel
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
    assert!(effects
        .as_slice()
        .iter()
        .all(|effect| !matches!(effect, KernelEffect::Send(KernelSend::FrameAck { .. }))));
}

#[test]
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
    assert!(kernel
        .tombstone(&chunk_terminal, chunk_stream, chunk_bootstrap)
        .is_some());
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
        },
        &mut effects,
    );
    assert!(matches!(
        finish_error,
        Err(KernelError::Engine(FakeError::MutatedThenFailed))
    ));
    assert!(kernel.staging(&finish_terminal).is_none());
    assert!(kernel
        .tombstone(&finish_terminal, finish_stream, finish_bootstrap)
        .is_some());
    let finish_retry = kernel.update(
        KernelInput::BootstrapReady {
            terminal_id: &finish_terminal,
            stream_id: finish_stream,
            bootstrap_id: finish_bootstrap,
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
    assert!(kernel
        .tombstone(&live_terminal, live_stream, live_bootstrap)
        .is_some());
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
fn bootstrap_effects_forward_status_and_jobs_but_suppress_send_and_damage() {
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
    assert_eq!(
        effects.as_slice(),
        &[
            KernelEffect::Status(KernelStatus::Engine {
                terminal_id: terminal_id.clone(),
                status: EngineStatus::Bell,
            }),
            KernelEffect::Job(KernelJob {
                terminal_id: terminal_id.clone(),
                job: EngineJob::Wakeup,
            }),
        ]
    );

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
    assert_eq!(
        effects.as_slice(),
        &[
            KernelEffect::Status(KernelStatus::Engine {
                terminal_id: terminal_id.clone(),
                status: EngineStatus::Title("bootstrap-finished".to_owned()),
            }),
            KernelEffect::Job(KernelJob {
                terminal_id: terminal_id.clone(),
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
