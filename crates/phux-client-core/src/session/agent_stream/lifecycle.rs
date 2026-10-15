//! Per-agent staging, validation, commit, and retirement.
//!
//! The session kernel admits the resource and enforces the connection-wide
//! staging budget. One [`AgentStream`] owns the decoder, sequence fence, log,
//! and the publication a successful commit returns.

use std::collections::HashMap;

use phux_protocol::wire::frame::TombstoneReason;
use phux_protocol::{BootstrapId, BootstrapStreamProfile, ResourceId, StreamId};

use super::super::{
    AgentSessionDeclaration, AgentStreamInfo, EffectBuffer, GenerationId, KernelEffect,
    KernelError, KernelStatus, MAX_BOOTSTRAP_STAGING_LIFETIME_MS, ReplicaKey, TombstoneRecord,
    generation_of, mismatch_error, retired_error,
};
use super::{
    AgentEventRecord, AgentLog, AgentPublication, AgentRecordMode, AgentSessionState,
    AgentSessionStatus, BootstrapRecordDecoder, parse_records,
};

struct AgentGeneration {
    key: ReplicaKey,
    last_seq: u64,
    next_seq: Option<u64>,
}

struct AgentStaging {
    key: ReplicaKey,
    base_seq: u64,
    next_chunk_seq: Option<u32>,
    records: Vec<AgentEventRecord>,
    accepted_bytes: usize,
    accepted_chunks: usize,
    started_at_ms: u64,
    decoder: BootstrapRecordDecoder,
}

/// Kernel state for one `AgentSession` resource: no replica, a record log.
pub(in crate::session) struct AgentStream {
    parent: Option<ResourceId>,
    /// Identity and state as declared before any record arrived; the
    /// baseline every published generation folds its records onto.
    declared: AgentSessionState,
    state: AgentSessionState,
    native_view: Option<phux_protocol::native_agent::NativeView>,
    log: AgentLog,
    published: Option<AgentGeneration>,
    staging: Option<AgentStaging>,
    retired: HashMap<GenerationId, TombstoneRecord>,
}

impl AgentStream {
    pub(in crate::session) fn new() -> Self {
        Self {
            parent: None,
            declared: AgentSessionState::default(),
            state: AgentSessionState::default(),
            native_view: None,
            log: AgentLog::default(),
            published: None,
            staging: None,
            retired: HashMap::new(),
        }
    }

    pub(in crate::session) const fn parent(&self) -> Option<&ResourceId> {
        self.parent.as_ref()
    }

    pub(in crate::session) const fn state(&self) -> &AgentSessionState {
        &self.state
    }

    pub(in crate::session) const fn native_view(
        &self,
    ) -> Option<&phux_protocol::native_agent::NativeView> {
        self.native_view.as_ref()
    }

    pub(in crate::session) const fn log(&self) -> &AgentLog {
        &self.log
    }

    pub(in crate::session) const fn is_published(&self) -> bool {
        self.published.is_some()
    }

    pub(in crate::session) fn committed(&self) -> Option<AgentStreamInfo> {
        self.published.as_ref().map(|published| AgentStreamInfo {
            stream_id: published.key.stream_id,
            bootstrap_id: published.key.bootstrap_id,
            last_seq: published.last_seq,
        })
    }

    /// Bytes and chunks accepted by the open staging generation.
    pub(in crate::session) fn staged_accounting(&self) -> Option<(usize, usize)> {
        self.staging
            .as_ref()
            .map(|staging| (staging.accepted_bytes, staging.accepted_chunks))
    }

    pub(in crate::session) fn expired_staging(&self, now_ms: u64) -> Option<GenerationId> {
        let staging = self.staging.as_ref()?;
        staging_lifetime_exceeded(staging.started_at_ms, now_ms)
            .then(|| generation_of(&staging.key))
    }

    /// Staged record count while a generation is open.
    ///
    /// `None` after commit, retirement, or before begin. Session tests use
    /// this to observe that a split line is not decoded before its LF.
    #[cfg(test)]
    pub(in crate::session) fn staged_record_count(&self) -> Option<usize> {
        self.staging.as_ref().map(|staging| staging.records.len())
    }

