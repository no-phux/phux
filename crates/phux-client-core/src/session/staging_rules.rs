//! Admission and retirement rules shared by the replica-less lanes
//! (`AgentSession` records and documents).

use std::collections::HashMap;

use phux_protocol::ResourceId;
use phux_protocol::wire::frame::TombstoneReason;

use super::{
    EffectBuffer, GenerationId, KernelEffect, KernelError, KernelStatus,
    MAX_BOOTSTRAP_STAGING_LIFETIME_MS, TombstoneRecord,
};

/// Contiguous `chunk_seq`, then the connection budget and staging lifetime.
pub(super) const fn chunk_admission<E>(
    next_chunk_seq: Option<u32>,
    chunk_seq: u32,
    started_at_ms: u64,
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
        Some(_) if !usage_allowed || staging_lifetime_exceeded(started_at_ms, now_ms) => {
            Err(KernelError::BootstrapStagingLimitExceeded)
        }
        Some(_) => Ok(()),
    }
}

pub(super) const fn staging_lifetime_exceeded(started_at_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(started_at_ms) > MAX_BOOTSTRAP_STAGING_LIFETIME_MS
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
