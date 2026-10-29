//! Input dispatch and `AgentSession` stream handling.

#[cfg(doc)]
use super::KernelStatus;
use super::{
    AgentSessionDeclaration, AgentStream, BootstrapId, BootstrapStreamProfile, EffectBuffer,
    EngineAdapter, GenerationId, HistoryPageInput, KernelEffect, KernelError, KernelInput,
    ResourceId, ResourceKind, SessionKernel, StreamId, TombstoneRecord, is_agent_profile,
};

impl<E: EngineAdapter> SessionKernel<E> {
    /// Apply one normalized input and replace `effects` with its declarative result.
    ///
    /// An adapter error retires the possibly mutated generation before
    /// returning and leaves a [`KernelStatus::ResyncRequired`] effect in the
    /// buffer. The host must execute effects even when this method returns an
    /// error.
    #[allow(clippy::too_many_lines)]
    pub fn update(
        &mut self,
        input: KernelInput<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        effects.clear();
        match input {
            KernelInput::AttachStarted {
                attach_id,
                terminals,
            } => self.attach_started(attach_id, terminals),
            KernelInput::AttachReady { attach_id } => self.attach_ready(attach_id, effects),
            KernelInput::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                geometry,
                base_seq,
            } => {
                if is_agent_profile(profile) {
                    self.agent_bootstrap_begin(
                        terminal_id,
                        stream_id,
                        bootstrap_id,
                        profile,
                        base_seq,
                    )
                } else {
                    self.expect_kind(terminal_id, ResourceKind::Terminal)?;
                    self.bootstrap_begin(
                        terminal_id,
                        stream_id,
                        bootstrap_id,
                        profile,
                        geometry,
                        base_seq,
                        effects,
                    )
                }
            }
            KernelInput::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } => {
                if self.agents.contains_key(terminal_id) {
                    self.agent_bootstrap_chunk(
                        terminal_id,
                        stream_id,
                        bootstrap_id,
                        chunk_seq,
                        payload,
                        effects,
                    )
                } else {
                    self.bootstrap_chunk(
                        terminal_id,
                        stream_id,
                        bootstrap_id,
                        chunk_seq,
                        payload,
                        effects,
                    )
                }
            }
            KernelInput::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor,
            } => {
                if self.agents.contains_key(terminal_id) {
                    self.agent_bootstrap_ready(terminal_id, stream_id, bootstrap_id, effects)
                } else {
                    self.bootstrap_ready(
                        terminal_id,
                        stream_id,
                        bootstrap_id,
                        history_cursor,
                        effects,
                    )
                }
            }
            KernelInput::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                page_seq,
                rows,
                payload,
                cursor,
                next_cursor,
            } => self.history_page(
                &HistoryPageInput {
                    terminal_id,
                    generation: GenerationId {
                        stream_id,
                        bootstrap_id,
                    },
                    page_seq,
                    rows,
                    payload,
                    cursor,
                    next_cursor,
                },
                effects,
            ),
            KernelInput::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
            } => self.history_tombstone(
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
                effects,
            ),
            KernelInput::HistoryRejected {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
                required_bytes,
                required_rows,
            } => self.history_rejected(
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
                required_bytes,
                required_rows,
                effects,
            ),
            KernelInput::ResourceOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                payload,
            } if self.agents.contains_key(terminal_id) => {
                self.agent_output(terminal_id, stream_id, bootstrap_id, seq, payload, effects)
            }
            KernelInput::ResourceOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                payload,
            } => {
                crate::perf::OUTPUT_FRAMES.incr();
                crate::perf::OUTPUT_BYTES.add_len(payload.len());
                self.perf_echo.observe(terminal_id);
                let _timed = crate::perf::apply_timer();
                self.terminal_output(terminal_id, stream_id, bootstrap_id, seq, payload, effects)
            }
            KernelInput::Tombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            } => {
                self.tombstone_generation(
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    TombstoneRecord {
                        reason,
                        last_valid_seq,
                    },
                );
                Ok(())
            }
            KernelInput::ResourceClosed {
                terminal_id,
                exit_status,
                signal,
                reason,
            } => {
                self.terminal_closed(terminal_id, exit_status, signal, reason, effects);
                Ok(())
            }
            KernelInput::Event { terminal_id, event } => {
                // Untracked or already-closed terminals get no status.
                if self.terminals.contains_key(terminal_id)
                    && !self.closed.contains(terminal_id)
                    && let Some(status) = self.status_for_agent_event(terminal_id, event)
                {
                    effects.push(KernelEffect::Status(status));
                }
                Ok(())
            }
            KernelInput::AgentSessionDeclared(declaration) => {
                self.agent_session_declared(&declaration)
            }
            KernelInput::Action(action) => self.action(&action, effects),
        }
    }

    /// Record `kind` for a resource, or reject a frame that implies a kind
    /// other than the one already known.
    pub(super) fn expect_kind(
        &mut self,
        terminal_id: &ResourceId,
        kind: ResourceKind,
    ) -> Result<(), KernelError<E::Error>> {
        match self.kinds.get(terminal_id) {
            Some(declared) if *declared != kind => Err(KernelError::KindMismatch {
                terminal_id: terminal_id.clone(),
                declared: *declared,
                incoming: kind,
            }),
            Some(_) => Ok(()),
            None => {
                self.kinds.insert(terminal_id.clone(), kind);
                Ok(())
            }
        }
    }

    fn agent_session_declared(
        &mut self,
        declaration: &AgentSessionDeclaration<'_>,
    ) -> Result<(), KernelError<E::Error>> {
        let terminal_id = declaration.terminal_id;
        self.ensure_open(terminal_id)?;
        self.expect_kind(terminal_id, ResourceKind::AgentSession)?;
        self.agents
            .entry(terminal_id.clone())
            .or_insert_with(AgentStream::new)
            .declare(declaration);
        Ok(())
    }

    fn agent_bootstrap_begin(
        &mut self,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        profile: BootstrapStreamProfile,
        base_seq: u64,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        self.expect_kind(terminal_id, ResourceKind::AgentSession)?;
        let now_ms = self.monotonic_now_ms();
        self.agents
            .entry(terminal_id.clone())
            .or_insert_with(AgentStream::new)
            .begin(
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                base_seq,
                now_ms,
            )
    }

    fn agent_bootstrap_chunk(
        &mut self,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        chunk_seq: u32,
        payload: &[u8],
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let usage_allowed = self.next_bootstrap_staging_usage(payload.len()).is_some();
        let now_ms = self.monotonic_now_ms();
        self.agents
            .get_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?
            .accept_chunk(
                terminal_id,
                generation,
                chunk_seq,
                payload,
                usage_allowed,
                now_ms,
                effects,
            )
    }

    fn agent_bootstrap_ready(
        &mut self,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let now_ms = self.monotonic_now_ms();
        let publication = self
            .agents
            .get_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?
            .commit_ready(terminal_id, generation, now_ms, effects)?;
        effects.push(KernelEffect::AgentPublication(publication));
        self.mark_attach_resolved(terminal_id);
        Ok(())
    }

    fn agent_output(
        &mut self,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        seq: u64,
        payload: &[u8],
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let publication = self
            .agents
            .get_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?
            .commit_live(terminal_id, generation, seq, payload, effects)?;
        effects.push(KernelEffect::AgentPublication(publication));
        Ok(())
    }
}
