//! Handshake, attach, and command response handling.

#[cfg(feature = "engine")]
use std::sync::Arc;

use super::{
    AttachTarget, BootstrapLimits, BootstrapProfile, Command, CommandResult, ControlError,
    ControlPlane, DetachReason, EngineConfig, EngineEvent, EngineHandle, ErrorCode, Event,
    FrameKind, HashSet, Pending, ResourceId, ResourceKind, ServerFeature, ServerInfo,
    SessionSnapshot, SpawnResult, StateScope, Status, Topology, topology, validate_hello_ok,
};
use crate::control::topology::View;

impl ControlPlane {
    // ----- handshake --------------------------------------------------

    pub(super) fn hello_ok(
        &mut self,
        protocol: (u16, u16, u16),
        server_id: &[u8],
        server_caps: phux_protocol::caps::ServerCapabilities,
        profile: BootstrapProfile,
        limits: BootstrapLimits,
    ) -> Result<(), ControlError> {
        if self.handshake_ready {
            return Err(ControlError::Protocol(
                "server sent duplicate HELLO_OK".to_owned(),
            ));
        }
        if let Err(error) = validate_hello_ok(
            &self.offered_caps,
            protocol.0,
            protocol.1,
            protocol.2,
            profile,
            limits,
        ) {
            let message = error.to_string();
            self.error = Some(message.clone());
            return Err(ControlError::Refused(message));
        }
        self.requalify_attach_target(server_id)?;
        // Replicas survive a reconnect so the same incarnation can resume
        // their streams. A new incarnation recycles resource ids for fresh
        // PTYs (a restarted server reseeds `@1`), and a replica kept under a
        // reused id would go on publishing the old terminal's screen while
        // the new one's stream is never bootstrapped into it.
        if self
            .server
            .as_ref()
            .is_some_and(|previous| previous.id != server_id)
        {
            if let Some(engine) = &self.engine {
                engine.reset_connection();
            }
            self.damaged.clear();
        }
        self.server = Some(ServerInfo {
            id: server_id.to_vec(),
            features: server_caps.features,
            features_ext: server_caps.features_ext,
            layers: server_caps.layers,
            protocol,
            profile,
            limits,
        });
        self.ensure_engine(profile, limits)?;
        self.handshake_ready = true;
        self.set_status(Status::Negotiated);
        let replay_supported = server_caps
            .features
            .contains(ServerFeature::AcknowledgedInput);
        let now_ms = self.now_ms();
        let reports =
            self.input_replay
                .begin_connection_at(Some(server_id), replay_supported, now_ms);
        self.publish_replay_reports(reports, None);
        if self.options.automatic_lifecycle {
            self.queue_post_handshake();
            // A session selected after startup also queues an attach. Wait for
            // its inventory before replaying subscriptions outside that session.
            if self.active_attach_id.is_none() {
                self.replay_preserving_subscriptions();
            }
        }
        self.queue_durable_frames();
        self.queue_next_upload();
        Ok(())
    }

    /// Session numbers are local to a server incarnation. Recover the same
    /// logical session by its last authoritative name, never by a recycled ID.
    fn requalify_attach_target(&mut self, server_id: &[u8]) -> Result<(), ControlError> {
        let Some(AttachTarget::ById(id)) = &self.attach_target else {
            return Ok(());
        };
        if self
            .server
            .as_ref()
            .is_none_or(|previous| previous.id == server_id)
        {
            return Ok(());
        }
        let name = self
            .topology
            .as_ref()
            .and_then(|topology| {
                topology
                    .sessions
                    .iter()
                    .find(|session| session.id == id.get())
            })
            .map(|session| session.name.clone())
            .ok_or_else(|| {
                ControlError::Refused(format!(
                    "cannot recover session {}: its previous name is unavailable",
                    id.get()
                ))
            })?;
        self.attach_target = Some(AttachTarget::ByName(name));
        Ok(())
    }

    pub(super) fn ensure_engine(
        &mut self,
        profile: BootstrapProfile,
        limits: BootstrapLimits,
    ) -> Result<(), ControlError> {
        let config = EngineConfig {
            profile,
            limits,
            scrollback_lines: self.options.scrollback_lines,
            history: self.history_config,
        };
        let same = self.engine_config.as_ref() == Some(&config);
        if self.engine.is_some() && same {
            return Ok(());
        }
        let engine = EngineHandle::start(
            &config,
            #[cfg(feature = "engine")]
            Arc::clone(&self.publication),
        )
        .map_err(|error| ControlError::Protocol(error.to_string()))?;
        if let Some(previous) = self.engine.replace(engine) {
            previous.stop();
        }
        self.engine_config = Some(config);
        Ok(())
    }

