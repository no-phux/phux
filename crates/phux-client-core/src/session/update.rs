//! Input dispatch: every stream frame reaches its lane through one function.

#[cfg(doc)]
use super::KernelStatus;
use super::lane::{
    AttachSignal, BeginFrame, ChunkFrame, KindLane, LaneCx, LaneKind, LaneRails, OutputFrame,
    ReadyFrame, ResourceLane, StreamFrame,
};
use super::{
    AgentSessionDeclaration, BootstrapStreamProfile, EffectBuffer, EngineAdapter, GenerationId,
    HistoryPageInput, KernelEffect, KernelError, KernelInput, MAX_BOOTSTRAP_STAGING_BYTES,
    ResourceId, ResourceKind, SessionKernel, TombstoneRecord,
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
        self.check_deadlines(&input, effects);
        if self.is_refused_stream_frame(&input) {
            return Ok(());
        }
        match input {
            KernelInput::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::Unknown { codec_tag },
                base_seq,
                ..
            } => self.refuse_unknown_codec(
                terminal_id,
                GenerationId {
                    stream_id,
                    bootstrap_id,
                },
                codec_tag,
                base_seq,
                effects,
            ),
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
                self.lift_refusal(terminal_id);
                let frame = BeginFrame {
                    generation: GenerationId {
                        stream_id,
                        bootstrap_id,
                    },
                    profile,
                    geometry,
                    base_seq,
                };
                self.stream_frame(terminal_id, StreamFrame::Begin(frame), effects)
            }
            KernelInput::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } => {
                let frame = ChunkFrame {
                    generation: GenerationId {
                        stream_id,
                        bootstrap_id,
                    },
                    chunk_seq,
                    payload,
                    usage_allowed: false,
                };
                self.stream_frame(terminal_id, StreamFrame::Chunk(frame), effects)
            }
            KernelInput::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor,
            } => {
                let frame = ReadyFrame {
                    generation: GenerationId {
                        stream_id,
                        bootstrap_id,
                    },
                    history_cursor,
                };
                self.stream_frame(terminal_id, StreamFrame::Ready(frame), effects)
            }
            KernelInput::ResourceOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                payload,
            } => {
                let frame = OutputFrame {
                    generation: GenerationId {
                        stream_id,
                        bootstrap_id,
                    },
                    seq,
                    payload,
                };
                self.stream_frame(terminal_id, StreamFrame::Output(frame), effects)
            }
            KernelInput::Tombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            } => {
                let signal = self.lanes.fence_slot(terminal_id).on_tombstone(
                    GenerationId {
                        stream_id,
                        bootstrap_id,
                    },
                    TombstoneRecord {
                        reason,
                        last_valid_seq,
                    },
                );
                self.apply_attach_signal(terminal_id, signal);
                Ok(())
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
                if self.lanes.terminal(terminal_id).is_some()
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

    /// The one route from a stream frame to its resource's lane. A begin
    /// opens (or keeps) the lane its profile names; every other frame needs
    /// a lane already. Connection-wide rails run here, around the lane: the
    /// closed set, the staging ceilings, and the ATTACH barrier.
    fn stream_frame(
        &mut self,
        terminal_id: &ResourceId,
        mut frame: StreamFrame<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        let opens = match &mut frame {
            StreamFrame::Begin(begin) => {
                let kind = LaneKind::of_profile(begin.profile);
                self.expect_kind(terminal_id, kind.resource_kind())?;
                kind.admit_begin(self.selected_profile, begin)?;
                Some(kind)
            }
            StreamFrame::Chunk(chunk) => {
                chunk.usage_allowed = self
                    .next_bootstrap_staging_usage(chunk.payload.len())
                    .is_some();
                None
            }
            StreamFrame::Ready(_) | StreamFrame::Output(_) => None,
        };
        // Live output reads no clock, and only a publishing frame can damage.
        let now_ms = if matches!(frame, StreamFrame::Output(_)) {
            0
        } else {
            self.monotonic_now_ms()
        };
        let attach_blocks = matches!(frame, StreamFrame::Ready(_) | StreamFrame::Output(_))
            && self.attach_blocks(terminal_id);
        // One lookup per frame: a begin's `open` is its lane.
        let lane = match opens {
            Some(kind) => self.lanes.open(terminal_id, kind),
            None => self
                .lanes
                .get_mut(terminal_id)
                .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?,
        };
        let mut cx = LaneCx {
            resource_id: terminal_id,
            adapter: &mut self.adapter,
            engine_effects: &mut self.engine_effects,
            history_config: self.history_config,
            perf_echo: &self.perf_echo,
            now_ms,
            attach_blocks,
            attach: AttachSignal::Unchanged,
        };
        let result = match &frame {
            StreamFrame::Begin(begin) => lane.on_begin(&mut cx, begin, effects),
            StreamFrame::Chunk(chunk) => lane.on_chunk(&mut cx, chunk, effects),
            StreamFrame::Ready(ready) => lane.on_ready(&mut cx, ready, effects),
            StreamFrame::Output(output) => lane.on_output(&mut cx, output, effects),
        };
        let signal = cx.attach;
        self.deadline_watch |= opens.is_some();
        self.apply_attach_signal(terminal_id, signal);
        result?;
        if let StreamFrame::Chunk(chunk) = &frame
            && self.bootstrap_retained_bytes() > MAX_BOOTSTRAP_STAGING_BYTES
        {
            self.retire_staging_limit(terminal_id, chunk.generation, effects);
            return Err(KernelError::BootstrapStagingLimitExceeded);
        }
        Ok(())
    }

    fn apply_attach_signal(&mut self, terminal_id: &ResourceId, signal: AttachSignal) {
        match signal {
            AttachSignal::Unchanged => {}
            AttachSignal::Resolved => self.mark_attach_resolved(terminal_id),
            AttachSignal::Unresolved { pending_removal } => {
                self.mark_attach_unresolved(terminal_id, pending_removal);
            }
        }
    }

    /// Every lane's accepted staging plus one more chunk of `payload_len`,
    /// if that fits the connection-wide ceilings.
    pub(super) fn next_bootstrap_staging_usage(
        &self,
        payload_len: usize,
    ) -> Option<(usize, usize)> {
        let mut bytes = payload_len;
        let mut chunks = 1_usize;
        for (accepted_bytes, accepted_chunks) in
            self.lanes.values().filter_map(LaneRails::staged_accounting)
        {
            bytes = bytes.checked_add(accepted_bytes)?;
            chunks = chunks.checked_add(accepted_chunks)?;
        }
        super::staging_rules::within_staging_ceilings(bytes, chunks).then_some((bytes, chunks))
    }

    /// Staged bytes every lane retains, engine state included.
    fn bootstrap_retained_bytes(&self) -> usize {
        self.lanes.values().fold(0_usize, |total, lane| {
            total.saturating_add(lane.retained_staging_bytes(&self.adapter))
        })
    }

    pub(super) fn retire_staging_limit(
        &mut self,
        terminal_id: &ResourceId,
        generation: GenerationId,
        effects: &mut EffectBuffer,
    ) {
        if let Some(lane) = self.lanes.get_mut(terminal_id) {
            lane.retire_for_limit(terminal_id, generation, effects);
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
        if let ResourceLane::Agent(stream) = self.lanes.open(terminal_id, LaneKind::Agent) {
            stream.declare(declaration);
        }
        Ok(())
    }
}
