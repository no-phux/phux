//! The document lane keeps the agent lane's rails: shared staging ceilings,
//! expiry, generation identity, contiguous chunks, closure, and incarnation.

use phux_protocol::wire::frame::TombstoneReason;
use phux_protocol::{BootstrapId, BootstrapProfile, BootstrapStreamProfile, ResourceId, StreamId};

use super::super::kernel_rig::RigAdapter;
use super::super::test_rig::{Gen, closed};
use super::super::{
    EffectBuffer, KernelEffect, KernelError, KernelStatus, MAX_BOOTSTRAP_STAGING_BYTES,
    MAX_BOOTSTRAP_STAGING_CHUNKS, MAX_BOOTSTRAP_STAGING_IDLE_MS, SessionKernel,
};
use super::{documents, generation, stream, surface};

type Kernel = SessionKernel<RigAdapter>;
type Outcome = Result<(), KernelError<std::convert::Infallible>>;

fn kernel() -> Kernel {
    SessionKernel::new(RigAdapter::default(), BootstrapProfile::SynthesizedVtRaw)
}

fn on(id: &ResourceId, ids: (StreamId, BootstrapId)) -> Gen {
    Gen {
        id: id.clone(),
        s: ids.0,
        b: ids.1,
    }
}

fn begin_profile(
    kernel: &mut Kernel,
    id: &ResourceId,
    profile: BootstrapStreamProfile,
    ids: (StreamId, BootstrapId),
    effects: &mut EffectBuffer,
) -> Outcome {
    let g = on(id, ids);
    let input = if profile == BootstrapStreamProfile::SynthesizedVtRaw {
        g.begin(4)
    } else {
        g.begin_gridless(profile, 4)
    };
    kernel.update_at(0, input, effects)
}

fn begin(kernel: &mut Kernel, ids: (StreamId, BootstrapId), effects: &mut EffectBuffer) -> Outcome {
    let png = BootstrapStreamProfile::PixelSurfacePngV1;
    begin_profile(kernel, &surface(), png, ids, effects)
}

fn chunk(
    kernel: &mut Kernel,
    id: &ResourceId,
    ids: (StreamId, BootstrapId),
    chunk_seq: u32,
    payload: &[u8],
    effects: &mut EffectBuffer,
) -> Outcome {
    kernel.update_at(0, on(id, ids).chunk(chunk_seq, payload), effects)
}

fn ready(
    kernel: &mut Kernel,
    now_ms: u64,
    ids: (StreamId, BootstrapId),
    effects: &mut EffectBuffer,
) -> Outcome {
    kernel.update_at(now_ms, on(&surface(), ids).ready(None), effects)
}

fn output(
    kernel: &mut Kernel,
    ids: (StreamId, BootstrapId),
    seq: u64,
    effects: &mut EffectBuffer,
) -> Outcome {
    kernel.update(on(&surface(), ids).output(seq, b"frame"), effects)
}

fn first() -> (StreamId, BootstrapId) {
    (stream(), generation(1))
}

fn resync_required(effects: &EffectBuffer) -> bool {
    effects.as_slice().iter().any(|effect| {
        matches!(
            effect,
            KernelEffect::Status(KernelStatus::ResyncRequired {
                reason: TombstoneReason::CodecFailure,
                ..
            })
        )
    })
}

fn fill_chunk_ceiling(kernel: &mut Kernel, effects: &mut EffectBuffer) {
    for chunk_seq in 0..u32::try_from(MAX_BOOTSTRAP_STAGING_CHUNKS).unwrap() {
        chunk(kernel, &surface(), first(), chunk_seq, b"", effects).unwrap();
    }
}

#[test]
fn document_staging_hits_the_chunk_ceiling_and_is_retired() {
    let mut kernel = kernel();
    let mut effects = EffectBuffer::new();
    begin(&mut kernel, first(), &mut effects).unwrap();
    fill_chunk_ceiling(&mut kernel, &mut effects);
    let over = u32::try_from(MAX_BOOTSTRAP_STAGING_CHUNKS).unwrap();
    assert!(matches!(
        chunk(&mut kernel, &surface(), first(), over, b"", &mut effects),
        Err(KernelError::BootstrapStagingLimitExceeded)
    ));
    assert!(resync_required(&effects));
    assert!(matches!(
        ready(&mut kernel, 0, first(), &mut effects),
        Err(KernelError::RetiredGeneration { .. })
    ));
    assert!(documents(&effects).is_empty());
}

