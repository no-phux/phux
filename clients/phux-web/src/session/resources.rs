//! Agent-resource discovery and optional child-stream subscriptions.

use super::*;
use phux_protocol::wire::frame::CloseReason;
use phux_protocol::wire::info::SessionSnapshot;

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
            let declared = self.attach_agent(AgentSessionDeclaration {
                terminal_id: &pane.id,
                parent: pane.parent.as_ref(),
                provider: facet.map(|facet| facet.provider.as_str()),
                native_id: facet.and_then(|facet| facet.native_id.as_deref()),
                state: facet.map(|facet| facet.state.as_str()),
            });
            if declared.fatal.is_some() {
                return declared;
            }
            outcome.send.extend(declared.send);
            outcome.badges |= declared.badges;
        }
        self.focused_terminal = Some(focused_terminal);
        self.cancel_path_query();
        self.terminal_order = terminal_ids;
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
        if !kernel.active_attach_contains(parent) && !self.terminal_order.contains(parent) {
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
            } => self.attach_agent(AgentSessionDeclaration {
                terminal_id: id,
                parent: parent.as_ref(),
                provider: None,
                native_id: None,
                state: None,
            }),
            // The event can beat the attach reply (or be the only close when
            // attachment was refused). Removing this badge never closes its parent.
            AgentEvent::ResourceClosed { exit_status } if self.is_agent_session(id) => {
                self.close_agent(id, *exit_status)
            }
            _ => Outcome::default(),
        }
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
