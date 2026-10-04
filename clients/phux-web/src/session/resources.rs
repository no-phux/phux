//! Agent-resource discovery and optional child-stream subscriptions.

use super::*;
use phux_protocol::wire::frame::CloseReason;
use phux_protocol::wire::info::SessionSnapshot;

// Only admission windows retain server-wide observations. Overflow is a visible
// connection failure rather than silent eviction of a potentially relevant child.
const MAX_PENDING_AGENT_DISCOVERIES: usize = 256;

#[derive(Default)]
pub(super) struct AgentDiscovery {
    parent: Option<ResourceId>,
    closed: bool,
}

impl Session {
    pub(super) fn accept_snapshot(&mut self, attach_id: u32, snapshot: SessionSnapshot) -> Outcome {
        if attach_id != ATTACH_ID {
            return self.protocol_failure("ATTACHED used the wrong attach identifier");
        }
        // Only Terminal-kind resources build panes and gate the
        // barrier; an AgentSession bound to one of them is declared
        // to the kernel as a record stream and projected as a badge.
        let terminal_ids: Vec<_> = snapshot
            .resources
            .iter()
            .filter(|pane| pane.kind == ResourceKind::Terminal)
            .map(|pane| pane.id.clone())
            .collect();
        let focused_terminal = snapshot.focused_resource;
        let (mut outcome, applied) = self.apply_kernel(KernelInput::AttachStarted {
            attach_id,
            terminals: &terminal_ids,
        });
        if !applied {
            return outcome;
        }
        for pane in snapshot
            .resources
            .iter()
            .filter(|pane| pane.kind == ResourceKind::AgentSession)
            .filter(|pane| {
                pane.parent
                    .as_ref()
                    .is_some_and(|parent| terminal_ids.contains(parent))
            })
        {
            let facet = pane.agent.as_ref();
            let declaration = AgentSessionDeclaration {
                terminal_id: &pane.id,
                parent: pane.parent.as_ref(),
                provider: facet.map(|facet| facet.provider.as_str()),
                native_id: facet.and_then(|facet| facet.native_id.as_deref()),
                state: facet.map(|facet| facet.state.as_str()),
            };
            let closed = self.discovery_closed(&pane.id)
                || pane
                    .parent
                    .as_ref()
                    .is_some_and(|parent| self.discovery_closed(parent));
            let declared = if closed {
                self.retire_discovered_agent(declaration)
            } else {
                self.attach_agent(declaration)
            };
            if declared.fatal.is_some() {
                return declared;
            }
            outcome.send.extend(declared.send);
            outcome.badges |= declared.badges;
        }
        self.focused_terminal = Some(focused_terminal);
        self.cancel_path_query();
        self.terminal_order = terminal_ids;
        self.awaiting_agent_inventory = false;
        self.reconcile_agent_discoveries(&mut outcome);
        self.restore_layout();
        self.render_visible = false;
        outcome
    }

    /// Every declaration has one stream request. The kernel remembers closed IDs,
    /// so duplicate announcements cannot resurrect a closed resource.
    fn attach_agent(&mut self, declaration: AgentSessionDeclaration<'_>) -> Outcome {
        let Some(kernel) = self.kernel.as_ref() else {
            return Outcome::default();
        };
        if kernel.resource_kind(declaration.terminal_id).is_some() {
            return Outcome::default();
        }
        let Some(parent) = declaration.parent else {
            return Outcome::default();
        };
        if !self.holds_agent_parent(parent) {
            return Outcome::default();
        }
        let (mut outcome, applied) =
            self.apply_kernel(KernelInput::AgentSessionDeclared(declaration));
        if !applied {
            return outcome;
        }
        let request_id = self.next_pane_request();
        self.pending_agents
            .insert(request_id, declaration.terminal_id.clone());
        outcome.send.push(encode(&FrameKind::Command {
            request_id,
            command: Command::AttachResource {
                terminal_id: declaration.terminal_id.clone(),
                role_policy: None,
            },
        }));
        outcome.badges = true;
        outcome
    }

    pub(super) fn reduce_resource_event(&mut self, id: &ResourceId, event: &AgentEvent) -> Outcome {
        match event {
            AgentEvent::ResourceSpawned {
                kind: ResourceKind::AgentSession,
                parent,
            } => self.discover_agent(id, parent.as_ref()),
            // The event can beat the attach reply (or be the only close when
            // attachment was refused). Removing this badge never closes its parent.
            AgentEvent::ResourceClosed { exit_status } => {
                self.close_discovered_resource(id, *exit_status)
            }
            _ => Outcome::default(),
        }
    }

    fn holds_agent_parent(&self, parent: &ResourceId) -> bool {
        self.kernel
            .as_ref()
            .is_some_and(|kernel| kernel.active_attach_contains(parent))
            || self.terminal_order.contains(parent)
    }

    fn discovery_closed(&self, id: &ResourceId) -> bool {
        self.agent_discoveries
            .get(id)
            .is_some_and(|discovery| discovery.closed)
    }

    fn discover_agent(&mut self, id: &ResourceId, parent: Option<&ResourceId>) -> Outcome {
        let Some(parent) = parent else {
            return Outcome::default();
        };
        if self.holds_agent_parent(parent) {
            return self.attach_agent(AgentSessionDeclaration {
                terminal_id: id,
                parent: Some(parent),
                provider: None,
                native_id: None,
                state: None,
            });
        }
        self.defer_agent_discovery(id, Some(parent), false)
    }