#[test]
fn document_and_terminal_staging_share_the_connection_ceilings() {
    // A terminal holding the whole byte budget leaves none for a document.
    let mut kernel = kernel();
    let mut effects = EffectBuffer::new();
    let terminal = ResourceId::local(1);
    let raw = BootstrapStreamProfile::SynthesizedVtRaw;
    begin_profile(&mut kernel, &terminal, raw, first(), &mut effects).unwrap();
    kernel
        .lanes
        .terminal_mut(&terminal)
        .and_then(|state| state.staging.as_mut())
        .unwrap()
        .accepted_bytes = MAX_BOOTSTRAP_STAGING_BYTES;
    begin(&mut kernel, first(), &mut effects).unwrap();
    assert!(matches!(
        chunk(&mut kernel, &surface(), first(), 0, b"x", &mut effects),
        Err(KernelError::BootstrapStagingLimitExceeded)
    ));
    assert!(kernel.staging(&terminal).is_some());

    // A document holding every chunk leaves none for a terminal.
    let mut kernel = self::kernel();
    begin(&mut kernel, first(), &mut effects).unwrap();
    fill_chunk_ceiling(&mut kernel, &mut effects);
    begin_profile(&mut kernel, &terminal, raw, first(), &mut effects).unwrap();
    assert!(matches!(
        chunk(&mut kernel, &terminal, first(), 0, b"x", &mut effects),
        Err(KernelError::BootstrapStagingLimitExceeded)
    ));
    assert!(kernel.staging(&terminal).is_none());
}

#[test]
fn silent_document_staging_expires_at_ready() {
    let mut effects = EffectBuffer::new();
    let lifetime = MAX_BOOTSTRAP_STAGING_IDLE_MS;
    let mut kernel = kernel();
    begin(&mut kernel, first(), &mut effects).unwrap();
    assert!(matches!(
        ready(&mut kernel, lifetime + 1, first(), &mut effects),
        Err(KernelError::BootstrapStagingLimitExceeded)
    ));
    assert!(resync_required(&effects));
    assert!(documents(&effects).is_empty());
}

#[test]
fn document_chunks_must_name_the_staging_generation_in_order() {
    let mut kernel = kernel();
    let mut effects = EffectBuffer::new();
    let id = surface();
    begin(&mut kernel, first(), &mut effects).unwrap();
    let other_stream = StreamId::new(8).unwrap();
    for ids in [(other_stream, generation(1)), (stream(), generation(2))] {
        assert!(matches!(
            chunk(&mut kernel, &id, ids, 0, b"x", &mut effects),
            Err(KernelError::GenerationMismatch { .. })
        ));
    }
    assert!(matches!(
        begin(&mut kernel, first(), &mut effects),
        Err(KernelError::DuplicateGeneration { .. })
    ));
    chunk(&mut kernel, &id, first(), 0, b"a", &mut effects).unwrap();
    assert!(matches!(
        chunk(&mut kernel, &id, first(), 0, b"a", &mut effects),
        Err(KernelError::DuplicateChunk {
            expected: 1,
            actual: 0
        })
    ));
    assert!(resync_required(&effects), "a broken cut is fenced");

    begin(&mut kernel, (stream(), generation(2)), &mut effects).unwrap();
    assert!(matches!(
        chunk(
            &mut kernel,
            &id,
            (stream(), generation(2)),
            1,
            b"b",
            &mut effects
        ),
        Err(KernelError::ChunkGap {
            expected: 0,
            actual: 1
        })
    ));
    assert!(matches!(
        ready(&mut kernel, 0, (stream(), generation(2)), &mut effects),
        Err(KernelError::RetiredGeneration { .. })
    ));
}

