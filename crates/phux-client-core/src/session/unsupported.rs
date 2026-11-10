//! Refusing one resource whose stream codec this build does not know.
//!
//! A newer server may stream a kind or codec this client cannot decode
//! (`docs/compatibility.md#a-newer-server-an-older-client`). For a resource
//! of no known kind, or of a document kind, that is not a protocol
//! violation: the kernel reports it once, fences whatever generations it
//! held for the resource, and ignores the stream's later frames, so every
//! other resource and the connection carry on. A Terminal or `AgentSession` has no newer
//! codec this client did not offer, so an unknown one there is malformed.

use super::{
    BootstrapStreamProfile, EffectBuffer, EngineAdapter, GenerationId, KernelEffect, KernelError,
    KernelInput, KernelStatus, ResourceId, ResourceKind, SessionKernel,
};

impl<E: EngineAdapter> SessionKernel<E> {
    /// Refuse `terminal_id`'s stream, whose `generation` names `codec_tag`,
    /// or fail when the resource is a kind whose codec cannot be newer.
    pub(super) fn refuse_unknown_codec(
        &mut self,
        terminal_id: &ResourceId,
        generation: GenerationId,
        codec_tag: u8,
        base_seq: u64,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        match self.kinds.get(terminal_id) {
            Some(ResourceKind::Terminal) => {
                return Err(KernelError::ProfileMismatch {
                    selected: self.selected_profile,
                    incoming: BootstrapStreamProfile::Unknown { codec_tag },
                });
            }
            // The codec names no kind this build knows.
            Some(declared @ ResourceKind::AgentSession) => {
                return Err(KernelError::KindMismatch {
                    terminal_id: terminal_id.clone(),
                    declared: *declared,
                    incoming: ResourceKind::Unknown { tag: codec_tag },
                });
            }
            _ => {}
        }
        // Only a Terminal or AgentSession stages a replica or a record
        // stream, and both were refused above; a document stages a cut. Its
        // generations stay fenced rather than forgotten.
        debug_assert!(
            !self.terminals.contains_key(terminal_id) && !self.agents.contains_key(terminal_id)
        );
        if let Some(stream) = self.documents.get_mut(terminal_id) {
            stream.fence_for_unknown_codec(generation, base_seq);
        }
        if self.unsupported.insert(terminal_id.clone()) {
            effects.push(KernelEffect::Status(KernelStatus::Unsupported {
                terminal_id: terminal_id.clone(),
                codec_tag,
            }));
        }
        Ok(())
    }

    /// Whether `input` belongs to a refused stream and changes nothing.
    pub(super) fn is_refused_stream_frame(&self, input: &KernelInput<'_>) -> bool {
        let terminal_id = match input {
            KernelInput::BootstrapChunk { terminal_id, .. }
            | KernelInput::BootstrapReady { terminal_id, .. }
            | KernelInput::ResourceOutput { terminal_id, .. }
            | KernelInput::Tombstone { terminal_id, .. }
            | KernelInput::HistoryPage { terminal_id, .. }
            | KernelInput::HistoryTombstone { terminal_id, .. }
            | KernelInput::HistoryRejected { terminal_id, .. } => *terminal_id,
            _ => return false,
        };
        self.unsupported.contains(terminal_id)
    }

    /// Whether the kernel refused `terminal_id`'s stream as an unknown codec.
    #[must_use]
    pub fn is_unsupported(&self, terminal_id: &ResourceId) -> bool {
        self.unsupported.contains(terminal_id)
    }
}
