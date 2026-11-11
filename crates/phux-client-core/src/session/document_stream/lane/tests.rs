#![allow(clippy::unwrap_used, reason = "tests")]

use super::super::super::{
    MAX_BOOTSTRAP_STAGING_CHUNKS, MAX_BOOTSTRAP_STAGING_IDLE_MS, ResourceKind,
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
    let expired_at = MAX_BOOTSTRAP_STAGING_IDLE_MS + 1;
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
    lane.tombstone(stream_id, bootstrap_id, TombstoneReason::OutboundGap, 4, 0);
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
    assert!(
        fresh
            .output(stream_id, bootstrap_id, 1, b"x")
            .unwrap_err()
            .is_benign_generation(),
        "a live frame before any bootstrap names no current generation"
    );
}

fn stalled(lane: &mut DocumentLane, now_ms: u64) -> Option<u32> {
    match lane.stalled(now_ms)? {
        KernelStatus::DocumentStalled { attempt, .. } => Some(attempt),
        KernelStatus::DocumentLost { .. } => Some(0),
        other => panic!("not a document stall: {other:?}"),
    }
}

#[test]
fn a_stalled_cut_or_fence_asks_for_a_reattach_with_backoff_then_is_lost() {
    let idle = MAX_BOOTSTRAP_STAGING_IDLE_MS;
    let (stream_id, bootstrap_id) = ids(1);
    let mut lane = begun(1);
    assert!(lane.has_deadline());
    assert_eq!(stalled(&mut lane, idle), None);
    assert_eq!(
        stalled(&mut lane, idle + 1),
        Some(1),
        "a cut stopped arriving"
    );
    assert!(matches!(
        lane.chunk(stream_id, bootstrap_id, 0, b"late", idle + 2),
        Err(KernelError::RetiredGeneration { .. })
    ));
    // Each re-attach that brings no recut waits twice as long, up to four
    // idle windows; the fourth stall gives the document up.
    let mut at = idle + 1;
    for (attempt, window) in [(2, 2 * idle), (3, 4 * idle)] {
        assert_eq!(stalled(&mut lane, at + window), None);
        at += window + 1;
        assert_eq!(stalled(&mut lane, at), Some(attempt));
    }
    assert_eq!(stalled(&mut lane, at + 4 * idle + 1), Some(0), "lost");
    assert!(!lane.has_deadline());
    assert_eq!(stalled(&mut lane, 99 * idle), None);
}

#[test]
fn a_delivered_document_resets_the_stall_count() {
    let idle = MAX_BOOTSTRAP_STAGING_IDLE_MS;
    let (stream_id, bootstrap_id) = ids(1);
    let mut lane = begun(1);
    lane.ready(stream_id, bootstrap_id, 0).unwrap();
    assert!(!lane.has_deadline());
    lane.tombstone(stream_id, bootstrap_id, TombstoneReason::OutboundGap, 4, 5);
    assert_eq!(stalled(&mut lane, 5 + idle), None);
    assert_eq!(
        stalled(&mut lane, 6 + idle),
        Some(1),
        "a fence no recut followed"
    );
    let (_, recut) = ids(2);
    lane.begin(
        stream_id,
        recut,
        BootstrapStreamProfile::PixelSurfacePngV1,
        6,
        7 + idle,
    )
    .unwrap();
    lane.ready(stream_id, recut, 7 + idle).unwrap();
    assert!(!lane.has_deadline());
    lane.tombstone(stream_id, recut, TombstoneReason::OutboundGap, 6, 8 + idle);
    assert_eq!(stalled(&mut lane, 9 + 2 * idle), Some(1), "counted afresh");
}

#[test]
fn a_first_cut_that_never_comes_is_a_stall() {
    let idle = MAX_BOOTSTRAP_STAGING_IDLE_MS;
    let (stream_id, bootstrap_id) = ids(1);
    // A relay fenced a bootstrap this viewer never saw.
    let mut fenced = DocumentLane::new(ResourceId::local(3));
    fenced.tombstone(stream_id, bootstrap_id, TombstoneReason::OutboundGap, 0, 0);
    assert_eq!(stalled(&mut fenced, idle + 1), Some(1));
    // Or nothing came at all.
    let mut silent = DocumentLane::new(ResourceId::local(3));
    silent.await_first_cut(0);
    assert_eq!(stalled(&mut silent, idle + 1), Some(1));
    let mut answered = DocumentLane::new(ResourceId::local(3));
    answered.await_first_cut(0);
    answered
        .begin(
            stream_id,
            bootstrap_id,
            BootstrapStreamProfile::PixelSurfacePngV1,
            4,
            1,
        )
        .unwrap();
    answered.ready(stream_id, bootstrap_id, 1).unwrap();
    assert!(!answered.has_deadline());
}

#[test]
fn a_run_of_frames_off_the_current_generation_is_a_stall() {
    let (stream_id, bootstrap_id) = ids(1);
    let (_, other) = ids(9);
    let mut lane = begun(1);
    lane.ready(stream_id, bootstrap_id, 0).unwrap();
    for seq in 0..MAX_BENIGN_DROPS - 1 {
        assert!(
            lane.output(stream_id, other, u64::from(seq) + 5, b"x")
                .is_err()
        );
    }
    assert_eq!(stalled(&mut lane, 0), None);
    lane.output(stream_id, bootstrap_id, 5, b"ok").unwrap();
    for seq in 0..MAX_BENIGN_DROPS {
        assert!(
            lane.output(stream_id, other, u64::from(seq) + 6, b"x")
                .is_err()
        );
    }
    assert!(lane.has_deadline());
    assert_eq!(stalled(&mut lane, 0), Some(1));
}