#[test]
fn live_documents_name_the_published_generation_and_move_forward() {
    let mut kernel = kernel();
    let mut effects = EffectBuffer::new();
    begin(&mut kernel, first(), &mut effects).unwrap();
    ready(&mut kernel, 0, first(), &mut effects).unwrap();
    assert!(matches!(
        output(&mut kernel, (stream(), generation(2)), 5, &mut effects),
        Err(KernelError::GenerationMismatch { .. })
    ));
    assert!(matches!(
        output(&mut kernel, first(), 4, &mut effects),
        Err(KernelError::DuplicateSequence {
            expected: 5,
            actual: 4
        })
    ));
    output(&mut kernel, first(), 7, &mut effects).unwrap();
    assert_eq!(
        documents(&effects)[0].seq,
        7,
        "a skipped document loses nothing"
    );

    // A recut fences the old generation: its late frames are refused.
    let recut = (stream(), generation(2));
    let fenced = on(&surface(), first());
    let fence = fenced.tombstone(TombstoneReason::OutboundGap, 7);
    kernel.update(fence, &mut effects).unwrap();
    begin(&mut kernel, recut, &mut effects).unwrap();
    ready(&mut kernel, 0, recut, &mut effects).unwrap();
    assert_eq!(documents(&effects).len(), 1);
    assert!(matches!(
        output(&mut kernel, first(), 8, &mut effects),
        Err(KernelError::RetiredGeneration { .. })
    ));
    output(&mut kernel, recut, 5, &mut effects).unwrap();
}

#[test]
fn closing_a_document_forgets_it_and_refuses_later_frames() {
    let mut kernel = kernel();
    let mut effects = EffectBuffer::new();
    let id = surface();
    begin(&mut kernel, first(), &mut effects).unwrap();
    ready(&mut kernel, 0, first(), &mut effects).unwrap();
    kernel.update(closed(&id), &mut effects).unwrap();
    assert!(kernel.lanes.is_empty());
    assert!(matches!(
        output(&mut kernel, first(), 5, &mut effects),
        Err(KernelError::ClosedTerminal(_))
    ));
    assert!(matches!(
        begin(&mut kernel, (stream(), generation(2)), &mut effects),
        Err(KernelError::ClosedTerminal(_))
    ));
}

#[test]
fn a_new_incarnation_may_reuse_an_open_document_id_for_any_kind() {
    let mut kernel = kernel();
    let mut effects = EffectBuffer::new();
    let id = surface();
    begin(&mut kernel, first(), &mut effects).unwrap();
    ready(&mut kernel, 0, first(), &mut effects).unwrap();
    kernel.release_incarnation();
    assert!(kernel.lanes.is_empty());
    begin_profile(
        &mut kernel,
        &id,
        BootstrapStreamProfile::SynthesizedVtRaw,
        first(),
        &mut effects,
    )
    .unwrap();
}

#[test]
fn every_typed_cut_names_its_own_content_type() {
    let mut kernel = kernel();
    let mut effects = EffectBuffer::new();
    let id = surface();
    let typed = BootstrapStreamProfile::DocumentTypedV1;
    let named = phux_protocol::wire::frame::typed_document::encode_bootstrap("text/plain", b"one");
    begin_profile(&mut kernel, &id, typed, first(), &mut effects).unwrap();
    chunk(&mut kernel, &id, first(), 0, &named, &mut effects).unwrap();
    ready(&mut kernel, 0, first(), &mut effects).unwrap();
    assert_eq!(
        documents(&effects)[0].content_type.as_deref(),
        Some("text/plain")
    );

    // A recut that names none does not inherit the earlier cut's.
    let recut = (stream(), generation(2));
    begin_profile(&mut kernel, &id, typed, recut, &mut effects).unwrap();
    assert!(matches!(
        ready(&mut kernel, 0, recut, &mut effects),
        Err(KernelError::UntypedDocument(_))
    ));
    assert!(resync_required(&effects));
    assert!(documents(&effects).is_empty());
}

#[test]
fn only_a_frame_off_the_current_generations_is_benign() {
    let mut kernel = kernel();
    let mut effects = EffectBuffer::new();
    let id = surface();
    begin(&mut kernel, first(), &mut effects).unwrap();
    let duplicate = begin(&mut kernel, first(), &mut effects).unwrap_err();
    assert!(duplicate.is_benign_generation(), "{duplicate}");
    let gap = chunk(&mut kernel, &id, first(), 1, b"gap", &mut effects).unwrap_err();
    assert!(!gap.is_benign_generation(), "{gap}");
    // The broken cut is fenced; a generation never staged was never live.
    for ids in [first(), (stream(), generation(9))] {
        let late = ready(&mut kernel, 0, ids, &mut effects).unwrap_err();
        assert!(late.is_benign_generation(), "{late}");
    }
}
