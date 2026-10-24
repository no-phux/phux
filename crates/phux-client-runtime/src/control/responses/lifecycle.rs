//! Spawn, command, and pending-request results.

use super::super::{
    CommandResult, ControlError, ControlPlane, ErrorCode, Event, FrameKind, Pending, ResourceId,
    SessionSnapshot, SpawnResult, Status,
};
use crate::control::topology::View;
use phux_protocol::wire::frame::SpawnError;

impl ControlPlane {
    pub(in crate::control) fn resource_spawned(
        &mut self,
        request_id: u32,
        result: &SpawnResult,
    ) -> bool {
        if !matches!(self.pending.get(&request_id), Some(Pending::Spawn { .. })) {
            return false;
        }
        self.pending.remove(&request_id);
        let spawned = result.spawned_id().cloned();
        let error = match result {
            SpawnResult::Err(SpawnError::Unknown(_)) => Some(UNCERTAIN_SPAWN.to_owned()),
            SpawnResult::Err(error) => Some(spawn_error_message(error)),
            _ if spawned.is_none() => Some("unrecognized spawn result".to_owned()),
            _ => None,
        };
        if matches!(result, SpawnResult::Err(SpawnError::Unknown(_))) {
            self.refresh_after_uncertain_spawn();
        }
        if let Some(id) = &spawned {
            // The server pumps a spawned terminal's output to its spawner
            // and answers before it broadcasts the spawn event, so this set
            // is populated before the foreign-pane pickup examines it.
            self.own_spawns.insert(id.clone());
        }
        self.push_event(Event::TerminalSpawned {
            request_id,
            terminal_id: spawned.clone(),
            error,
        });
        // Nothing else announces this client's own spawn: refresh so the
        // topology lists it.
        if spawned.is_some() && self.handshake_ready && self.options.automatic_lifecycle {
            self.fence_topology_read();
            self.queue_refresh_topology();
        }
        self.retire_unadmitted_agent_subscriptions();
        true
    }

    pub(in crate::control) fn command_result(
        &mut self,
        request_id: u32,
        result: CommandResult,
    ) -> Result<(), ControlError> {
        if self.input_replay.owns(request_id) {
            let code = match &result {
                CommandResult::Error { code, .. } => Some(code.as_wire()),
                _ => None,
            };
            let report = self.input_replay.resolve(request_id, &result);
            self.publish_replay_reports(report.into_iter().collect(), code);
            self.queue_durable_frames();
            return Ok(());
        }
        if self.owns_org_request(request_id) {
            self.resolve_org_request(request_id, &result);
            return Ok(());
        }
        self.resolve_pending(request_id, result)
    }

    pub(in crate::control) fn resolve_pending(
        &mut self,
        request_id: u32,
        result: CommandResult,
    ) -> Result<(), ControlError> {
        let Some(pending) = self.pending.remove(&request_id) else {
            self.push_event(Event::Frame(Box::new(FrameKind::CommandResult {
                request_id,
                result,
            })));
            return Ok(());
        };
        let error = command_result_error(&result);
        if self.resolve_subscription_result(request_id, &pending, error.as_deref())? {
            self.queue_durable_frames();
            self.retire_unadmitted_agent_subscriptions();
            return Ok(());
        }
        match pending {
            // A correlated `ERROR` settles the spawn. `UNSUPPORTED` (or any
            // definite refusal) admitted nothing. A code this build does not
            // know, `UNREADABLE_REPLY` included, leaves the outcome unknown:
            // the pane may exist, so the topology is re-read to list it.
            Pending::Spawn { .. } if error.is_some() => {
                let uncertain = matches!(
                    result,
                    CommandResult::Error {
                        code: ErrorCode::Unknown(_),
                        ..
                    }
                );
                if uncertain {
                    self.refresh_after_uncertain_spawn();
                }
                self.push_event(Event::TerminalSpawned {
                    request_id,
                    terminal_id: None,
                    error: if uncertain {
                        Some(UNCERTAIN_SPAWN.to_owned())
                    } else {
                        error
                    },
                });
                self.retire_unadmitted_agent_subscriptions();
            }
            Pending::Spawn { .. } => {
                return Err(ControlError::Protocol(
                    "spawn request received COMMAND_RESULT instead of RESOURCE_SPAWNED".to_owned(),
                ));
            }
            Pending::Kill(terminal_id) => {
                self.push_event(Event::TerminalKilled {
                    request_id,
                    terminal_id,
                    error,
                });
            }
            Pending::Close => {
                self.push_event(Event::TerminalsClosed { request_id, error });
            }
            Pending::NativeAgent {
                agent_session_id,
                operation_id,
            } => {
                self.resolve_native_agent(agent_session_id, operation_id, result);
            }
            Pending::RefreshTopology => {
                self.resolve_topology_read(result)?;
            }
            Pending::AttachTerminal(_)
            | Pending::DetachTerminal { .. }
            | Pending::AgentSubscription(_)
            | Pending::AgentRelease => {}
            Pending::Extension => {
                self.push_event(Event::CommandResult { request_id, result });
            }
            Pending::Checkout(terminal_id) => {
                self.resolve_checkout(request_id, terminal_id, result);
            }
            Pending::ServiceHttp => self.resolve_service_http(request_id, result),
            pending @ (Pending::PutFile(_) | Pending::Transcribe(_)) => {
                self.resolve_extension_result(&pending, request_id, result);
            }
            pending @ (Pending::OrgPage(_) | Pending::OrgArrangement(_)) => {
                self.resolve_org_read(&pending, result);
            }
        }
        self.queue_durable_frames();
        self.retire_unadmitted_agent_subscriptions();
        Ok(())
    }

