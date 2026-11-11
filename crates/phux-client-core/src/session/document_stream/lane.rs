//! One document resource read on its own, outside a [`SessionKernel`].
//!
//! A headless reader (a snapshot or follow over one connection) folds its
//! frames through the same [`DocumentStream`] the kernel uses, so it keeps
//! the same rails: generation identity, contiguous chunks, the bootstrap
//! staging ceilings and clock, tombstones, forward-only live frames, and
//! stall recovery.

use std::convert::Infallible;

#[cfg(doc)]
use super::super::SessionKernel;
use super::super::deadlines::MAX_BENIGN_DROPS;
use super::super::staging_rules::within_staging_ceilings;
use super::super::{
    BootstrapId, BootstrapStreamProfile, DocumentPublication, EffectBuffer, GenerationId,
    KernelError, KernelStatus, ResourceId, ResourceKind, StreamId, TombstoneRecord, document_kind,
    is_agent_profile,
};
use super::DocumentStream;
use phux_protocol::wire::frame::TombstoneReason;

/// The document lane of one resource, driven frame by frame.
///
/// Times are caller-supplied monotonic milliseconds. A refusal fences the
/// generation it names where the kernel would; the caller decides whether
/// to end the read or wait for the server's recut, and [`Self::stalled`]
/// says when to attach again.
pub struct DocumentLane {
    resource_id: ResourceId,
    stream: DocumentStream,
    /// Consecutive frames dropped off the current generations.
    benign_drops: u32,
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
    pub fn new(resource_id: ResourceId) -> Self {
        Self {
            resource_id,
            stream: DocumentStream::new(None),
            benign_drops: 0,
            scratch: EffectBuffer::new(),
        }
    }

    /// The document kind the first bootstrap named, if any arrived.
    #[must_use]
    pub const fn kind(&self) -> Option<ResourceKind> {
        self.stream.kind
    }

    /// The resource was just attached at `now_ms`: unless a cut or a
    /// document is already live, its first cut is awaited like a recut.
    pub fn await_first_cut(&mut self, now_ms: u64) {
        if self.stream.is_idle() {
            self.stream.await_recut(TombstoneReason::Other, now_ms);
        }
    }

    /// `BOOTSTRAP_BEGIN`: open a cut; it replaces (and fences) one in flight.
    ///
    /// # Errors
    ///
    /// A codec this build cannot decode, a non-document profile, a profile
    /// of another document kind, or a retired or duplicate generation.
    pub fn begin(
        &mut self,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        profile: BootstrapStreamProfile,
        base_seq: u64,
        now_ms: u64,
    ) -> Result<(), KernelError<Infallible>> {
        if let BootstrapStreamProfile::Unknown { codec_tag } = profile {
            return Err(KernelError::UnknownCodec {
                terminal_id: self.resource_id.clone(),
                codec_tag,
            });
        }
        let incoming = document_kind(profile).ok_or_else(|| KernelError::UnsupportedKind {
            terminal_id: self.resource_id.clone(),
            kind: if is_agent_profile(profile) {
                ResourceKind::AgentSession
            } else {
                ResourceKind::Terminal
            },
        })?;
        let declared = *self.stream.kind.get_or_insert(incoming);
        if declared != incoming {
            return Err(KernelError::KindMismatch {
                terminal_id: self.resource_id.clone(),
                declared,
                incoming,
            });
        }
        let result = self.stream.begin(
            &self.resource_id,
            generation(stream_id, bootstrap_id),
            base_seq,
            now_ms,
        );
        self.tally(result)
    }

    /// `BOOTSTRAP_CHUNK`: stage the next contiguous chunk.
    ///
    /// # Errors
    ///
    /// A generation other than the cut in flight, a duplicate or gap, or a
    /// cut past the staging ceilings or clock; the last two fence it.
    pub fn chunk(
        &mut self,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        chunk_seq: u32,
        payload: &[u8],
        now_ms: u64,
    ) -> Result<(), KernelError<Infallible>> {
        let (bytes, chunks) = self.stream.staged_accounting().unwrap_or_default();
        let usage_allowed = bytes
            .checked_add(payload.len())
            .is_some_and(|bytes| within_staging_ceilings(bytes, chunks.saturating_add(1)));
        self.scratch.clear();
        let result = self.stream.accept_chunk(
            &self.resource_id,
            generation(stream_id, bootstrap_id),
            chunk_seq,
            payload,
            usage_allowed,
            now_ms,
            &mut self.scratch,
        );
        self.tally(result)
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
        self.scratch.clear();
        let result = self.stream.commit_ready(
            &self.resource_id,
            generation(stream_id, bootstrap_id),
            now_ms,
            &mut self.scratch,
        );
        self.tally(result)
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
        let result = self.stream.commit_live(
            &self.resource_id,
            generation(stream_id, bootstrap_id),
            seq,
            payload,
        );
        self.tally(result)
    }

    /// `BOOTSTRAP_TOMBSTONE`: fence a generation; its late frames are refused.
    /// Fencing the live generation, or any while nothing is live, starts the
    /// wait for a recut.
    pub fn tombstone(
        &mut self,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        reason: TombstoneReason,
        last_valid_seq: u64,
        now_ms: u64,
    ) {
        self.stream.note_external_tombstone(
            generation(stream_id, bootstrap_id),
            TombstoneRecord {
                reason,
                last_valid_seq,
            },
            now_ms,
        );
    }

    /// Whether a cut in flight or an awaited recut has a deadline, so the
    /// caller knows to call [`Self::stalled`] even while no frame arrives.
    #[must_use]
    pub fn has_deadline(&self) -> bool {
        self.benign_drops >= MAX_BENIGN_DROPS || self.stream.deadline_ms().is_some()
    }

    /// The stall due at `now_ms`, reported once:
    /// [`KernelStatus::DocumentStalled`] asks the caller to attach again,
    /// and [`KernelStatus::DocumentLost`] says the re-attaches ran out. A
    /// stall is a cut that stopped arriving, a fence no recut followed, or
    /// a run of frames that all named a generation that is not current.
    pub fn stalled(&mut self, now_ms: u64) -> Option<KernelStatus> {
        if self.benign_drops >= MAX_BENIGN_DROPS {
            self.benign_drops = 0;
            return Some(
                self.stream
                    .stall(&self.resource_id, TombstoneReason::Other, now_ms),
            );
        }
        self.stream.take_stall(&self.resource_id, now_ms)
    }

    const fn tally<T>(
        &mut self,
        result: Result<T, KernelError<Infallible>>,
    ) -> Result<T, KernelError<Infallible>> {
        match &result {
            Ok(_) => self.benign_drops = 0,
            Err(error) if error.is_benign_generation() => {
                self.benign_drops = self.benign_drops.saturating_add(1);
            }
            Err(_) => {}
        }
        result
    }
}

const fn generation(stream_id: StreamId, bootstrap_id: BootstrapId) -> GenerationId {
    GenerationId {
        stream_id,
        bootstrap_id,
    }
}

#[cfg(test)]
mod tests;
