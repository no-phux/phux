//! Relaying satellite stream frames: events, output, lease mirrors, closes, and bells.

use super::{
    AgentEvent, ClientId, ControlAction, FrameKind, RelaySession, ResourceId, ServerFeature,
    foreign_client, link_journal_gap, satellite_gap_as_source_gap, stream_frame_scope,
    stream_frame_scope_mut, warn,
};

impl RelaySession {
    /// Forward one terminal-scoped return-leg frame: check its flow gate,
    /// re-tag, fan out.
    pub(super) fn relay_stream_frame(&mut self, frame: FrameKind) -> Result<(), String> {
        if let Some(dropped) = link_journal_gap(&frame) {
            self.relay_link_gap(dropped);
            return Ok(());
        }
        let Some(id) = self.retag_inbound(stream_frame_scope(&frame)) else {
            return Ok(());
        };
        if !matches!(frame, FrameKind::Event { .. })
            && self
                .pending_detaches
                .values()
                .any(|pending| pending.terminal == id)
        {
            // Pre-detach frames; a fresh proxy must not take them as its prefix.
            return Ok(());
        }
        if self.enforce_bootstrap_flow {
            self.enforce_stream_frame_flow(id, &frame)?;
        }
        let retagged = self.retag_stream_frame(frame, id);
        self.deliver_stream_frame(id, retagged);
        Ok(())
    }

    /// Deliver one re-tagged frame. An `EVENT` is re-stamped by the hub's
    /// registry (ADR-0123, L1 §7.3) and reaches that Terminal's subscribers;
    /// without a journal it fans out with its stamp dropped.
    pub(super) fn deliver_stream_frame(&mut self, id: u32, frame: FrameKind) {
        let FrameKind::Event {
            terminal,
            mut event,
            stamp,
        } = frame
        else {
            self.fan_out(id, &frame);
            return;
        };
        if let AgentEvent::ResourceSpawned { kind, .. } = &event
            && let Some(catalog) = &self.resource_kinds
        {
            catalog
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .kinds
                .insert(id, *kind);
        }
        satellite_gap_as_source_gap(&mut event);
        self.mirror_satellite_lease_state(id, &event, stamp.as_deref().map(|s| s.seq));
        // L1 §9.1: a keyed operation's event names the consumer that sent
        // it, and only on a terminal that operation targeted.
        let (stamp, actor) = self.vet_event_stamp(id, stamp);
        let Some(event) = self.retag_event_ids(id, event, actor) else {
            return;
        };
        if let (Some(journal), Some(scope)) = (self.journal.clone(), terminal.clone()) {
            let _ = journal
                .with_mut(|s| s.record_relayed_event_as(scope, event, stamp.as_ref(), actor));
            return;
        }
        let unstamped = FrameKind::Event {
            terminal,
            event,
            stamp: None,
        };
        self.fan_out(id, &unstamped);
    }

    /// The satellite's stamp with every id the hub cannot vouch for removed,
    /// and the hub consumer the event belongs to. An `operation_id` a hub
    /// consumer forwarded for another terminal is dropped (the satellite may
    /// not claim it here); one this link never forwarded is kept but names no
    /// hub consumer; the satellite's own `actor` names its connections, never
    /// the hub's, and never survives.
    pub(super) fn vet_event_stamp(
        &self,
        terminal: u32,
        stamp: Option<Box<phux_protocol::wire::frame::EventStamp>>,
    ) -> (
        Option<phux_protocol::wire::frame::EventStamp>,
        Option<ClientId>,
    ) {
        use crate::hub::operation_fence::EventOperation;
        let Some(mut stamp) = stamp.map(|stamp| *stamp) else {
            return (None, None);
        };
        stamp.actor = None;
        let verdict = stamp
            .operation_id
            .map(|key| self.operations.event_operation(&key, terminal));
        let actor = match verdict {
            Some(EventOperation::Forwarded(actor)) => Some(actor),
            Some(EventOperation::Misattributed) => {
                warn!(
                    satellite = %self.host,
                    terminal,
                    "satellite stamped a forwarded operation id on another terminal's event; dropping the id"
                );
                stamp.operation_id = None;
                None
            }
            Some(EventOperation::Unknown) | None => None,
        };
        (Some(stamp), actor)
    }

