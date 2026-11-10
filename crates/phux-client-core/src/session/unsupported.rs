//! Refusing one resource whose stream codec this build does not know.
//!
//! A newer server may stream a kind or codec this client cannot decode
//! (`docs/compatibility.md#a-newer-server-an-older-client`). That is not a
//! protocol violation: the kernel reports it once, drops whatever it staged
//! for the resource, and ignores the stream's later frames, so every other
//! resource and the connection carry on.

use super::{
    EffectBuffer, EngineAdapter, KernelEffect, KernelInput, KernelStatus, ResourceId, SessionKernel,
};

impl<E: EngineAdapter> SessionKernel<E> {
    /// Refuse `terminal_id`'s stream, which names `codec_tag`.
    pub(super) fn refuse_unknown_codec(
        &mut self,
        terminal_id: &ResourceId,
        codec_tag: u8,
        effects: &mut EffectBuffer,
    ) {
        self.documents.remove(terminal_id);
        // A refused participant must not hold the aggregate barrier open.
        self.mark_attach_resolved(terminal_id);
        if self.unsupported.insert(terminal_id.clone()) {
            effects.push(KernelEffect::Status(KernelStatus::Unsupported {
                terminal_id: terminal_id.clone(),
                codec_tag,
            }));
        }
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
