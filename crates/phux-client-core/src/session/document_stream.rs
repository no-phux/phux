//! The document lane: `AccessibilityTree`, `PixelSurface` and `Document`.
//!
//! A document has no replica. Its bootstrap is the newest document in
//! chunks and every live frame is a whole newer document, each handed on as
//! a [`KernelEffect::Document`]. The lane keeps the same rails as the agent
//! lane: exact generation identity, contiguous `chunk_seq`, staging counted
//! against the connection-wide ceilings and staging clock, and retirement.
//!
//! A stream that stalls is recovered on its own ([`stall`]): the rest of
//! the connection carries on.

use std::collections::HashMap;

mod lane;
mod stall;

pub use lane::DocumentLane;

use super::lane::{
    AttachSignal, BeginFrame, ChunkFrame, KindLane, LaneCx, LaneRails, OutputFrame, ReadyFrame,
};
use super::staging_rules::{
    StagingClock, chunk_admission, fence_replaced, reject_retired, retire_for_codec_failure,
};
use super::{
    DocumentPublication, EffectBuffer, EngineAdapter, GenerationId, KernelEffect, KernelError,
    KernelStatus, ResourceId, ResourceKind, TombstoneRecord, mismatch_error,
};
use stall::RecutWatch;

struct DocumentCut {
    generation: GenerationId,
    base_seq: u64,
    next_chunk_seq: Option<u32>,
    buffer: Vec<u8>,
    accepted_chunks: usize,
    clock: StagingClock,
}

/// Kernel state for one document resource: the cut in flight and the fence.
pub(super) struct DocumentStream {
    /// Named by the first bootstrap; a headless reader may see a fence first.
    kind: Option<ResourceKind>,
    /// A typed document's content type, from its bootstrap.
    content_type: Option<String>,
    /// The published generation and the last document it delivered.
    published: Option<(GenerationId, u64)>,
    staging: Option<DocumentCut>,
    retired: HashMap<GenerationId, TombstoneRecord>,
    recut_watch: Option<RecutWatch>,
    /// Consecutive stalls since a document was last delivered.
    stalls: u32,
}

impl DocumentStream {
    pub(super) fn new(kind: Option<ResourceKind>) -> Self {
        Self {
            kind,
            content_type: None,
            published: None,
            staging: None,
            retired: HashMap::new(),
            recut_watch: None,
            stalls: 0,
        }
    }

    /// Bytes and chunks accepted by the open staging cut.
    pub(super) fn staged_accounting(&self) -> Option<(usize, usize)> {
        self.staging
            .as_ref()
            .map(|cut| (cut.buffer.len(), cut.accepted_chunks))
    }

    pub(super) fn expired_staging(&self, now_ms: u64) -> Option<GenerationId> {
        let cut = self.staging.as_ref()?;
        cut.clock
            .expired(cut.buffer.len(), now_ms)
            .then_some(cut.generation)
    }

    pub(super) fn retire_for_limit(
        &mut self,
        resource_id: &ResourceId,
        generation: GenerationId,
        effects: &mut EffectBuffer,
    ) {
        let last_valid_seq = self.staging.as_ref().map_or(0, |cut| cut.base_seq);
        self.retire_generation(resource_id, generation, last_valid_seq, effects);
    }