    /// Re-tag the ids inside a satellite event into the hub's id space
    /// (L1 §9.1), or `None` to drop an event naming an id the hub cannot
    /// represent: a resource parent is re-tagged `Satellite { host, id }`
    /// (a chained one drops the event); a client id becomes the hub
    /// consumer the hub knows for it (the lease holder, the operation's
    /// sender) or a [`foreign_client`] id; an approval event naming one of
    /// the hub's own pending approvals is dropped, since approval ids share
    /// one namespace and the satellite's must never decide or shadow the
    /// hub's.
    pub(super) fn retag_event_ids(
        &self,
        terminal: u32,
        event: AgentEvent,
        operation_actor: Option<ClientId>,
    ) -> Option<AgentEvent> {
        match event {
            AgentEvent::ResourceSpawned { kind, parent } => {
                let parent = match parent {
                    None => None,
                    Some(ResourceId::Local { id }) => {
                        Some(ResourceId::satellite(self.host.clone(), id))
                    }
                    Some(ResourceId::Satellite { .. }) => {
                        warn!(
                            satellite = %self.host,
                            terminal,
                            "satellite reported a Satellite-tagged parent; hub-and-spoke does not chain — dropping"
                        );
                        return None;
                    }
                };
                Some(AgentEvent::ResourceSpawned { kind, parent })
            }
            AgentEvent::TerminalControl {
                lifecycle,
                exit_status,
                input_holder,
                action,
                actor,
            } => {
                let holder = self.journal.as_ref().and_then(|journal| {
                    journal.with(|s| s.satellite_lease_holder(&self.host, terminal))
                });
                Some(AgentEvent::TerminalControl {
                    lifecycle,
                    exit_status,
                    input_holder: input_holder.map(|id| {
                        holder.map_or_else(|| foreign_client(id), crate::runtime::wire_client)
                    }),
                    action,
                    actor: actor.map(|id| {
                        operation_actor
                            .map_or_else(|| foreign_client(id), crate::runtime::wire_client)
                    }),
                })
            }
            // The origin label stays on the server that saw the key. Relaying
            // it would publish a client name across the satellite link.
            AgentEvent::InputActivity { .. } => None,
            AgentEvent::ApprovalRequested { id } | AgentEvent::ApprovalDecided { id, .. }
                if self.names_hub_approval(id) =>
            {
                warn!(
                    satellite = %self.host,
                    terminal,
                    "satellite event named one of the hub's pending approvals; dropping"
                );
                None
            }
            other => Some(other),
        }
    }

    /// Whether `id` is one of the hub's own pending approvals.
    pub(super) fn names_hub_approval(&self, id: phux_protocol::ids::ApprovalId) -> bool {
        self.journal
            .as_ref()
            .is_some_and(|journal| journal.with(|s| s.pending_approval(id).is_some()))
    }

    /// Mirror a satellite lease transition into the hub's ledger (ADR-0033):
    /// the satellite owns the timer, the hub reflects it.
    ///
    /// Acquired/Seized never change the holder by themselves (the hub's own
    /// `ACQUIRE_INPUT` reply installs it) but clear a pending mark and record
    /// `seq`. Released/Expired may evict, except while an acquire reply is
    /// pending or when `seq` is not newer: reply and event share the link but
    /// are not ordered against each other.
    pub(super) fn mirror_satellite_lease_state(
        &self,
        id: u32,
        event: &AgentEvent,
        seq: Option<u64>,
    ) {
        let Some(journal) = &self.journal else {
            return;
        };
        let is_end = match event {
            AgentEvent::TerminalControl {
                action: ControlAction::Acquired | ControlAction::Seized,
                ..
            } => false,
            AgentEvent::TerminalControl {
                action: ControlAction::Expired | ControlAction::Released,
                ..
            } => true,
            _ => return,
        };
        journal.with_mut(|s| s.mirror_satellite_lease_event(&self.host, id, is_end, seq));
    }

    /// Report a satellite's link-level `journal_gap` as a `source_gap` to
    /// every relayed Terminal's consumers. `dropped` is an upper bound.
    pub(super) fn relay_link_gap(&mut self, dropped: u64) {
        let terminals: Vec<u32> = self.subscribers.keys().copied().collect();
        for id in terminals {
            let gap = FrameKind::Event {
                terminal: Some(ResourceId::satellite(self.host.clone(), id)),
                event: AgentEvent::SourceGap { dropped },
                stamp: None,
            };
            self.deliver_stream_frame(id, gap);
        }
    }

    /// The cursor for the link's own `SUBSCRIBE_EVENTS`: journal semantics
    /// with no replay when the satellite speaks them (L1 §7.1), else live-only.
    pub(super) fn link_event_cursor(&self) -> Option<u64> {
        self.satellite_features
            .contains(ServerFeature::EventJournal)
            .then_some(u64::MAX)
    }

