//! The open staging generation: opening one, and what the kernel reads of
//! it for its ceilings and deadlines.

use phux_protocol::wire::frame::TombstoneReason;
use phux_protocol::{BootstrapId, BootstrapStreamProfile, ResourceId, StreamId};

use super::super::super::staging_rules::StagingClock;
use super::super::super::{GenerationId, ReplicaKey, TombstoneRecord, generation_of};
use super::{AgentStaging, AgentStream, BootstrapRecordDecoder};

impl AgentStream {
    /// Bytes and chunks accepted by the open staging generation.
    pub(in crate::session) fn staged_accounting(&self) -> Option<(usize, usize)> {
        self.staging
            .as_ref()
            .map(|staging| (staging.accepted_bytes, staging.accepted_chunks))
    }

    /// The open staging generation, its clock, and its staged bytes.
    pub(in crate::session) fn staging_clock(&self) -> Option<(GenerationId, StagingClock, usize)> {
        let staging = self.staging.as_ref()?;
        let generation = generation_of(&staging.key);
        Some((generation, staging.clock, staging.accepted_bytes))
    }

    /// Staged record count while a generation is open.
    ///
    /// `None` after commit, retirement, or before begin. Session tests use
    /// this to observe that a split line is not decoded before its LF.
    #[cfg(test)]
    pub(in crate::session) fn staged_record_count(&self) -> Option<usize> {
        self.staging.as_ref().map(|staging| staging.records.len())
    }

    /// Open a staging generation; one already open is replaced and fenced.
    pub(super) fn open_staging(
        &mut self,
        resource_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        profile: BootstrapStreamProfile,
        base_seq: u64,
        started_at_ms: u64,
    ) {
        let replaced = self.staging.replace(AgentStaging {
            key: ReplicaKey {
                terminal_id: resource_id.clone(),
                stream_id,
                bootstrap_id,
                profile,
            },
            base_seq,
            next_chunk_seq: Some(0),
            records: Vec::new(),
            accepted_bytes: 0,
            accepted_chunks: 0,
            clock: StagingClock::start(started_at_ms),
            decoder: BootstrapRecordDecoder::default(),
        });
        if let Some(old) = replaced {
            self.retired.insert(
                generation_of(&old.key),
                TombstoneRecord {
                    reason: TombstoneReason::ExplicitReattach,
                    last_valid_seq: old.base_seq,
                },
            );
        }
    }
}
