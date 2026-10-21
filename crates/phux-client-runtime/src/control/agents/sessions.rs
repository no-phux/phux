//! The runtime's own `AgentSession` subscriptions: following the catalog,
//! admitting delivery, and releasing streams when their parent closes.

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::wire::frame::{CloseReason, Command, FrameKind};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

use crate::control::{
    AgentSessionInfo, ControlError, ControlPlane, EngineEvent, Event, Pending, ResourceId,
    ResourceKind,
};

/// One `AgentSession` stream `subscribe_agent_sessions` follows.
#[derive(Debug)]
pub(in crate::control) struct AgentSubscription {
    /// What the catalog last said about it.
    pub(in crate::control) info: AgentSessionInfo,
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
    /// Safe committed sequence for an `AgentSession` resume request.
    ///
    /// The current server must advertise resume support. The owner-thread
    /// query reads only committed kernel state, which survives reconnect to
    /// the same incarnation and is reset before a new incarnation is admitted.
    #[must_use]
    pub fn agent_after_seq(&self, terminal_id: &ResourceId) -> Option<u64> {
        if !self.handshake_ready
            || !self
                .server
                .as_ref()
                .is_some_and(|server| server.has_ext(ServerFeatureExt::AgentStreamResume))
        {
            return None;
        }
        self.engine
            .as_ref()?
            .agent_stream_info(terminal_id)
            .map(|committed| committed.last_seq)
    }

    pub(in crate::control) const fn subscribes_agent_sessions(&self) -> bool {
        self.options.subscribe_agent_sessions && self.options.automatic_lifecycle
    }

    /// The facts the runtime subscribed `terminal_id` with.
    pub(in crate::control) fn agent_session_info(
        &self,
        terminal_id: &ResourceId,
    ) -> AgentSessionInfo {
        self.agent_subscriptions
            .get(terminal_id)
            .map(|subscription| subscription.info.clone())
            .unwrap_or_default()
    }

    /// Apply an authoritative registry snapshot to agent delivery admission.
    ///
    /// `snapshot` is a full registry read (`GET_STATE`, or the C workspace
    /// registry), never a partial `ATTACHED` view. Listed `AgentSession` ids
    /// are admitted. If this connection is already attached, their withheld
    /// publications release into the existing event queue in commit order.
    /// Omitted committed streams are forgotten, including their resume cursor;
    /// any withheld publications are discarded. That release is local: no detach frame
    /// and no close event. A later `AgentSessionDeclared` may admit the id
    /// again for new commits; discarded rows do not return.
    ///
    /// Automatic subscription closes stay in `reconcile_agent_sessions`.
    /// Call this after that retirement so a subscribed id is closed once.
    pub fn reconcile_agent_delivery_catalog(&mut self, snapshot: &SessionSnapshot) {
        let listed: std::collections::HashSet<_> = snapshot
            .resources
            .iter()
            .filter(|resource| resource.kind == ResourceKind::AgentSession)
            .map(|resource| &resource.id)
            .collect();
        let omitted = self.agent_delivery.ids_absent(&listed);
        for id in &omitted {
            let _ = self.agent_delivery.retire(id);
            self.agent_streams.remove(id);
            if let Some(engine) = &self.engine {
                let _ = engine.detach(id.clone());
            }
        }
        self.events.retain(|event| match event {
            Event::AgentPublication { publication, .. } => {
                listed.contains(&publication.resource_id)
            }
            _ => true,
        });
        for id in &listed {
            self.agent_delivery.admit(id);
        }
        if self.status == crate::control::Status::Attached {
            self.release_agent_delivery();
        }
    }

