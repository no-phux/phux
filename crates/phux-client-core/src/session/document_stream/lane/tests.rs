#![allow(clippy::unwrap_used, reason = "tests")]

use super::super::super::{
    MAX_BOOTSTRAP_STAGING_CHUNKS, MAX_BOOTSTRAP_STAGING_LIFETIME_MS, ResourceKind,
};
use super::*;

fn ids(bootstrap: u64) -> (StreamId, BootstrapId) {
    (
        StreamId::new(1).unwrap(),
        BootstrapId::new(bootstrap).unwrap(),
    )
}

fn begun(bootstrap: u64) -> DocumentLane {
    let mut lane = DocumentLane::new(ResourceId::local(3));
    let (stream_id, bootstrap_id) = ids(bootstrap);
    lane.begin(
        stream_id,
        bootstrap_id,
        BootstrapStreamProfile::PixelSurfacePngV1,
        4,
        0,
    )
    .unwrap();
    lane
}

#[test]
fn a_cut_lands_whole_and_live_documents_only_move_forward() {
    let mut lane = begun(1);
    let (stream_id, bootstrap_id) = ids(1);
    lane.chunk(stream_id, bootstrap_id, 0, b"ab", 0).unwrap();
    lane.chunk(stream_id, bootstrap_id, 1, b"c", 0).unwrap();
    let cut = lane.ready(stream_id, bootstrap_id, 0).unwrap();
    assert_eq!(
        (cut.kind, cut.seq, cut.bytes.as_slice()),
        (ResourceKind::PixelSurface, 4, b"abc" as &[u8])
    );
    assert!(matches!(
        lane.output(stream_id, bootstrap_id, 4, b"old"),
        Err(KernelError::DuplicateSequence {
            expected: 5,
            actual: 4
        })
    ));
    assert_eq!(
        lane.output(stream_id, bootstrap_id, 9, b"new").unwrap().seq,
        9
    );
}

#[test]
fn chunks_must_name_the_cut_and_arrive_in_order() {
    let mut lane = begun(1);
    let (stream_id, bootstrap_id) = ids(1);
    let (_, other) = ids(2);
    assert!(matches!(
        lane.chunk(stream_id, other, 0, b"x", 0),
        Err(KernelError::GenerationMismatch { .. })
    ));
    assert!(matches!(
        lane.chunk(stream_id, bootstrap_id, 1, b"x", 0),
        Err(KernelError::ChunkGap {
            expected: 0,
            actual: 1
        })
    ));
    assert!(matches!(
        lane.ready(stream_id, bootstrap_id, 0),
        Err(KernelError::RetiredGeneration { .. })
    ));
}

#[test]
fn staging_is_bounded_by_the_kernel_ceilings_and_lifetime() {
    let mut lane = begun(1);
    let (stream_id, bootstrap_id) = ids(1);
    let ceiling = u32::try_from(MAX_BOOTSTRAP_STAGING_CHUNKS).unwrap();
    for chunk_seq in 0..ceiling {
        lane.chunk(stream_id, bootstrap_id, chunk_seq, b"", 0)
            .unwrap();
    }
    assert!(matches!(
        lane.chunk(stream_id, bootstrap_id, ceiling, b"", 0),
        Err(KernelError::BootstrapStagingLimitExceeded)
    ));

    let mut lane = begun(1);
    let expired_at = MAX_BOOTSTRAP_STAGING_LIFETIME_MS + 1;
    assert!(matches!(
        lane.chunk(stream_id, bootstrap_id, 0, b"x", expired_at),
        Err(KernelError::BootstrapStagingLimitExceeded)
    ));
    let mut lane = begun(1);
    assert!(matches!(
        lane.ready(stream_id, bootstrap_id, expired_at),
        Err(KernelError::BootstrapStagingLimitExceeded)
    ));
}

#[test]
fn a_tombstone_fences_the_generation_until_its_recut() {
    let mut lane = begun(1);
    let (stream_id, bootstrap_id) = ids(1);
    lane.ready(stream_id, bootstrap_id, 0).unwrap();
    lane.tombstone(stream_id, bootstrap_id, TombstoneReason::OutboundGap, 4);
    assert!(matches!(
        lane.output(stream_id, bootstrap_id, 5, b"late"),
        Err(KernelError::RetiredGeneration { .. })
    ));
    let (_, recut) = ids(2);
    lane.begin(
        stream_id,
        recut,
        BootstrapStreamProfile::PixelSurfacePngV1,
        6,
        0,
    )
    .unwrap();
    assert_eq!(lane.ready(stream_id, recut, 0).unwrap().seq, 6);
}

#[test]
fn only_one_document_kind_is_admitted() {
    let mut lane = begun(1);
    let (stream_id, _) = ids(2);
    let (_, bootstrap_id) = ids(2);
    assert!(matches!(
        lane.begin(
            stream_id,
            bootstrap_id,
            BootstrapStreamProfile::AccessibilityTreeJsonV1,
            0,
            0
        ),
        Err(KernelError::KindMismatch { .. })
    ));
    let mut fresh = DocumentLane::new(ResourceId::local(3));
    assert!(matches!(
        fresh.begin(
            stream_id,
            bootstrap_id,
            BootstrapStreamProfile::SynthesizedVtRaw,
            0,
            0
        ),
        Err(KernelError::UnsupportedKind {
            kind: ResourceKind::Terminal,
            ..
        })
    ));
    assert!(matches!(
        fresh.begin(
            stream_id,
            bootstrap_id,
            BootstrapStreamProfile::Unknown { codec_tag: 0x77 },
            0,
            0
        ),
        Err(KernelError::UnknownCodec {
            codec_tag: 0x77,
            ..
        })
    ));
    assert!(matches!(
        fresh.output(stream_id, bootstrap_id, 1, b"x"),
        Err(KernelError::UnknownTerminal(_))
    ));
}
