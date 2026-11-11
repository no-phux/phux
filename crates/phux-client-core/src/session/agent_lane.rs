//! The `AgentSession` lane: a record stream with no replica.
//!
//! [`AgentStream`] owns the decoder, sequence fence and log; this is how
//! the kernel's lane dispatch drives it.

use phux_protocol::ResourceId;

use super::lane::{
    AttachSignal, BeginFrame, ChunkFrame, KindLane, LaneCx, LaneRails, OutputFrame, ReadyFrame,
};
use super::{
    AgentStream, EffectBuffer, EngineAdapter, GenerationId, KernelEffect, KernelError,
    TombstoneRecord,
};

impl LaneRails for AgentStream {
    fn on_tombstone(
        &mut self,
        generation: GenerationId,
        record: TombstoneRecord,
        _now_ms: u64,
    ) -> AttachSignal {
        self.note_external_tombstone(generation, record);
        AttachSignal::Unchanged
    }

    fn staged_accounting(&self) -> Option<(usize, usize)> {
        Self::staged_accounting(self)
    }

    fn expired_staging(&self, now_ms: u64) -> Option<GenerationId> {
        let (generation, clock, staged_bytes) = self.staging_clock()?;
        clock.expired(staged_bytes, now_ms).then_some(generation)
    }

    fn deadline_ms(&self) -> Option<u64> {
        let (_, clock, staged_bytes) = self.staging_clock()?;
        Some(clock.deadline_ms(staged_bytes))
    }

    fn retire_for_limit(
        &mut self,
        resource_id: &ResourceId,
        generation: GenerationId,
        effects: &mut EffectBuffer,
    ) {
        Self::retire_for_limit(self, resource_id, generation, effects);
    }
}

impl<E: EngineAdapter> KindLane<E> for AgentStream {
    fn on_begin(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &BeginFrame,
        _effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.begin(
            cx.resource_id,
            frame.generation.stream_id,
            frame.generation.bootstrap_id,
            frame.profile,
            frame.base_seq,
            cx.now_ms,
        )
    }

    fn on_chunk(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &ChunkFrame<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.accept_chunk(
            cx.resource_id,
            frame.generation,
            frame.chunk_seq,
            frame.payload,
            frame.usage_allowed,
            cx.now_ms,
            effects,
        )
    }

    fn on_ready(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &ReadyFrame<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        let publication =
            self.commit_ready(cx.resource_id, frame.generation, cx.now_ms, effects)?;
        effects.push(KernelEffect::AgentPublication(publication));
        cx.attach = AttachSignal::Resolved;
        Ok(())
    }

    fn on_output(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &OutputFrame<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        let publication = self.commit_live(
            cx.resource_id,
            frame.generation,
            frame.seq,
            frame.payload,
            effects,
        )?;
        effects.push(KernelEffect::AgentPublication(publication));
        Ok(())
    }
}