    /// Follow the `AgentSession` resources a topology lists. A `GET_STATE`
    /// read (`authoritative`) proves an unlisted subscription closed; an
    /// `ATTACHED` view may omit resources, so a missing one there only asks
    /// for that read.
    pub(in crate::control) fn reconcile_agent_sessions(
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
    pub(in crate::control) fn subscribe_catalogued_agents(&mut self) -> Result<(), ControlError> {
        if !self.subscribes_agent_sessions() {
            return Ok(());
        }
        for resource in self.agent_catalog.clone() {
            let admitted = resource
                .parent
                .as_ref()
                .is_some_and(|parent| self.terminal_is_admitted(parent));
            if admitted {
                self.follow_agent_session(resource)?;
            }
        }
        Ok(())
    }

    /// Keep one admitted `AgentSession` subscribed and declared on this
    /// connection.
    fn follow_agent_session(&mut self, resource: ResourceInfo) -> Result<(), ControlError> {
        let epoch = self.connection_epoch;
        let subscribed = self
            .agent_subscriptions
            .get(&resource.id)
            .is_some_and(|subscription| subscription.connection == epoch);
        // A replacement ATTACH forgets the kernel declarations, not the
        // per-resource stream: declare again without re-subscribing.
        if subscribed && self.agent_streams.contains(&resource.id) {
            if let Some(subscription) = self.agent_subscriptions.get_mut(&resource.id) {
                subscription.info = session_info(&resource);
            }
            return Ok(());
        }
        // One engine query, only for a session not yet followed.
        if !subscribed
            && self
                .engine
                .as_ref()
                .is_some_and(|engine| engine.is_closed(&resource.id))
        {
            return Ok(());
        }
        self.agent_delivery.admit(&resource.id);
        let info = session_info(&resource);
        self.apply_engine(EngineEvent::AgentSessionDeclared {
            terminal_id: resource.id.clone(),
            parent: info.parent.clone(),
            provider: info.provider.clone(),
            native_id: info.native_id.clone(),
            state: resource.agent.as_ref().map(|facet| facet.state.clone()),
        })?;
        self.agent_streams.insert(resource.id.clone());
        self.agent_subscriptions.insert(
            resource.id.clone(),
            AgentSubscription {
                info,
                connection: epoch,
            },
        );
        if subscribed {
            return Ok(());
        }
        self.retired_agents.remove(&resource.id);
        let request_id = self.next_request_id();
        self.pending
            .insert(request_id, Pending::AgentSubscription(resource.id.clone()));
        let agent_after_seq = self.agent_after_seq(&resource.id);
        self.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::attach_agent_session(
                resource.id,
                self.options.attach_role,
                agent_after_seq,
            ),
        });
        Ok(())
    }

    /// End one subscription: it is closed to the consumer exactly once.
    pub(in crate::control) fn end_agent_subscription(&mut self, terminal_id: &ResourceId) -> bool {
        let Some(subscription) = self.agent_subscriptions.remove(terminal_id) else {
            return false;
        };
        self.agent_streams.remove(terminal_id);
        self.retired_agents.insert(terminal_id.clone());
        let stream = self.agent_delivery.retire(terminal_id);
        self.push_event(Event::AgentSessionClosed {
            terminal_id: terminal_id.clone(),
            session: subscription.info,
            stream,
        });
        true
    }

    /// The pane `parent` is no longer streamed: release its agent sessions
    /// and end them for the consumer, so every followed session still runs
    /// in a pane this connection streams.
    pub(in crate::control) fn release_agents_of(&mut self, parent: &ResourceId) {
        let epoch = self.connection_epoch;
        let children: Vec<ResourceId> = self
            .agent_subscriptions
            .iter()
            .filter(|(_, subscription)| subscription.info.parent.as_ref() == Some(parent))
            .map(|(id, _)| id.clone())
            .collect();
        for terminal_id in children {
            self.release_agent_subscription(&terminal_id, epoch);
        }
    }

    fn release_agent_subscription(&mut self, terminal_id: &ResourceId, epoch: u64) {
        if self
            .agent_subscriptions
            .get(terminal_id)
            .is_some_and(|subscription| subscription.connection == epoch)
        {
            let request_id = self.next_request_id();
            self.pending.insert(request_id, Pending::AgentRelease);
            self.queue_frame(&FrameKind::Command {
                request_id,
                command: Command::DetachResource {
                    terminal_id: terminal_id.clone(),
                },
            });
        }
        // Losing admission ends a subscription, not the actual resource.
        // Detach leaves it eligible for a later subscription in this incarnation.
        if let Some(engine) = &self.engine {
            let _ = engine.detach(terminal_id.clone());
        }
        self.end_agent_subscription(terminal_id);
    }

    /// A reconnect may attach the selected session rather than the old home.
    /// Keep old subscriptions until all replayed admissions have answered:
    /// neither ATTACHED nor an inventory alone proves their parents unstreamed.
    pub(in crate::control) fn retire_unadmitted_agent_subscriptions(&mut self) {
        if self.status != crate::control::Status::Attached || self.agent_admissions_pending() {
            return;
        }
        let orphaned: Vec<_> = self
            .agent_subscriptions
            .iter()
            .filter(|(_, subscription)| {
                !subscription
                    .info
                    .parent
                    .as_ref()
                    .is_some_and(|parent| self.terminal_is_admitted(parent))
            })
            .map(|(id, _)| id.clone())
            .collect();
        for terminal_id in orphaned {
            self.release_agent_subscription(&terminal_id, self.connection_epoch);
        }
    }

    fn agent_admissions_pending(&self) -> bool {
        self.pending.values().any(|pending| {
            matches!(
                pending,
                Pending::AttachTerminal(_) | Pending::DetachTerminal { .. } | Pending::Spawn { .. }
            )
        })
    }

    /// A new server incarnation: every followed session is gone, and its
    /// ids may now name anything.
    pub(in crate::control) fn end_agent_incarnation(&mut self) {
        let ended: Vec<ResourceId> = self.agent_subscriptions.keys().cloned().collect();
        for terminal_id in ended {
            self.end_agent_subscription(&terminal_id);
        }
        self.discard_agent_delivery();
        self.agent_catalog.clear();
        self.retired_agents.clear();
    }

    /// A refused subscription releases the stream; the next topology read
    /// tries again.
    pub(in crate::control) fn agent_subscription_answered(
        &mut self,
        terminal_id: &ResourceId,
        refused: bool,
    ) {
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