    pub(super) fn queue_post_handshake(&mut self) {
        let after_seq = self
            .options
            .event_after_seq
            .filter(|_| self.server_has(ServerFeature::EventJournal));
        self.queue_frame(&FrameKind::SubscribeEvents {
            terminal: None,
            after_seq,
        });
        let target = self.attach_target.clone();
        match target {
            Some(target) => {
                let attach_id = self.next_request_id();
                self.active_attach_id = Some(attach_id);
                if let AttachTarget::CreateIfMissing { name, .. } = &target {
                    // Creation is a one-shot act; a reconnect re-attaches.
                    self.attach_target = Some(AttachTarget::ByName(name.clone()));
                }
                self.viewport.pending = false;
                self.queue_frame(&FrameKind::Attach {
                    attach_id,
                    target,
                    viewport: self.viewport.desired,
                    request_scrollback: true,
                    scrollback_limit_lines: self.options.scrollback_lines,
                    role_policy: self.options.attach_role,
                });
            }
            None => {
                self.queue_refresh_topology();
            }
        }
    }

    pub(super) fn queue_refresh_topology(&mut self) -> u32 {
        if let Some((request_id, _)) = self.roster.topology {
            return request_id;
        }
        let request_id = self.next_request_id();
        self.roster.topology = Some((request_id, false));
        self.pending.insert(request_id, Pending::RefreshTopology);
        self.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::GetState {
                scope: StateScope::Server,
            },
        });
        request_id
    }

    pub(super) fn attached(
        &mut self,
        attach_id: u32,
        snapshot: &SessionSnapshot,
    ) -> Result<(), ControlError> {
        if self.active_attach_id != Some(attach_id) {
            return Err(ControlError::Protocol(format!(
                "ATTACHED used unexpected attach id {attach_id}"
            )));
        }
        // Lifecycle events are not replayed on attach; the previous attach's
        // listing is the floor.
        let closed = self.listings.relist(View::Attach, snapshot);
        self.close_proven(closed)?;
        let (terminals, seen) = attached_terminals(snapshot)?;
        self.attach_terminals = seen;
        self.attached_session = Some(snapshot.focused_session.get());
        self.selected_session = self.attached_session;
        self.topology = Some(Topology::from_snapshot(snapshot));
        self.error = None;
        self.apply_engine(EngineEvent::AttachStarted {
            attach_id,
            terminals: terminals.clone(),
        })?;
        self.declare_agent_sessions(snapshot, &terminals)?;
        self.reconcile_agent_sessions(snapshot, false)?;
        self.push_event(Event::TopologySnapshot {
            attach_id: Some(attach_id),
            snapshot: snapshot.clone(),
        });
        self.push_event(Event::TopologyChanged);
        self.fence_topology_read();
        // Any earlier attachment's subscriptions ended with it.
        self.forget_roster_subscriptions();
        self.sync_agent_metadata();
        // A hub's satellites and anything the grant inventories but does not
        // observe are missing from this view, not gone: read them back.
        if self.options.automatic_lifecycle && self.listings.hides(View::Attach, snapshot) {
            self.queue_refresh_topology();
        }
        Ok(())
    }

    /// Close the terminals a same-view listing proved gone.
    fn close_proven(&mut self, closed: Vec<ResourceId>) -> Result<(), ControlError> {
        for terminal_id in closed {
            self.apply_engine(EngineEvent::closed_unknown(terminal_id.clone()))?;
            self.close_pane(
                &terminal_id,
                None,
                None,
                phux_protocol::wire::frame::CloseReason::Unknown,
            );
        }
        Ok(())
    }

    fn declare_agent_sessions(
        &mut self,
        snapshot: &SessionSnapshot,
        terminals: &[ResourceId],
    ) -> Result<(), ControlError> {
        self.agent_streams.clear();
        let agent_sessions = snapshot.resources.iter().filter(|resource| {
            resource.kind == ResourceKind::AgentSession
                && resource
                    .parent
                    .as_ref()
                    .is_some_and(|parent| terminals.contains(parent))
        });
        for resource in agent_sessions {
            let facet = resource.agent.as_ref();
            self.apply_engine(EngineEvent::AgentSessionDeclared {
                terminal_id: resource.id.clone(),
                parent: resource.parent.clone(),
                provider: facet.map(|facet| facet.provider.clone()),
                native_id: facet.and_then(|facet| facet.native_id.clone()),
                state: facet.map(|facet| facet.state.clone()),
            })?;
            self.agent_streams.insert(resource.id.clone());
        }
        Ok(())
    }

    pub(super) fn attach_ready(&mut self, attach_id: u32) -> Result<(), ControlError> {
        self.apply_engine(EngineEvent::AttachReady { attach_id })?;
        if self.active_attach_id != Some(attach_id) {
            return Err(ControlError::Protocol(format!(
                "ATTACH_READY used unexpected attach id {attach_id}"
            )));
        }
        self.attached_once = true;
        self.error = None;
        self.set_status(Status::Attached);
        self.push_event(Event::Attached { attach_id });
        self.publish_viewport();
        self.replay_preserving_subscriptions();
        Ok(())
    }

    fn is_attach_refusal(&self, request_id: Option<u32>) -> bool {
        self.status == Status::Negotiated && self.active_attach_id.is_some() && request_id.is_none()
    }

    pub(super) fn server_error(
        &mut self,
        request_id: Option<u32>,
        code: ErrorCode,
        message: String,
    ) -> Result<(), ControlError> {
        if code != ErrorCode::VersionIncompatible && self.resolve_path_error(request_id, &message) {
            return Ok(());
        }
        let rendered = format!("server error {code:?}: {message}");
        // ATTACH uses attach_id, not the command request_id namespace. Its
        // refusals are uncorrelated; a correlated error belongs to a command,
        // even when its request_id numerically equals the active attach_id.
        // End this attempt instead of waiting forever for ATTACH_READY.
        if self.is_attach_refusal(request_id) {
            self.error = Some(rendered.clone());
            self.push_event(Event::ServerError {
                code,
                message,
                request_id,
            });
            return Err(ControlError::Refused(rendered));
        }
        let roster_read = request_id.and_then(|id| self.agent_metadata_error(id, code));
        if roster_read == Some(true) {
            return Ok(());
        }
        if roster_read.is_some() {
            self.push_event(Event::ServerError {
                code,
                message,
                request_id,
            });
            if code == ErrorCode::VersionIncompatible {
                return Err(ControlError::Refused(rendered));
            }
            return Ok(());
        }
        if let Some(request_id) = request_id
            && !self.pending.contains_key(&request_id)
            && !self.input_replay.owns(request_id)
        {
            // The binding extension that allocated this correlation owns its
            // reply. Surface the original wire shape exactly once.
            self.push_event(Event::Frame(Box::new(FrameKind::Error {
                request_id: Some(request_id),
                code,
                message,
            })));
            if code == ErrorCode::VersionIncompatible {
                return Err(ControlError::Refused(rendered));
            }
            return Ok(());
        }
        self.error = Some(rendered.clone());
        if let Some(request_id) = request_id {
            self.resolve_pending(
                request_id,
                CommandResult::Error {
                    code,
                    message: message.clone(),
                },
            )?;
        }
        self.push_event(Event::ServerError {
            code,
            message,
            request_id,
        });
        if code == ErrorCode::VersionIncompatible {
            return Err(ControlError::Refused(rendered));
        }
        Ok(())
    }

    pub(super) fn detached(
        &mut self,
        reason: Option<DetachReason>,
        message: &str,
    ) -> Result<(), ControlError> {
        self.forget_roster_subscriptions();
        self.push_event(Event::Detached {
            reason,
            message: message.to_owned(),
        });
        // A `None` reason is unstated (an older server) and is never read
        // as Requested.
        if reason == Some(DetachReason::Requested) {
            if self.detach_requested {
                self.close();
                return Err(ControlError::Closed);
            }
            return Ok(());
        }
        let detail = match (reason, message) {
            (Some(reason), "") => reason.describe().to_owned(),
            (Some(reason), extra) => format!("{}: {extra}", reason.describe()),
            (None, "") => "the server ended the attach without saying why".to_owned(),
            (None, extra) => extra.to_owned(),
        };
        self.error = Some(detail.clone());
        // A refused HELLO ended with PROTOCOL_ERROR before workload-auth §7's
        // AUTHENTICATION_FAILED was emitted; both stop retrying.
        let fatal = matches!(
            reason,
            Some(DetachReason::ProtocolError | DetachReason::AuthenticationFailed)
        );
        if fatal && self.options.automatic_lifecycle {
            return Err(ControlError::Refused(detail));
        }
        // Other endings: the server closes the socket itself, and the
        // frames it sent ahead of the close still apply.
        Ok(())
    }

    // ----- lifecycle frames ------------------------------------------------

    pub(super) fn resource_spawned(&mut self, request_id: u32, result: &SpawnResult) -> bool {
        if !matches!(self.pending.get(&request_id), Some(Pending::Spawn)) {
            return false;
        }
        self.pending.remove(&request_id);
        let spawned = result.spawned_id().cloned();
        let error = match result {
            SpawnResult::Err(error) => Some(spawn_error_message(error)),
            _ if spawned.is_none() => Some("unrecognized spawn result".to_owned()),
            _ => None,
        };
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
        true
    }

    pub(super) fn command_result(
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
        self.resolve_pending(request_id, result)
    }

    pub(super) fn resolve_pending(
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
        match pending {
            Pending::Spawn => {
                return Err(ControlError::Protocol(
                    "spawn request received COMMAND_RESULT instead of RESOURCE_SPAWNED".to_owned(),
                ));
            }
            Pending::AttachTerminal(terminal_id) => {
                if error.is_some() {
                    self.terminal_attached.remove(&terminal_id);
                    self.stream_recoveries.remove(&terminal_id);
                } else {
                    // Agents running in a pane admitted after the topology
                    // listed them.
                    self.subscribe_catalogued_agents()?;
                }
                self.push_event(Event::TerminalAttached {
                    request_id,
                    terminal_id,
                    error,
                });
            }
            Pending::DetachTerminal(terminal_id) => {
                if error.is_none() {
                    self.attach_terminals.remove(&terminal_id);
                    self.agent_streams.remove(&terminal_id);
                    if let Some(engine) = &self.engine {
                        let _ = engine.detach(terminal_id.clone());
                    }
                }
                self.push_event(Event::TerminalDetached {
                    request_id,
                    terminal_id,
                    error,
                });
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
            Pending::RefreshTopology => {
                self.resolve_topology_read(result)?;
            }
            Pending::AgentSubscription(terminal_id) => {
                self.agent_subscription_answered(&terminal_id, error.is_some());
            }
            Pending::Extension => {
                self.push_event(Event::CommandResult { request_id, result });
            }
            pending @ (Pending::PutFile(_) | Pending::Transcribe(_)) => {
                self.resolve_extension_result(&pending, request_id, result);
            }
        }
        self.queue_durable_frames();
        Ok(())
    }

    /// Apply a `GET_STATE` snapshot on a live connection: terminals the
    /// previous `GET_STATE` listed and this one does not are closed, but a
    /// close already processed is never resurrected by a snapshot the server
    /// built before it. A terminal only `ATTACHED` listed is merely hidden.
    pub(super) fn apply_topology_refresh(
        &mut self,
        snapshot: &SessionSnapshot,
    ) -> Result<(), ControlError> {
        let closed = self.listings.relist(View::Inventory, snapshot);
        self.close_proven(closed)?;
        let mut topology = Topology::from_snapshot(snapshot);
        if let Some(engine) = &self.engine {
            topology
                .panes
                .retain(|pane| !engine.is_closed(&pane.terminal_id));
        }
        if self.attached_session.is_none() {
            // A browsing connection has no attach barrier; its first
            // topology is the moment it becomes usable.
            self.attached_once = true;
            self.set_status(Status::Attached);
        }
        self.error = None;
        self.topology = Some(topology);
        self.reconcile_agent_sessions(snapshot, true)?;
        self.push_event(Event::TopologySnapshot {
            attach_id: None,
            snapshot: snapshot.clone(),
        });
        self.push_event(Event::TopologyChanged);
        self.sync_agent_metadata();
        Ok(())
    }
}

fn attached_terminals(
    snapshot: &SessionSnapshot,
) -> Result<(Vec<ResourceId>, HashSet<ResourceId>), ControlError> {
    // Only the focused session's Terminal-kind resources take part in the
    // attach barrier: the server bootstraps only that session, and an
    // AgentSession paints nothing.
    let focused_windows: HashSet<_> = snapshot
        .windows
        .iter()
        .filter(|window| window.session_id == snapshot.focused_session)
        .map(|window| window.id)
        .collect();
    let terminals: Vec<ResourceId> = topology::terminal_resources(snapshot)
        .filter(|pane| focused_windows.contains(&pane.window_id))
        .map(|pane| pane.id.clone())
        .collect();
    let mut seen = HashSet::new();
    if !terminals.iter().all(|id| seen.insert(id.clone())) {
        return Err(ControlError::Protocol(
            "ATTACHED target session contains duplicate terminal ids".to_owned(),
        ));
    }
    Ok((terminals, seen))
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
