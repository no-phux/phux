//! Agent-event folding and topology-driven terminal closure.

use super::{AgentEvent, ControlError, ControlPlane, EngineEvent, Event, ResourceId, ResourceKind};

impl ControlPlane {
    pub(super) fn agent_event(
        &mut self,
        terminal: Option<ResourceId>,
        event: AgentEvent,
    ) -> Result<(), ControlError> {
        // Server-scoped events carry no terminal; the server always scopes
        // the kinds this build folds.
        let Some(terminal_id) = terminal else {
            return Ok(());
        };
        if let AgentEvent::ResourceSpawned {
            kind: ResourceKind::Terminal,
            ..
        } = &event
            && self.options.auto_attach_foreign_spawns
        {
            self.pick_up_foreign_spawn(&terminal_id);
        }
        // The kernel folds the process-exit bookkeeping (a retained
        // resource, ADR-0124) for terminals it knows; every other kind is
        // projected here directly.
        if self.kernel_knows(&terminal_id)
            && let Some(engine) = &self.engine
        {
            match engine.apply(EngineEvent::Agent {
                terminal_id: terminal_id.clone(),
                event: event.clone(),
            }) {
                Ok(outcome) => self.process_outcome(outcome, false)?,
                Err(error) => return Err(ControlError::Protocol(error.to_string())),
            }
        }
        self.fold_agent_event(terminal_id, event);
        Ok(())
    }

    pub(super) fn kernel_knows(&self, terminal_id: &ResourceId) -> bool {
        self.attach_terminals.contains(terminal_id)
            || self.own_spawns.contains(terminal_id)
            || self.terminal_attached.contains(terminal_id)
            || self.agent_streams.contains(terminal_id)
    }

    /// A terminal spawned by another client has no output pump on this
    /// connection: attach it on the live socket and refresh the topology
    /// so it gets its window and session context.
    pub(super) fn pick_up_foreign_spawn(&mut self, terminal_id: &ResourceId) {
        let already_admitted = self.own_spawns.contains(terminal_id)
            || self.terminal_attached.contains(terminal_id)
            || self.engine.as_ref().is_some_and(|engine| {
                engine.has_projection(terminal_id) || engine.is_closed(terminal_id)
            });
        if already_admitted {
            return;
        }
        self.attach_terminal(terminal_id);
        self.queue_refresh_topology();
    }

    pub(super) fn fold_agent_event(&mut self, terminal_id: ResourceId, event: AgentEvent) {
        match event {
            AgentEvent::Bell => self.push_event(Event::Bell { terminal_id }),
            AgentEvent::TitleChanged { title } => {
                if let Some(pane) = self
                    .topology
                    .as_mut()
                    .and_then(|topology| topology.pane_mut(&terminal_id))
                {
                    pane.title = Some(title.clone());
                }
                self.push_event(Event::TitleChanged { terminal_id, title });
            }
            AgentEvent::ResourceSpawned {
                kind: ResourceKind::Terminal,
                ..
            } => self.push_event(Event::PaneSpawned { terminal_id }),
            AgentEvent::ResourceClosed { exit_status } => {
                self.close_pane(
                    &terminal_id,
                    exit_status,
                    None,
                    phux_protocol::wire::frame::CloseReason::Unknown,
                );
            }
            AgentEvent::Dirty => self.push_event(Event::OutputStarted { terminal_id }),
            AgentEvent::Idle => self.push_event(Event::OutputSettled { terminal_id }),
            AgentEvent::Asked {
                id,
                question,
                suggestions,
                elapsed_seconds,
            } => self.push_event(Event::AgentAsked {
                terminal_id,
                question_id: id,
                text: question,
                suggestions,
                waiting_seconds: elapsed_seconds,
            }),
            AgentEvent::CommandStarted => self.push_event(Event::CommandStarted { terminal_id }),
            AgentEvent::CommandFinished { exit_code } => self.push_event(Event::CommandFinished {
                terminal_id,
                exit_code,
            }),
            AgentEvent::CwdChanged { cwd } => {
                if let Some(pane) = self
                    .topology
                    .as_mut()
                    .and_then(|topology| topology.pane_mut(&terminal_id))
                {
                    pane.cwd = Some(cwd.clone());
                }
                self.push_event(Event::CwdChanged { terminal_id, cwd });
            }
            // Supervisory, unknown, and non-Terminal spawns: forward-compat
            // skip.
            _ => {}
        }
    }

    /// Apply a terminal's closure to the topology and correlations.
    /// Idempotent: the first application removes the entry.
    pub(super) fn close_pane(
        &mut self,
        terminal_id: &ResourceId,
        exit_status: Option<i32>,
        signal: Option<i32>,
        reason: phux_protocol::wire::frame::CloseReason,
    ) -> bool {
        let was_known = self.own_spawns.contains(terminal_id)
            || self.terminal_attached.contains(terminal_id)
            || self
                .topology
                .as_ref()
                .is_some_and(|topology| topology.pane(terminal_id).is_some());
        self.terminal_attached.remove(terminal_id);
        self.stream_recoveries.remove(terminal_id);
        self.own_spawns.remove(terminal_id);
        self.agent_streams.remove(terminal_id);
        if let Some(topology) = self.topology.as_mut() {
            topology
                .panes
                .retain(|pane| &pane.terminal_id != terminal_id);
        }
        let reports = self
            .input_replay
            .retire_terminal(terminal_id, "terminal closed");
        self.publish_replay_reports(reports, None);
        if was_known {
            self.push_event(Event::TerminalClosed {
                terminal_id: terminal_id.clone(),
                exit_status,
                signal,
                reason,
            });
        }
        was_known
    }
}