    fn defer_agent_discovery(
        &mut self,
        id: &ResourceId,
        parent: Option<&ResourceId>,
        closed: bool,
    ) -> Outcome {
        if self.kernel.is_none() || !(self.awaiting_agent_inventory || self.pending_split.is_some())
        {
            return Outcome::default();
        }
        if !self.agent_discoveries.contains_key(id)
            && self.agent_discoveries.len() >= MAX_PENDING_AGENT_DISCOVERIES
        {
            return self.protocol_failure("agent discovery admission buffer exhausted; reconnect");
        }
        let parent_closed = parent.is_some_and(|parent| self.discovery_closed(parent));
        let discovery = self.agent_discoveries.entry(id.clone()).or_default();
        if parent.is_some() {
            discovery.parent = parent.cloned();
        }
        discovery.closed |= closed || parent_closed;
        Outcome::default()
    }

    fn close_discovered_resource(&mut self, id: &ResourceId, exit_status: Option<i32>) -> Outcome {
        for discovery in self.agent_discoveries.values_mut() {
            if discovery.parent.as_ref() == Some(id) {
                discovery.closed = true;
            }
        }
        if self.is_agent_session(id) {
            return self.close_agent(id, exit_status);
        }
        self.defer_agent_discovery(id, None, true)
    }

    /// Admission is authoritative; unrelated server-wide observations are dropped.
    /// Closed children become kernel tombstones without ever requesting a stream.
    pub(super) fn reconcile_agent_discoveries(&mut self, outcome: &mut Outcome) {
        let discoveries = std::mem::take(&mut self.agent_discoveries);
        for (id, discovery) in discoveries {
            let Some(parent) = discovery.parent else {
                continue;
            };
            if !self.holds_agent_parent(&parent) {
                continue;
            }
            let declaration = AgentSessionDeclaration {
                terminal_id: &id,
                parent: Some(&parent),
                provider: None,
                native_id: None,
                state: None,
            };
            let update = if discovery.closed {
                self.retire_discovered_agent(declaration)
            } else {
                self.attach_agent(declaration)
            };
            outcome.send.extend(update.send);
            outcome.badges |= update.badges;
            if update.fatal.is_some() {
                outcome.fatal = update.fatal;
                return;
            }
        }
    }

    fn retire_discovered_agent(&mut self, declaration: AgentSessionDeclaration<'_>) -> Outcome {
        if self.is_agent_session(declaration.terminal_id) {
            return self.close_agent(declaration.terminal_id, None);
        }
        let (outcome, applied) = self.apply_kernel(KernelInput::AgentSessionDeclared(declaration));
        if !applied {
            return outcome;
        }
        self.close_agent(declaration.terminal_id, None)
    }

    fn close_agent(&mut self, id: &ResourceId, exit_status: Option<i32>) -> Outcome {
        let (mut outcome, _) = self.apply_kernel(KernelInput::ResourceClosed {
            terminal_id: id,
            exit_status,
            signal: None,
            reason: CloseReason::Unknown,
        });
        outcome.badges = true;
        outcome
    }

    pub(super) fn reduce_agent_reply(&mut self, frame: &FrameKind) -> Option<Outcome> {
        let (request_id, refused) = match frame {
            FrameKind::CommandResult { request_id, result } => {
                (*request_id, matches!(result, CommandResult::Error { .. }))
            }
            FrameKind::Error {
                request_id: Some(id),
                ..
            } => (*id, true),
            _ => return None,
        };
        let id = self.pending_agents.remove(&request_id)?;
        Some(if refused {
            self.close_agent(&id, None)
        } else {
            Outcome::default()
        })
    }

    pub(super) fn close_resource(
        &mut self,
        terminal_id: ResourceId,
        exit_status: Option<i32>,
        reason: CloseReason,
        signal: Option<i32>,
    ) -> Outcome {
        // Only defer a known child; terminal stream closures must retain the
        // existing kernel barrier/split cancellation behavior.
        if self
            .agent_discoveries
            .get(&terminal_id)
            .is_some_and(|discovery| discovery.parent.is_some())
        {
            return self.close_discovered_resource(&terminal_id, exit_status);
        }
        self.retiring_panes.retain(|id| id != &terminal_id);
        self.pane_sizes.remove(&terminal_id);
        let was_focused = self.focused_terminal.as_ref() == Some(&terminal_id);
        let was_agent = self.is_agent_session(&terminal_id);
        let (mut outcome, applied) = self.apply_kernel(KernelInput::ResourceClosed {
            terminal_id: &terminal_id,
            exit_status,
            signal,
            reason,
        });
        if applied && !was_agent {
            self.terminal_order.retain(|id| id != &terminal_id);
            self.layout = self
                .layout
                .take()
                .and_then(|layout| layout.remove(&terminal_id));
            if self
                .pending_close
                .as_ref()
                .is_some_and(|(_, id)| id == &terminal_id)
            {
                self.pending_close = None;
            }
            if self
                .pending_split
                .as_ref()
                .is_some_and(|pending| pending.resource.as_ref() == Some(&terminal_id))
            {
                self.pending_split = None;
                self.agent_discoveries.clear();
                self.pane_error = Some(
                    "The terminal closed before the split completed. Try splitting again."
                        .to_owned(),
                );
            }
            if was_focused {
                self.cancel_path_query();
                self.focused_terminal = self.first_published_terminal();
            }
            outcome.panes = true;
            outcome.render |= self.render_visible;
            outcome.badges = true;
            outcome.send.extend(self.pane_resize_frames());
        }
        if applied && was_agent {
            outcome.badges = true;
        }
        outcome
    }

    pub(super) fn finish_attach(&mut self, attach_id: u32) -> Outcome {
        let (mut outcome, applied) = self.apply_kernel(KernelInput::AttachReady { attach_id });
        if applied {
            self.attach_ready = true;
            outcome.panes = true;
            if self.pane_rects().len() > 1 {
                outcome.send.extend(self.pane_resize_frames());
            }
        }
        outcome
    }
}
