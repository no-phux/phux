//! Agent-event folding, topology-driven terminal closure, and the runtime's
//! own `AgentSession` subscriptions.

use phux_protocol::wire::frame::{CloseReason, Command, FrameKind};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

use super::{
    AgentEvent, AgentSessionInfo, ControlError, ControlPlane, EngineEvent, Event, Pending,
    ResourceId, ResourceKind,
};

/// One `AgentSession` stream `subscribe_agent_sessions` follows.
#[derive(Debug)]
pub(super) struct AgentSubscription {
    /// What the catalog last said about it.
    info: AgentSessionInfo,
    /// The connection epoch its `ATTACH_RESOURCE` went out on; any other
    /// value (0 after a refusal) means it is not subscribed on this one.
    connection: u64,
}

fn session_info(resource: &ResourceInfo) -> AgentSessionInfo {
    let facet = resource.agent.as_ref();
    AgentSessionInfo {
        parent: resource.parent.clone(),
        provider: facet.map(|facet| facet.provider.clone()),
        native_id: facet.and_then(|facet| facet.native_id.clone()),
    }
}

impl ControlPlane {
    pub(super) fn agent_event(
        &mut self,
        terminal: Option<ResourceId>,
        event: AgentEvent,
    ) -> Result<(), ControlError> {
        self.observe_roster_event(&event);
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
        // The announcement names the parent but not the provider identity:
        // read the catalog, whose reconcile subscribes it.
        if let AgentEvent::ResourceSpawned {
            kind: ResourceKind::AgentSession,
            ..
        } = &event
            && self.subscribes_agent_sessions()
        {
            self.queue_refresh_topology();
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
            } => {
                self.live_asks.insert(terminal_id.clone(), id.clone());
                self.push_event(Event::AgentAsked {
                    terminal_id,
                    question_id: id,
                    text: question,
                    suggestions,
                    waiting_seconds: elapsed_seconds,
                });
            }
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
            // Every broadcast restates the holder (ADR-0033), so a consumer
            // never has to infer the wheel from dropped input. The exit it
            // may also carry is the kernel's to report.
            AgentEvent::TerminalControl {
                input_holder,
                action,
                ..
            } => self.push_event(Event::InputHolderChanged {
                terminal_id,
                holder: input_holder,
                mine: input_holder.is_some() && input_holder == self.own_client_id,
                action,
            }),
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
        self.forget_agent_metadata(terminal_id);
        self.live_asks.remove(terminal_id);
        self.listings.forget(terminal_id);
        let was_known = self.own_spawns.contains(terminal_id)
            || self.terminal_attached.contains(terminal_id)
            || self
                .topology
                .as_ref()
                .is_some_and(|topology| topology.pane(terminal_id).is_some());
        self.terminal_attached.remove(terminal_id);
        self.preserve_terminal_geometry.remove(terminal_id);
        self.terminal_roles.remove(terminal_id);
        self.geometry_bootstrapped.remove(terminal_id);
        self.stream_recoveries.remove(terminal_id);
        self.own_spawns.remove(terminal_id);
        self.agent_streams.remove(terminal_id);
        // A subscribed agent stream ends as itself, never as a pane.
        if let Some(subscription) = self.agent_subscriptions.remove(terminal_id) {
            self.push_event(Event::AgentSessionClosed {
                terminal_id: terminal_id.clone(),
                session: subscription.info,
            });
            return true;
        }
        if let Some(topology) = self.topology.as_mut() {
            topology
                .panes
                .retain(|pane| &pane.terminal_id != terminal_id);
        }
        let reports = self
            .input_replay
            .retire_terminal(terminal_id, "terminal closed");
        self.publish_replay_reports(reports, None);
        self.delivery_fences.remove(terminal_id);
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

impl ControlPlane {
    pub(super) const fn subscribes_agent_sessions(&self) -> bool {
        self.options.subscribe_agent_sessions && self.options.automatic_lifecycle
    }

    /// The facts the runtime subscribed `terminal_id` with.
    pub(super) fn agent_session_info(&self, terminal_id: &ResourceId) -> AgentSessionInfo {
        self.agent_subscriptions
            .get(terminal_id)
            .map(|subscription| subscription.info.clone())
            .unwrap_or_default()
    }

    /// Follow the `AgentSession` resources a topology lists. A `GET_STATE`
    /// read (`authoritative`) proves an unlisted subscription closed; an
    /// `ATTACHED` view may omit resources, so a missing one there only asks
    /// for that read.
    pub(super) fn reconcile_agent_sessions(
        &mut self,
        snapshot: &SessionSnapshot,
        authoritative: bool,
    ) -> Result<(), ControlError> {
        if !self.subscribes_agent_sessions() {
            return Ok(());
        }
        self.agent_catalog = snapshot
            .resources
            .iter()
            .filter(|resource| resource.kind == ResourceKind::AgentSession)
            .cloned()
            .collect();
        let gone: Vec<ResourceId> = self
            .agent_subscriptions
            .keys()
            .filter(|id| !self.agent_catalog.iter().any(|listed| &listed.id == *id))
            .cloned()
            .collect();
        if authoritative {
            for terminal_id in gone {
                self.apply_engine(EngineEvent::closed_unknown(terminal_id.clone()))?;
                self.close_pane(&terminal_id, None, None, CloseReason::Unknown);
            }
        } else if !gone.is_empty() {
            self.queue_refresh_topology();
        }
        self.subscribe_catalogued_agents()
    }

    /// Subscribe every catalogued `AgentSession` whose parent this
    /// connection streams and which is not subscribed on it yet. A stream
    /// needs no geometry, so no resize follows the attach.
    pub(super) fn subscribe_catalogued_agents(&mut self) -> Result<(), ControlError> {
        if !self.subscribes_agent_sessions() {
            return Ok(());
        }
        let epoch = self.connection_epoch;
        for resource in self.agent_catalog.clone() {
            let admitted = resource
                .parent
                .as_ref()
                .is_some_and(|parent| self.kernel_knows(parent));
            let closed = self
                .engine
                .as_ref()
                .is_some_and(|engine| engine.is_closed(&resource.id));
            if !admitted || closed {
                continue;
            }
            let info = session_info(&resource);
            let subscription = self
                .agent_subscriptions
                .entry(resource.id.clone())
                .or_insert_with(|| AgentSubscription {
                    info: AgentSessionInfo::default(),
                    connection: 0,
                });
            subscription.info = info.clone();
            if subscription.connection == epoch {
                continue;
            }
            subscription.connection = epoch;
            self.apply_engine(EngineEvent::AgentSessionDeclared {
                terminal_id: resource.id.clone(),
                parent: info.parent,
                provider: info.provider,
                native_id: info.native_id,
                state: resource.agent.as_ref().map(|facet| facet.state.clone()),
            })?;
            self.agent_streams.insert(resource.id.clone());
            let request_id = self.next_request_id();
            self.pending
                .insert(request_id, Pending::AgentSubscription(resource.id.clone()));
            self.queue_frame(&FrameKind::Command {
                request_id,
                command: Command::AttachResource {
                    terminal_id: resource.id,
                    role_policy: self.options.attach_role,
                },
            });
        }
        Ok(())
    }

    /// A refused subscription releases the stream; the next topology read
    /// tries again.
    pub(super) fn agent_subscription_answered(&mut self, terminal_id: &ResourceId, refused: bool) {
        if !refused {
            return;
        }
        if let Some(subscription) = self.agent_subscriptions.get_mut(terminal_id) {
            subscription.connection = 0;
        }
        self.agent_streams.remove(terminal_id);
        if let Some(engine) = &self.engine {
            let _ = engine.detach(terminal_id.clone());
        }
    }
}