    pub(in crate::session) fn declare(&mut self, declaration: &AgentSessionDeclaration<'_>) {
        let declared = AgentSessionState {
            provider: declaration.provider.map(ToOwned::to_owned),
            native_id: declaration.native_id.map(ToOwned::to_owned),
            status: declaration
                .state
                .map_or(AgentSessionStatus::Unknown, AgentSessionStatus::parse),
        };
        if let Some(parent) = declaration.parent {
            self.parent = Some(parent.clone());
        }
        if self.published.is_none() {
            self.state.clone_from(&declared);
        } else {
            if self.state.provider.is_none() {
                self.state.provider.clone_from(&declared.provider);
            }
            if self.state.native_id.is_none() {
                self.state.native_id.clone_from(&declared.native_id);
            }
        }
        self.declared = declared;
    }

    pub(in crate::session) fn begin<E>(
        &mut self,
        resource_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        profile: BootstrapStreamProfile,
        base_seq: u64,
        started_at_ms: u64,
    ) -> Result<(), KernelError<E>> {
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        if self.retired.contains_key(&generation) {
            return Err(retired_error(resource_id, generation));
        }
        if self.published_matches(generation) || self.staging_matches(generation) {
            return Err(KernelError::DuplicateGeneration {
                terminal_id: resource_id.clone(),
                stream_id,
                bootstrap_id,
            });
        }
        if let BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq } = profile {
            self.require_resume_anchor(resource_id, after_seq)?;
            if base_seq < after_seq {
                return Err(KernelError::DuplicateSequence {
                    expected: after_seq,
                    actual: base_seq,
                });
            }
        }
        self.open_staging(
            resource_id,
            stream_id,
            bootstrap_id,
            profile,
            base_seq,
            started_at_ms,
        );
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "wire chunk identity plus connection budget, clock and retirement effects stay explicit"
    )]
    pub(in crate::session) fn accept_chunk<E>(
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
        let staging = self
            .staging
            .as_mut()
            .filter(|staging| generation_of(&staging.key) == generation)
            .ok_or_else(|| mismatch_error(resource_id, generation))?;
        let base_seq = staging.base_seq;
        let admission = chunk_admission(
            staging.next_chunk_seq,
            chunk_seq,
            staging.started_at_ms,
            now_ms,
            usage_allowed,
        );
        if let Err(error) = admission {
            self.retire_generation(resource_id, generation, base_seq, effects);
            return Err(error);
        }
        match staging.decoder.push(payload) {
            Ok(records) => {
                staging.records.extend(records);
                staging.next_chunk_seq = chunk_seq.checked_add(1);
                staging.accepted_bytes += payload.len();
                staging.accepted_chunks += 1;
                Ok(())
            }
            Err(error) => {
                self.retire_generation(resource_id, generation, base_seq, effects);
                Err(KernelError::AgentRecord(error))
            }
        }
    }

    /// Commit a READY cut. Success always returns a publication, including an
    /// empty resume. Continuity and `decoder.finish` run before any log mutation.
    pub(in crate::session) fn commit_ready<E>(
        &mut self,
        resource_id: &ResourceId,
        generation: GenerationId,
        now_ms: u64,
        effects: &mut EffectBuffer,
    ) -> Result<AgentPublication, KernelError<E>> {
        self.reject_retired(resource_id, generation)?;
        let (base_seq, continuity) = {
            let staging = self
                .staging
                .as_ref()
                .filter(|staging| generation_of(&staging.key) == generation)
                .ok_or_else(|| mismatch_error(resource_id, generation))?;
            (
                staging.base_seq,
                self.ready_continuity(resource_id, staging, now_ms),
            )
        };
        if let Err(error) = continuity {
            self.retire_generation(resource_id, generation, base_seq, effects);
            return Err(error);
        }
        self.finish_ready(resource_id, generation, effects)
    }

    /// Commit one live batch as [`AgentRecordMode::Append`].
    ///
    /// The proven published generation is updated only after the whole batch
    /// validates. A decode failure retires that generation; a sequence fence
    /// does not, and neither path publishes.
    pub(in crate::session) fn commit_live<E>(
        &mut self,
        resource_id: &ResourceId,
        generation: GenerationId,
        seq: u64,
        payload: &[u8],
        effects: &mut EffectBuffer,
    ) -> Result<AgentPublication, KernelError<E>> {
        self.reject_retired(resource_id, generation)?;
        let published = self
            .published
            .as_mut()
            .filter(|published| generation_of(&published.key) == generation)
            .ok_or_else(|| mismatch_error(resource_id, generation))?;
        let Some(expected) = published.next_seq else {
            return Err(KernelError::SequenceExhausted);
        };
        if seq < expected {
            return Err(KernelError::DuplicateSequence {
                expected,
                actual: seq,
            });
        }
        let last_seq = published.last_seq;
        let records = match parse_records(payload) {
            Ok(records) => records,
            Err(error) => {
                self.retire_generation(resource_id, generation, last_seq, effects);
                return Err(KernelError::AgentRecord(error));
            }
        };
        validate_batch(&records, expected, seq)?;
        published.last_seq = seq;
        published.next_seq = seq.checked_add(1);
        self.fold_and_append(&records);
        Ok(AgentPublication {
            resource_id: resource_id.clone(),
            stream: AgentStreamInfo {
                stream_id: generation.stream_id,
                bootstrap_id: generation.bootstrap_id,
                last_seq: seq,
            },
            mode: AgentRecordMode::Append,
            records,
        })
    }

    pub(in crate::session) fn retire_for_limit(
        &mut self,
        resource_id: &ResourceId,
        generation: GenerationId,
        effects: &mut EffectBuffer,
    ) {
        let last_valid_seq = self.staging.as_ref().map_or(0, |staging| staging.base_seq);
        self.retire_generation(resource_id, generation, last_valid_seq, effects);
    }

    /// Record an external tombstone. Staging for that generation is dropped;
    /// a different published generation is left in place and no effect is emitted.
    pub(in crate::session) fn note_external_tombstone(
        &mut self,
        generation: GenerationId,
        record: TombstoneRecord,
    ) {
        self.retired.entry(generation).or_insert(record);
        if self.staging_matches(generation) {
            self.staging = None;
        }
    }

    fn open_staging(
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
            started_at_ms,
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

    /// Resume anchor, record continuity, and staging lifetime for one READY cut.
    fn ready_continuity<E>(
        &self,
        resource_id: &ResourceId,
        staging: &AgentStaging,
        now_ms: u64,
    ) -> Result<(), KernelError<E>> {
        if staging_lifetime_exceeded(staging.started_at_ms, now_ms) {
            return Err(KernelError::BootstrapStagingLimitExceeded);
        }
        let expected = if let BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq } =
            staging.key.profile
        {
            self.require_resume_anchor(resource_id, after_seq)?;
            if staging.records.is_empty() {
                require_sequence(after_seq, staging.base_seq)?;
                return Ok(());
            }
            after_seq
                .checked_add(1)
                .ok_or(KernelError::SequenceExhausted)?
        } else if let Some(first) = staging.records.first() {
            if first.seq == 0 {
                return Err(KernelError::DuplicateSequence {
                    expected: 1,
                    actual: 0,
                });
            }
            first.seq
        } else {
            return Ok(());
        };
        validate_batch(&staging.records, expected, staging.base_seq)
    }

    fn require_resume_anchor<E>(
        &self,
        resource_id: &ResourceId,
        after_seq: u64,
    ) -> Result<(), KernelError<E>> {
        let committed = self
            .published
            .as_ref()
            .ok_or_else(|| KernelError::AgentResumeUnavailable(resource_id.clone()))?;
        require_sequence(committed.last_seq, after_seq)
    }

    fn finish_ready<E>(
        &mut self,
        resource_id: &ResourceId,
        generation: GenerationId,
        effects: &mut EffectBuffer,
    ) -> Result<AgentPublication, KernelError<E>> {
        let Some(staging) = self.staging.take() else {
            return Err(KernelError::MissingStaging(resource_id.clone()));
        };
        if let Err(error) = staging.decoder.finish() {
            self.retire_generation(resource_id, generation, staging.base_seq, effects);
            return Err(KernelError::AgentRecord(error));
        }
        Ok(self.publish_staged(resource_id, staging))
    }

    fn publish_staged(
        &mut self,
        resource_id: &ResourceId,
        staging: AgentStaging,
    ) -> AgentPublication {
        let AgentStaging {
            key,
            base_seq,
            records,
            ..
        } = staging;
        let mode = if matches!(
            key.profile,
            BootstrapStreamProfile::AgentEventsJsonlResumeV1 { .. }
        ) {
            AgentRecordMode::Append
        } else {
            AgentRecordMode::Replace
        };
        let stream = AgentStreamInfo {
            stream_id: key.stream_id,
            bootstrap_id: key.bootstrap_id,
            last_seq: base_seq,
        };
        if mode == AgentRecordMode::Replace {
            let declared = self.declared.clone();
            self.state = declared;
            self.native_view = None;
            self.log.clear();
        }
        self.fold_and_append(&records);
        let replaced = self.published.replace(AgentGeneration {
            key,
            last_seq: base_seq,
            next_seq: base_seq.checked_add(1),
        });
        if let Some(old) = replaced {
            self.retired
                .entry(generation_of(&old.key))
                .or_insert(TombstoneRecord {
                    reason: TombstoneReason::ExplicitReattach,
                    last_valid_seq: old.last_seq,
                });
        }
        AgentPublication {
            resource_id: resource_id.clone(),
            stream,
            mode,
            records,
        }
    }

    fn fold_and_append(&mut self, records: &[AgentEventRecord]) {
        for record in records {
            self.state.fold(record);
            if record.kind == super::AgentEventKind::ProviderRaw
                && record.data_str("schema") == Some(phux_protocol::native_agent::NATIVE_SCHEMA)
            {
                // Only a registered controller's complete view is authoritative.
                // A malformed replacement retracts controls instead of keeping
                // an earlier activation or inferring capability from the provider.
                self.native_view = record
                    .data
                    .get("update")
                    .and_then(|update| {
                        <phux_protocol::native_agent::NativeUpdate as serde::Deserialize<'_>>::deserialize(update).ok()
                    })
                    .and_then(|update| match update {
                        phux_protocol::native_agent::NativeUpdate::View { view }
                            if !view.activation.is_empty()
                                && view.questions.len()
                                    <= phux_protocol::native_agent::MAX_NATIVE_QUESTIONS =>
                        {
                            Some(view)
                        }
                        _ => None,
                    });
            }
        }
        self.log.append(records);
    }

    fn reject_retired<E>(
        &self,
        resource_id: &ResourceId,
        generation: GenerationId,
    ) -> Result<(), KernelError<E>> {
        if self.retired.contains_key(&generation) {
            Err(retired_error(resource_id, generation))
        } else {
            Ok(())
        }
    }

    fn published_matches(&self, generation: GenerationId) -> bool {
        self.published
            .as_ref()
            .is_some_and(|published| generation_of(&published.key) == generation)
    }

    fn staging_matches(&self, generation: GenerationId) -> bool {
        self.staging
            .as_ref()
            .is_some_and(|staging| generation_of(&staging.key) == generation)
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
        self.retired.insert(
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
}

const fn chunk_admission<E>(
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

const fn staging_lifetime_exceeded(started_at_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(started_at_ms) > MAX_BOOTSTRAP_STAGING_LIFETIME_MS
}

/// Agent envelopes carry the final record sequence, not a frame counter.
/// Validate the whole run before mutating the log, derived state, or cursor.
fn validate_batch<E>(
    records: &[AgentEventRecord],
    mut expected: u64,
    envelope_seq: u64,
) -> Result<(), KernelError<E>> {
    let (last, preceding) = records.split_last().ok_or(KernelError::EmptyAgentBatch)?;
    for record in preceding {
        require_sequence(expected, record.seq)?;
        expected = expected
            .checked_add(1)
            .ok_or(KernelError::SequenceExhausted)?;
    }
    require_sequence(expected, last.seq)?;
    require_sequence(last.seq, envelope_seq)
}

const fn require_sequence<E>(expected: u64, actual: u64) -> Result<(), KernelError<E>> {
    if actual < expected {
        return Err(KernelError::DuplicateSequence { expected, actual });
    }
    if actual > expected {
        return Err(KernelError::SequenceGap { expected, actual });
    }
    Ok(())
}