    fn begin<E>(
        &mut self,
        resource_id: &ResourceId,
        generation: GenerationId,
        base_seq: u64,
        started_at_ms: u64,
    ) -> Result<(), KernelError<E>> {
        self.reject_retired(resource_id, generation)?;
        if self.published_matches(generation) || self.staging_matches(generation) {
            return Err(KernelError::DuplicateGeneration {
                terminal_id: resource_id.clone(),
                stream_id: generation.stream_id,
                bootstrap_id: generation.bootstrap_id,
            });
        }
        self.recut_watch = None;
        let replaced = self.staging.replace(DocumentCut {
            generation,
            base_seq,
            next_chunk_seq: Some(0),
            buffer: Vec::new(),
            accepted_chunks: 0,
            clock: StagingClock::start(started_at_ms),
        });
        if let Some(old) = replaced {
            fence_replaced(&mut self.retired, old.generation, old.base_seq);
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "wire chunk identity plus connection budget, clock and retirement effects stay explicit"
    )]
    fn accept_chunk<E>(
        &mut self,
        resource_id: &ResourceId,
        generation: GenerationId,
        chunk_seq: u32,
        payload: &[u8],
        usage_allowed: bool,
        now_ms: u64,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E>> {
        self.reject_retired(resource_id, generation)?;
        let cut = self
            .staging
            .as_mut()
            .filter(|cut| cut.generation == generation)
            .ok_or_else(|| mismatch_error(resource_id, generation))?;
        let admission = chunk_admission(
            cut.next_chunk_seq,
            chunk_seq,
            cut.clock,
            cut.buffer.len().saturating_add(payload.len()),
            now_ms,
            usage_allowed,
        );
        if let Err(error) = admission {
            let base_seq = cut.base_seq;
            self.retire_generation(resource_id, generation, base_seq, effects);
            return Err(error);
        }
        cut.buffer.extend_from_slice(payload);
        cut.next_chunk_seq = chunk_seq.checked_add(1);
        cut.accepted_chunks += 1;
        cut.clock.progress(now_ms);
        Ok(())
    }

    /// Publish a READY cut; the previously published generation is retired.
    fn commit_ready<E>(
        &mut self,
        resource_id: &ResourceId,
        generation: GenerationId,
        now_ms: u64,
        effects: &mut EffectBuffer,
    ) -> Result<DocumentPublication, KernelError<E>> {
        self.reject_retired(resource_id, generation)?;
        let kind = self
            .kind
            .ok_or_else(|| mismatch_error(resource_id, generation))?;
        let cut = self
            .staging
            .take_if(|cut| cut.generation == generation)
            .ok_or_else(|| mismatch_error(resource_id, generation))?;
        if cut.clock.expired(cut.buffer.len(), now_ms) {
            self.retire_generation(resource_id, generation, cut.base_seq, effects);
            return Err(KernelError::BootstrapStagingLimitExceeded);
        }
        let mut bytes = cut.buffer;
        // Every typed cut names its own content type first; an earlier
        // cut's never stands in for a missing one.
        if kind == ResourceKind::Document {
            let Some((content_type, document)) =
                phux_protocol::wire::frame::typed_document::split_bootstrap(&bytes)
            else {
                self.retire_generation(resource_id, generation, cut.base_seq, effects);
                return Err(KernelError::UntypedDocument(resource_id.clone()));
            };
            self.content_type = Some(content_type.to_owned());
            bytes = document.to_vec();
        }
        if let Some((old, last_seq)) = self.published.replace((generation, cut.base_seq)) {
            fence_replaced(&mut self.retired, old, last_seq);
        }
        self.stalls = 0;
        Ok(self.publication(resource_id, kind, cut.base_seq, bytes))
    }

    /// Each live frame is a whole newer document, so a skipped sequence loses
    /// nothing; one at or behind the last delivered is refused.
    fn commit_live<E>(
        &mut self,
        resource_id: &ResourceId,
        generation: GenerationId,
        seq: u64,
        payload: &[u8],
    ) -> Result<DocumentPublication, KernelError<E>> {
        self.reject_retired(resource_id, generation)?;
        let kind = self
            .kind
            .ok_or_else(|| mismatch_error(resource_id, generation))?;
        let last_seq = self
            .published
            .as_mut()
            .filter(|(published, _)| *published == generation)
            .map(|(_, last_seq)| last_seq)
            .ok_or_else(|| mismatch_error(resource_id, generation))?;
        if seq <= *last_seq {
            return Err(KernelError::DuplicateSequence {
                expected: last_seq
                    .checked_add(1)
                    .ok_or(KernelError::SequenceExhausted)?,
                actual: seq,
            });
        }
        *last_seq = seq;
        self.stalls = 0;
        Ok(self.publication(resource_id, kind, seq, payload.to_vec()))
    }

    /// A cut in a codec this build cannot decode replaces the stream: the
    /// cut in flight, the published generation, and the refused one are
    /// fenced, so their late frames stay fenced once a known bootstrap lifts
    /// the refusal.
    pub(super) fn fence_for_unknown_codec(&mut self, generation: GenerationId, base_seq: u64) {
        self.recut_watch = None;
        if let Some(cut) = self.staging.take() {
            fence_replaced(&mut self.retired, cut.generation, cut.base_seq);
        }
        if let Some((published, last_seq)) = self.published.take() {
            fence_replaced(&mut self.retired, published, last_seq);
        }
        fence_replaced(&mut self.retired, generation, base_seq);
    }

    fn publication(
        &self,
        resource_id: &ResourceId,
        kind: ResourceKind,
        seq: u64,
        bytes: Vec<u8>,
    ) -> DocumentPublication {
        DocumentPublication {
            resource_id: resource_id.clone(),
            kind,
            seq,
            bytes,
            content_type: self.content_type.clone(),
        }
    }

    fn reject_retired<E>(
        &self,
        resource_id: &ResourceId,
        generation: GenerationId,
    ) -> Result<(), KernelError<E>> {
        reject_retired(&self.retired, resource_id, generation)
    }

    fn published_matches(&self, generation: GenerationId) -> bool {
        self.published
            .is_some_and(|(published, _)| published == generation)
    }

    fn staging_matches(&self, generation: GenerationId) -> bool {
        self.staging
            .as_ref()
            .is_some_and(|cut| cut.generation == generation)
    }

    fn retire_generation(
        &mut self,
        resource_id: &ResourceId,
        generation: GenerationId,
        last_valid_seq: u64,
        effects: &mut EffectBuffer,
    ) {
        if self.staging_matches(generation) {
            self.staging = None;
        }
        if self.published_matches(generation) {
            self.published = None;
        }
        retire_for_codec_failure(
            &mut self.retired,
            resource_id,
            generation,
            last_valid_seq,
            effects,
        );
    }
}

impl LaneRails for DocumentStream {
    /// A lapped viewer is fenced and recut: the fenced generation refuses
    /// late frames, and the recut's READY carries the newest document.
    fn on_tombstone(
        &mut self,
        generation: GenerationId,
        record: TombstoneRecord,
        now_ms: u64,
    ) -> AttachSignal {
        self.note_external_tombstone(generation, record, now_ms);
        AttachSignal::Unchanged
    }

    fn staged_accounting(&self) -> Option<(usize, usize)> {
        Self::staged_accounting(self)
    }

    fn expired_staging(&self, now_ms: u64) -> Option<GenerationId> {
        Self::expired_staging(self, now_ms)
    }

    fn take_stall(&mut self, resource_id: &ResourceId, now_ms: u64) -> Option<KernelStatus> {
        Self::take_stall(self, resource_id, now_ms)
    }

    fn deadline_ms(&self) -> Option<u64> {
        Self::deadline_ms(self)
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

/// A document is no attach participant: publishing one never resolves the
/// ATTACH barrier.
impl<E: EngineAdapter> KindLane<E> for DocumentStream {
    fn on_begin(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &BeginFrame,
        _effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.begin(cx.resource_id, frame.generation, frame.base_seq, cx.now_ms)
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
        effects.push(KernelEffect::Document(publication));
        Ok(())
    }

    fn on_output(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &OutputFrame<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        let publication =
            self.commit_live(cx.resource_id, frame.generation, frame.seq, frame.payload)?;
        effects.push(KernelEffect::Document(publication));
        Ok(())
    }
}