    /// `forward`, with the link's event cursor when it subscribes events.
    pub(super) fn with_link_cursor(&self, forward: &FrameKind) -> FrameKind {
        let mut forward = forward.clone();
        if let FrameKind::SubscribeEvents { after_seq, .. } = &mut forward {
            *after_seq = self.link_event_cursor();
        }
        forward
    }

    /// The bootstrap-flow gate each stream frame must clear.
    pub(super) fn enforce_stream_frame_flow(
        &mut self,
        id: u32,
        frame: &FrameKind,
    ) -> Result<(), String> {
        match frame {
            FrameKind::ResourceOutput {
                stream_id,
                bootstrap_id,
                ..
            } => self.validate_ready_identity(id, *stream_id, *bootstrap_id, "RESOURCE_OUTPUT"),
            FrameKind::BootstrapBegin {
                stream_id,
                bootstrap_id,
                profile,
                ..
            } => self.begin_bootstrap_flow(id, *stream_id, *bootstrap_id, *profile),
            FrameKind::BootstrapChunk {
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
                ..
            } => self.accept_bootstrap_chunk(
                id,
                *stream_id,
                *bootstrap_id,
                *chunk_seq,
                payload.len(),
            ),
            FrameKind::BootstrapReady {
                stream_id,
                bootstrap_id,
                ..
            } => self.finish_bootstrap_flow(id, *stream_id, *bootstrap_id),
            FrameKind::HistoryPage {
                stream_id,
                bootstrap_id,
                ..
            } => self.validate_ready_identity(id, *stream_id, *bootstrap_id, "HISTORY_PAGE"),
            FrameKind::BootstrapTombstone {
                stream_id,
                bootstrap_id,
                ..
            } => {
                self.validate_known_identity(id, *stream_id, *bootstrap_id, "BOOTSTRAP_TOMBSTONE")?;
                self.retire_bootstrap_flow(id);
                Ok(())
            }
            FrameKind::HistoryTombstone {
                stream_id,
                bootstrap_id,
                ..
            } => self.validate_ready_identity(id, *stream_id, *bootstrap_id, "HISTORY_TOMBSTONE"),
            FrameKind::HistoryRejected {
                stream_id,
                bootstrap_id,
                ..
            } => self.validate_ready_identity(id, *stream_id, *bootstrap_id, "HISTORY_REJECTED"),
            _ => Ok(()),
        }
    }

    /// Re-tag a stream frame's scope `Local { id }` -> `Satellite { host,
    /// id }`, leaving every other field verbatim (ADR-0007). Only frames
    /// [`stream_frame_scope`] scoped reach here, so the scope slot exists.
    pub(super) fn retag_stream_frame(&self, mut frame: FrameKind, id: u32) -> FrameKind {
        if let Some(scope) = stream_frame_scope_mut(&mut frame) {
            *scope = ResourceId::satellite(self.host.clone(), id);
        }
        frame
    }

    /// Deliver `RESOURCE_CLOSED` with the satellite's reason unchanged
    /// (ADR-0104 §4), then reap what the terminal owned on this link.
    pub(super) fn relay_terminal_closed(
        &mut self,
        terminal_id: &ResourceId,
        exit_status: Option<i32>,
        reason: phux_protocol::wire::frame::CloseReason,
        signal: Option<i32>,
    ) {
        let Some(id) = self.retag_inbound(Some(terminal_id)) else {
            return;
        };
        if let Some(catalog) = &self.resource_kinds {
            catalog
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .kinds
                .remove(&id);
        }
        if let Some(subscribers) = self.subscribers.get_mut(&id) {
            for subscriber in subscribers {
                subscriber.retire_after_flush = true;
            }
        }
        // Past an AwaitingFirst gate but after retained output; subscribers
        // are removed once the close reaches them.
        self.fan_out_ungated(
            id,
            &FrameKind::ResourceClosed {
                terminal_id: ResourceId::satellite(self.host.clone(), id),
                exit_status,
                reason,
                signal,
            },
        );
        // The satellite already declared it gone: no upstream detach.
        self.orphaned_by_delivery.remove(&id);
        self.recalculate_retained_totals();
        self.retire_bootstrap_flow(id);
        self.explicit_content.remove(&id);
        self.legacy_events.remove(&id);
        self.forget_mirror(id);
    }

    /// Deliver one `BELL` side-channel notification.
    pub(super) fn relay_bell(&mut self, terminal_id: &ResourceId) {
        let Some(id) = self.retag_inbound(Some(terminal_id)) else {
            return;
        };
        // A bell is not in the snapshot, so it bypasses the gate rather than
        // being dropped.
        self.fan_out_ungated(
            id,
            &FrameKind::Bell {
                terminal_id: ResourceId::satellite(self.host.clone(), id),
            },
        );
    }
}