    /// Resolve stream-admission replies before unrelated command outcomes.
    /// Returns false for a command that does not change subscriptions.
    fn resolve_subscription_result(
        &mut self,
        request_id: u32,
        pending: &Pending,
        error: Option<&str>,
    ) -> Result<bool, ControlError> {
        match pending {
            Pending::AttachTerminal(terminal_id) => {
                if error.is_some() {
                    self.terminal_attached.remove(terminal_id);
                    self.stream_recoveries.remove(terminal_id);
                } else {
                    self.subscribe_catalogued_agents()?;
                }
                self.push_event(Event::TerminalAttached {
                    request_id,
                    terminal_id: terminal_id.clone(),
                    error: error.map(str::to_owned),
                });
            }
            Pending::DetachTerminal {
                terminal_id,
                was_attached,
            } => {
                self.resolve_terminal_detach(terminal_id, *was_attached, error.is_none());
                self.push_event(Event::TerminalDetached {
                    request_id,
                    terminal_id: terminal_id.clone(),
                    error: error.map(str::to_owned),
                });
            }
            Pending::AgentSubscription(terminal_id) => {
                self.agent_subscription_answered(terminal_id, error.is_some());
            }
            Pending::AgentRelease => {}
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// A detach acknowledgement releases the stream; refusal restores only
    /// an admission whose resource remained live while the request was pending.
    fn resolve_terminal_detach(
        &mut self,
        terminal_id: &ResourceId,
        was_attached: bool,
        accepted: bool,
    ) {
        if accepted {
            if self.clipboard_focus.as_ref() == Some(terminal_id) {
                self.clipboard_focus = None;
                self.events
                    .retain(|event| !matches!(event, Event::ClipboardWrite { .. }));
                self.clipboard_event_bytes = 0;
            }
            self.attach_terminals.remove(terminal_id);
            self.terminal_attached.remove(terminal_id);
            self.agent_streams.remove(terminal_id);
            if let Some(engine) = &self.engine {
                let _ = engine.detach(terminal_id.clone());
            }
            self.release_agents_of(terminal_id);
        } else if was_attached
            && self
                .engine
                .as_ref()
                .is_none_or(|engine| !engine.is_closed(terminal_id))
        {
            self.terminal_attached.insert(terminal_id.clone());
        }
    }

    /// Apply a `GET_STATE` snapshot on a live connection: terminals the
    /// previous `GET_STATE` listed and this one does not are closed, but a
    /// close already processed is never resurrected by a snapshot the server
    /// built before it. A terminal only `ATTACHED` listed is merely hidden.
    pub(in crate::control) fn apply_topology_refresh(
        &mut self,
        snapshot: &SessionSnapshot,
    ) -> Result<(), ControlError> {
        let closed = self.listings.relist(View::Inventory, snapshot);
        self.close_proven(closed)?;
        let mut topology = self.project_topology(snapshot);
        if let Some(engine) = &self.engine {
            topology
                .panes
                .retain(|pane| !engine.is_closed(&pane.terminal_id));
        }
        // The snapshot carries the user-set title; the shell's title only
        // ever arrives live, so a refresh keeps the one already shown.
        if let Some(previous) = &self.topology {
            for pane in &mut topology.panes {
                if pane.title.is_none() {
                    pane.title = previous
                        .pane(&pane.terminal_id)
                        .and_then(|shown| shown.title.clone());
                }
            }
        }
        if self.attached_session.is_none() && self.active_attach_id.is_none() {
            // A browsing connection has no attach barrier; its first
            // topology is the moment it becomes usable.
            self.attached_once = true;
            self.set_status(Status::Attached);
        }
        self.error = None;
        self.topology = Some(topology);
        self.push_event(Event::TopologySnapshot {
            attach_id: None,
            snapshot: snapshot.clone(),
        });
        self.push_event(Event::TopologyChanged);
        self.reconcile_agent_sessions(snapshot, true)?;
        self.retire_unadmitted_agent_subscriptions();
        self.sync_agent_metadata();
        self.reconcile_agent_delivery_catalog(snapshot);
        Ok(())
    }
}

/// What a spawn settles with when the server answered but this build could
/// not read whether it created anything.
pub(in crate::control) const UNCERTAIN_SPAWN: &str = "the spawn's outcome is unknown: the server's answer could not be read, so a pane may \
     exist; it is listed once the topology refresh lands";

impl ControlPlane {
    /// Re-read the topology after a spawn whose outcome is unknown, so a pane
    /// the server did create is listed rather than left invisible.
    fn refresh_after_uncertain_spawn(&mut self) {
        if self.handshake_ready && self.options.automatic_lifecycle {
            self.fence_topology_read();
            self.queue_refresh_topology();
        }
    }
}

fn command_result_error(result: &CommandResult) -> Option<String> {
    match result {
        CommandResult::Ok | CommandResult::OkWith(_) => None,
        CommandResult::Error { code, message } => Some(format!("{code:?}: {message}")),
        _ => Some("unrecognized command result".to_owned()),
    }
}

fn spawn_error_message(error: &phux_protocol::wire::frame::SpawnError) -> String {
    use phux_protocol::wire::frame::SpawnError;
    match error {
        SpawnError::GroupNotFound => "the server has no such group".to_owned(),
        SpawnError::SpawnFailed(message) | SpawnError::SatelliteUnreachable(message) => {
            message.clone()
        }
        SpawnError::UnsupportedSatelliteRoute => {
            "the server cannot route the spawn to that satellite".to_owned()
        }
        _ => "spawn failed".to_owned(),
    }
}
