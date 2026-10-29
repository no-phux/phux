//! Read-side kernel accessors for resources, agents, replicas, and history.

use super::{
    AgentSessionView, AgentStream, AgentStreamInfo, BootstrapId, ClosedReplica, EffectBuffer,
    EngineAdapter, EnginePresentationAdapter, GenerationId, HistoryCache, InputBlockReason,
    InputEligibility, KernelEffect, KernelError, KernelSend, KernelStatus, PublishedReplica,
    ResourceId, ResourceKind, SessionKernel, StagingReplica, StreamId, TombstoneRecord,
    generation_of,
};

impl<E: EngineAdapter> SessionKernel<E> {
    /// The kind of one resource this kernel has seen, closed ones included.
    #[must_use]
    pub fn resource_kind(&self, terminal_id: &ResourceId) -> Option<ResourceKind> {
        self.kinds.get(terminal_id).copied()
    }

    /// Borrow one open `AgentSession` resource.
    #[must_use]
    pub fn agent_session(&self, terminal_id: &ResourceId) -> Option<AgentSessionView<'_>> {
        let (terminal_id, stream) = self.agents.get_key_value(terminal_id)?;
        Some(Self::agent_view(terminal_id, stream))
    }

    /// The committed SDK controller view for this exact `AgentSession`.
    ///
    /// Only reserved `phux.native-agent/v1` full-view records establish
    /// native controls; provider names and terminal declarations never do.
    /// The folded view survives record-log eviction and is replaced atomically
    /// with a new bootstrap generation.
    /// Resource or parent closure makes it disconnected; the last source view
    /// remains readable until explicit release or incarnation replacement.
    #[must_use]
    pub fn native_agent_view(
        &self,
        agent_session_id: &ResourceId,
    ) -> Option<phux_protocol::native_agent::NativeView> {
        let Some(stream) = self.agents.get(agent_session_id) else {
            return self.closed_native_views.get(agent_session_id).cloned();
        };
        let mut view = stream.native_view()?.clone();
        if stream
            .parent()
            .is_some_and(|parent| self.closed.contains(parent))
        {
            view.connected = false;
            view.questions.clear();
            view.turn_id = None;
        }
        Some(view)
    }

    /// Whether this session retains a native projection, without cloning it.
    /// Presence identifies native ownership, not readiness to accept commands.
    #[must_use]
    pub fn has_native_agent_view(&self, agent_session_id: &ResourceId) -> bool {
        self.agents.get(agent_session_id).map_or_else(
            || self.closed_native_views.contains_key(agent_session_id),
            |stream| stream.native_view().is_some(),
        )
    }

    /// Identity and sequence cursor of an agent's committed generation.
    #[must_use]
    pub fn agent_stream_info(&self, terminal_id: &ResourceId) -> Option<AgentStreamInfo> {
        self.agents.get(terminal_id)?.committed()
    }

    /// Every open `AgentSession` resource, in no particular order.
    pub fn agent_sessions(&self) -> impl Iterator<Item = AgentSessionView<'_>> + '_ {
        self.agents
            .iter()
            .map(|(terminal_id, stream)| Self::agent_view(terminal_id, stream))
    }

    const fn agent_view<'a>(
        terminal_id: &'a ResourceId,
        stream: &'a AgentStream,
    ) -> AgentSessionView<'a> {
        AgentSessionView {
            terminal_id,
            parent: stream.parent(),
            state: stream.state(),
            log: stream.log(),
            published: stream.is_published(),
        }
    }

    /// Borrow the published replica for one terminal.
    #[must_use]
    pub fn published(&self, terminal_id: &ResourceId) -> Option<PublishedReplica<'_, E>> {
        let replica = self.terminals.get(terminal_id)?.published.as_ref()?;
        Some(PublishedReplica {
            key: &replica.key,
            geometry: replica.geometry,
            last_seq: replica.last_seq,
            engine: &replica.engine,
            history: &replica.history,
        })
    }
    /// Borrow the published engine replica directly for frontend projection.
    #[must_use]
    pub fn published_engine(&self, terminal_id: &ResourceId) -> Option<&E::Replica> {
        Some(&self.terminals.get(terminal_id)?.published.as_ref()?.engine)
    }

    /// Mutably borrow a published engine for frontend-local controlled operations.
    ///
    /// Wire bootstrap, history, and live bytes must still enter through
    /// [`Self::update`]. This is only for adapter-owned viewport controls that
    /// do not alter protocol sequencing.
    #[must_use]
    pub fn published_engine_mut(&mut self, terminal_id: &ResourceId) -> Option<&mut E::Replica> {
        Some(
            &mut self
                .terminals
                .get_mut(terminal_id)?
                .published
                .as_mut()?
                .engine,
        )
    }

    /// Choose whether the next close retains this terminal's published replica.
    ///
    /// Retention is opt-in so clients that do not expose finished-pane history
    /// keep the kernel's original immediate-drop behavior. The preference is
    /// consumed by close and cleared by release.
    pub fn set_retain_replica_on_close(&mut self, terminal_id: &ResourceId, retain: bool) {
        if retain {
            self.retain_replica_on_close.insert(terminal_id.clone());
        } else {
            self.retain_replica_on_close.remove(terminal_id);
        }
    }

    /// Transfer the final published replica retained by a close transition.
    ///
    /// Returns `None` when the terminal closed before publication, the final
    /// replica was already taken, or the resource is not closed. Taking the
    /// value does not reopen the terminal or alter close/damage semantics.
    #[must_use]
    pub fn take_closed_replica(&mut self, terminal_id: &ResourceId) -> Option<ClosedReplica<E>> {
        self.closed_replicas.remove(terminal_id)
    }

    /// Clear only this client's presentation. The published generation, parser
    /// and live sequence survive; in-flight older pages can no longer reappear.
    pub fn clear_presentation(
        &mut self,
        terminal_id: &ResourceId,
    ) -> Result<(), KernelError<E::Error>>
    where
        E: EnginePresentationAdapter,
    {
        self.ensure_open(terminal_id)?;
        let replica = self
            .terminals
            .get_mut(terminal_id)
            .and_then(|state| state.published.as_mut())
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        self.adapter
            .clear_presentation(&mut replica.engine)
            .map_err(KernelError::Engine)?;
        replica.history.clear_presentation();
        Ok(())
    }

    /// Borrow one published generation's progressive history cache.
    #[must_use]
    pub fn history_cache(&self, terminal_id: &ResourceId) -> Option<&HistoryCache> {
        Some(&self.terminals.get(terminal_id)?.published.as_ref()?.history)
    }

    /// Request the next older page when a pinned viewport nears the cache edge.
    ///
    /// Returns `true` only when a request was emitted. Repeated calls are
    /// idempotent while that exact cursor is in flight.
    pub fn prefetch_history(
        &mut self,
        terminal_id: &ResourceId,
        rows_from_oldest: usize,
        effects: &mut EffectBuffer,
    ) -> bool {
        let Some(state) = self.terminals.get_mut(terminal_id) else {
            return false;
        };
        let Some(replica) = state.published.as_mut() else {
            return false;
        };
        if state.retired.contains_key(&generation_of(&replica.key)) {
            return false;
        }
        if !replica.history.should_prefetch(rows_from_oldest) {
            return false;
        }
        let Some(cursor) = replica.history.begin_fetch() else {
            return false;
        };
        effects.push(KernelEffect::Send(KernelSend::HistoryRequest {
            key: replica.key.clone(),
            cursor: cursor.as_bytes().to_vec(),
            max_bytes: self.history_config.request_max_bytes,
            max_rows: self.history_config.request_max_rows,
        }));
        effects.push(KernelEffect::Status(KernelStatus::History {
            key: replica.key.clone(),
            status: replica.history.status(),
        }));
        true
    }

    /// Borrow the unpublished staging replica for one terminal.
    #[must_use]
    pub fn staging(&self, terminal_id: &ResourceId) -> Option<StagingReplica<'_, E>> {
        let staging = self.terminals.get(terminal_id)?.staging.as_ref()?;
        Some(StagingReplica {
            key: &staging.key,
            geometry: staging.geometry,
            engine_ready: staging.engine_ready,
            protocol_ready: staging.protocol_ready,
            engine: &staging.engine,
        })
    }

    /// Return a retained tombstone, if this exact generation is retired.
    #[must_use]
    pub fn tombstone(
        &self,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
    ) -> Option<TombstoneRecord> {
        self.terminals
            .get(terminal_id)?
            .retired
            .get(&GenerationId {
                stream_id,
                bootstrap_id,
            })
            .copied()
    }

    /// Compute explicit input eligibility without producing effects.
    #[must_use]
    pub fn input_eligibility(&self, terminal_id: &ResourceId) -> InputEligibility {
        if self.closed.contains(terminal_id) {
            return InputEligibility::Ineligible(InputBlockReason::Closed);
        }
        if self
            .kinds
            .get(terminal_id)
            .is_some_and(|kind| *kind != ResourceKind::Terminal)
        {
            return InputEligibility::Ineligible(InputBlockReason::NotATerminal);
        }
        let Some(state) = self.terminals.get(terminal_id) else {
            return InputEligibility::Ineligible(InputBlockReason::UnknownTerminal);
        };
        let Some(replica) = state.published.as_ref() else {
            return InputEligibility::Ineligible(InputBlockReason::AwaitingReplica);
        };
        if state.retired.contains_key(&generation_of(&replica.key)) {
            return InputEligibility::Ineligible(InputBlockReason::FrozenReplica);
        }
        if self.attach_blocks(terminal_id) {
            return InputEligibility::Ineligible(InputBlockReason::AwaitingAttachReady);
        }
        InputEligibility::Eligible {
            stream_id: replica.key.stream_id,
            bootstrap_id: replica.key.bootstrap_id,
        }
    }
}
