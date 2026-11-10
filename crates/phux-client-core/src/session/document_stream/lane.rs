//! One document resource read on its own, outside a [`SessionKernel`].
//!
//! A headless reader (a snapshot or follow over one connection) folds its
//! frames through the same [`DocumentStream`] the kernel uses, so it keeps
//! the same rails: generation identity, contiguous chunks, the bootstrap
//! staging ceilings and lifetime, tombstones, and forward-only live frames.

use std::convert::Infallible;

#[cfg(doc)]
use super::super::SessionKernel;
use super::super::staging_rules::within_staging_ceilings;
use super::super::{
    BootstrapId, BootstrapStreamProfile, DocumentPublication, EffectBuffer, GenerationId,
    KernelError, ResourceId, ResourceKind, StreamId, TombstoneRecord, document_kind,
    is_agent_profile,
};
use super::DocumentStream;
use phux_protocol::wire::frame::TombstoneReason;

/// The document lane of one resource, driven frame by frame.
///
/// Times are caller-supplied monotonic milliseconds. A refusal fences the
/// generation it names where the kernel would; the caller decides whether
/// to end the read or wait for the server's recut.
pub struct DocumentLane {
    resource_id: ResourceId,
    stream: Option<DocumentStream>,
    /// Retirement effects have no host here; the returned error says it all.
    scratch: EffectBuffer,
}

impl std::fmt::Debug for DocumentLane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DocumentLane")
            .field("resource_id", &self.resource_id)
            .field("kind", &self.kind())
            .finish_non_exhaustive()
    }
}

impl DocumentLane {
    /// A lane for `resource_id` that has seen no bootstrap yet.
    #[must_use]
    pub const fn new(resource_id: ResourceId) -> Self {
        Self {
            resource_id,
            stream: None,
            scratch: EffectBuffer::new(),
        }
    }

    /// The document kind the first bootstrap named, if any arrived.
    #[must_use]
    pub fn kind(&self) -> Option<ResourceKind> {
        self.stream.as_ref().map(|stream| stream.kind)
    }

    /// `BOOTSTRAP_BEGIN`: open a cut; it replaces (and fences) one in flight.
    ///
    /// # Errors
    ///
    /// A non-document profile, a profile of another document kind, or a
    /// retired or duplicate generation.
    pub fn begin(
        &mut self,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        profile: BootstrapStreamProfile,
        base_seq: u64,
        now_ms: u64,
    ) -> Result<(), KernelError<Infallible>> {
        let incoming = document_kind(profile).ok_or_else(|| KernelError::UnsupportedKind {
            terminal_id: self.resource_id.clone(),
            kind: if is_agent_profile(profile) {
                ResourceKind::AgentSession
            } else {
                ResourceKind::Terminal
            },
        })?;
        let stream = self
            .stream
            .get_or_insert_with(|| DocumentStream::new(incoming));
        if stream.kind != incoming {
            return Err(KernelError::KindMismatch {
                terminal_id: self.resource_id.clone(),
                declared: stream.kind,
                incoming,
            });
        }
        stream.begin(
            &self.resource_id,
            GenerationId {
                stream_id,
                bootstrap_id,
            },
            base_seq,
            now_ms,
        )
    }

    /// `BOOTSTRAP_CHUNK`: stage the next contiguous chunk.
    ///
    /// # Errors
    ///
    /// A generation other than the cut in flight, a duplicate or gap, or a
    /// cut past the staging ceilings or lifetime; the last two fence it.
    pub fn chunk(
        &mut self,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        chunk_seq: u32,
        payload: &[u8],
        now_ms: u64,
    ) -> Result<(), KernelError<Infallible>> {
        let stream = Self::stream_mut(&mut self.stream, &self.resource_id)?;
        let (bytes, chunks) = stream.staged_accounting().unwrap_or_default();
        let usage_allowed = bytes
            .checked_add(payload.len())
            .is_some_and(|bytes| within_staging_ceilings(bytes, chunks.saturating_add(1)));
        self.scratch.clear();
        stream.accept_chunk(
            &self.resource_id,
            GenerationId {
                stream_id,
                bootstrap_id,
            },
            chunk_seq,
            payload,
            usage_allowed,
            now_ms,
            &mut self.scratch,
        )
    }

    /// `BOOTSTRAP_READY`: the staged cut, whole.
    ///
    /// # Errors
    ///
    /// A generation other than the cut in flight, or an expired cut.
    pub fn ready(
        &mut self,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        now_ms: u64,
    ) -> Result<DocumentPublication, KernelError<Infallible>> {
        let stream = Self::stream_mut(&mut self.stream, &self.resource_id)?;
        self.scratch.clear();
        stream.commit_ready(
            &self.resource_id,
            GenerationId {
                stream_id,
                bootstrap_id,
            },
            now_ms,
            &mut self.scratch,
        )
    }

    /// `RESOURCE_OUTPUT`: one whole newer document.
    ///
    /// # Errors
    ///
    /// A generation other than the published one, or a sequence at or
    /// behind the last document delivered.
    pub fn output(
        &mut self,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        seq: u64,
        payload: &[u8],
    ) -> Result<DocumentPublication, KernelError<Infallible>> {
        let stream = Self::stream_mut(&mut self.stream, &self.resource_id)?;
        stream.commit_live(
            &self.resource_id,
            GenerationId {
                stream_id,
                bootstrap_id,
            },
            seq,
            payload,
        )
    }

    /// `BOOTSTRAP_TOMBSTONE`: fence a generation; its late frames are refused.
    pub fn tombstone(
        &mut self,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        reason: TombstoneReason,
        last_valid_seq: u64,
    ) {
        if let Some(stream) = self.stream.as_mut() {
            stream.note_external_tombstone(
                GenerationId {
                    stream_id,
                    bootstrap_id,
                },
                TombstoneRecord {
                    reason,
                    last_valid_seq,
                },
            );
        }
    }

    fn stream_mut<'a>(
        stream: &'a mut Option<DocumentStream>,
        resource_id: &ResourceId,
    ) -> Result<&'a mut DocumentStream, KernelError<Infallible>> {
        stream
            .as_mut()
            .ok_or_else(|| KernelError::UnknownTerminal(resource_id.clone()))
    }
}

#[cfg(test)]
mod tests;
