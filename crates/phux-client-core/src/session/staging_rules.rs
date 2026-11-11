//! Admission and retirement rules every lane shares: Terminal replicas,
//! `AgentSession` records and documents.

use std::collections::HashMap;

use phux_protocol::ResourceId;
use phux_protocol::wire::frame::TombstoneReason;

use super::{
    EffectBuffer, GenerationId, KernelEffect, KernelError, KernelStatus,
    MAX_BOOTSTRAP_STAGING_BYTES, MAX_BOOTSTRAP_STAGING_CHUNKS, MAX_BOOTSTRAP_STAGING_IDLE_MS,
    MIN_BOOTSTRAP_STAGING_BYTES_PER_SEC, TombstoneRecord, retired_error,
};

/// When one staging generation began and when its last chunk arrived.
///
/// A cut's wire time grows with its size, so no fixed lifetime suits both a
/// small cut and a large one on a slow link. A cut expires instead when no
/// chunk arrived for [`MAX_BOOTSTRAP_STAGING_IDLE_MS`], or when its age
/// passes that window plus the time its staged bytes take at
/// [`MIN_BOOTSTRAP_STAGING_BYTES_PER_SEC`]: a silent or trickling peer
/// still cannot pin staging, and the byte ceiling bounds the longest cut.
#[derive(Debug, Clone, Copy)]
pub(super) struct StagingClock {
    started_at_ms: u64,
    progress_at_ms: u64,
}

impl StagingClock {
    pub(super) const fn start(now_ms: u64) -> Self {
        Self {
            started_at_ms: now_ms,
            progress_at_ms: now_ms,
        }
    }

    /// A chunk was accepted at `now_ms`.
    pub(super) fn progress(&mut self, now_ms: u64) {
        self.progress_at_ms = self.progress_at_ms.max(now_ms);
    }

    /// The first millisecond at which a cut holding `staged_bytes` has
    /// stalled.
    pub(super) fn deadline_ms(self, staged_bytes: usize) -> u64 {
        let transfer_ms = u64::try_from(staged_bytes)
            .unwrap_or(u64::MAX)
            .saturating_mul(1_000)
            / MIN_BOOTSTRAP_STAGING_BYTES_PER_SEC;
        let idle = self
            .progress_at_ms
            .saturating_add(MAX_BOOTSTRAP_STAGING_IDLE_MS);
        let rate = self
            .started_at_ms
            .saturating_add(MAX_BOOTSTRAP_STAGING_IDLE_MS)
            .saturating_add(transfer_ms);
        idle.min(rate).saturating_add(1)
    }

    /// Whether a cut holding `staged_bytes` has stalled by `now_ms`.
    pub(super) fn expired(self, staged_bytes: usize, now_ms: u64) -> bool {
        now_ms >= self.deadline_ms(staged_bytes)
    }
}

/// Whether staged totals, including the chunk being admitted, fit the
/// connection-wide bootstrap ceilings.
pub(super) const fn within_staging_ceilings(bytes: usize, chunks: usize) -> bool {
    bytes <= MAX_BOOTSTRAP_STAGING_BYTES && chunks <= MAX_BOOTSTRAP_STAGING_CHUNKS
}

/// Contiguous `chunk_seq`, then the connection budget and staging clock.
/// `staged_bytes` counts the chunk being admitted: its own transfer is
/// progress.
pub(super) fn chunk_admission<E>(
    next_chunk_seq: Option<u32>,
    chunk_seq: u32,
    clock: StagingClock,
    staged_bytes: usize,
    now_ms: u64,
    usage_allowed: bool,
) -> Result<(), KernelError<E>> {
    match next_chunk_seq {
        None => Err(KernelError::ChunkSequenceExhausted),
        Some(expected) if chunk_seq < expected => Err(KernelError::DuplicateChunk {
            expected,
            actual: chunk_seq,
        }),
        Some(expected) if chunk_seq > expected => Err(KernelError::ChunkGap {
            expected,
            actual: chunk_seq,
        }),
        Some(_) if !usage_allowed || clock.expired(staged_bytes, now_ms) => {
            Err(KernelError::BootstrapStagingLimitExceeded)
        }
        Some(_) => Ok(()),
    }
}

/// Refuse a frame for a generation this lane already retired.
pub(super) fn reject_retired<E>(
    retired: &HashMap<GenerationId, TombstoneRecord>,
    resource_id: &ResourceId,
    generation: GenerationId,
) -> Result<(), KernelError<E>> {
    if retired.contains_key(&generation) {
        Err(retired_error(resource_id, generation))
    } else {
        Ok(())
    }
}

/// Fence a generation a newer one replaced; an earlier fence stands.
pub(super) fn fence_replaced(
    retired: &mut HashMap<GenerationId, TombstoneRecord>,
    generation: GenerationId,
    last_valid_seq: u64,
) {
    retired.entry(generation).or_insert(TombstoneRecord {
        reason: TombstoneReason::ExplicitReattach,
        last_valid_seq,
    });
}

/// Fence `generation` as a codec failure and ask the host to recut it.
pub(super) fn retire_for_codec_failure(
    retired: &mut HashMap<GenerationId, TombstoneRecord>,
    resource_id: &ResourceId,
    generation: GenerationId,
    last_valid_seq: u64,
    effects: &mut EffectBuffer,
) {
    retired.insert(
        generation,
        TombstoneRecord {
            reason: TombstoneReason::CodecFailure,
            last_valid_seq,
        },
    );
    effects.push(KernelEffect::Status(KernelStatus::ResyncRequired {
        terminal_id: resource_id.clone(),
        stream_id: generation.stream_id,
        bootstrap_id: generation.bootstrap_id,
        reason: TombstoneReason::CodecFailure,
    }));
}
