//! Handshake, attach, and command response handling.

#[cfg(feature = "engine")]
use std::sync::Arc;

use super::{
    AttachTarget, BootstrapLimits, BootstrapProfile, Command, CommandResult, ControlError,
    ControlPlane, DetachReason, EngineConfig, EngineEvent, EngineHandle, ErrorCode, Event,
    FrameKind, HashSet, Pending, ResourceId, ResourceKind, ServerFeature, SessionSnapshot,
    SpawnResult, StateScope, Status, topology,
};
use crate::control::topology::View;
use crate::handshake::HelloOk;

impl ControlPlane {
    // ----- handshake --------------------------------------------------

    pub(super) fn hello_ok(&mut self, answer: HelloOk) -> Result<(), ControlError> {
        if self.handshake_ready {
            return Err(ControlError::Protocol(
                "server sent duplicate HELLO_OK".to_owned(),
            ));
        }
        let info = match answer.accept(&self.offered_caps) {
            Ok(info) => info,
            Err(error) => {
                let message = error.to_string();
                self.error = Some(message.clone());
                return Err(ControlError::Refused(message));
            }
        };
        self.requalify_attach_target(&info.id)?;
        // Replicas survive a reconnect so the same incarnation can resume
        // their streams. A new incarnation recycles resource ids for fresh
        // PTYs (a restarted server reseeds `@1`), and a replica kept under a
        // reused id would go on publishing the old terminal's screen while
        // the new one's stream is never bootstrapped into it.
        let new_incarnation = self
            .server
            .as_ref()
            .is_some_and(|previous| previous.id != info.id);
        if new_incarnation {
            if let Some(engine) = &self.engine {
                engine.reset_connection();
            }
            self.damaged.clear();
            // The new incarnation reuses resource ids, a Terminal's as
            // readily as an agent session's.
            self.end_agent_incarnation();
        }
        let (profile, limits) = (info.profile, info.limits);
        let replay_supported = info.has(ServerFeature::AcknowledgedInput);
        let server_id = info.id.clone();
        self.server = Some(info);
        self.ensure_engine(profile, limits)?;
        self.handshake_ready = true;
        self.set_status(Status::Negotiated);
        let now_ms = self.now_ms();
        let reports =
            self.input_replay
                .begin_connection_at(Some(&server_id), replay_supported, now_ms);
        self.publish_replay_reports(reports, None);
        if self.options.automatic_lifecycle {
            self.queue_post_handshake();
            // A session selected after startup also queues an attach. Wait for
            // its inventory before replaying subscriptions outside that session.
            if self.active_attach_id.is_none() {
                self.replay_preserving_subscriptions();
            }
        }
        self.org_connection_negotiated(new_incarnation);
        self.approvals_negotiated();
        self.queue_durable_frames();
        self.queue_next_upload();
        Ok(())
    }

    /// Session numbers are local to a server incarnation. Recover the same
    /// logical session by its last authoritative name, never by a recycled ID.
    fn requalify_attach_target(&mut self, server_id: &[u8]) -> Result<(), ControlError> {
        // Manual embedders select their own next attach after negotiation; an
        // old target must not refuse a connection they intend to retarget.
        if !self.options.automatic_lifecycle {
            return Ok(());
        }
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
        // Explicit history policies override the requested scrollback depth.
        // Compare effective settings so reconnect does not discard committed
        // agent records for a change in an unused default.
        //
        // A real profile, limits, or effective-history change still replaces
        // the owner: those values are constructor-private on the kernel, and
        // this crate cannot transplant agent streams. Resume cursors stay a
        // live kernel query. Delivery stamps are never aliased onto the new
        // owner. The publication journal is control-plane state and survives
        // a same-incarnation rebuild; incarnation reset discards it.
        let same = self.engine_config.as_ref().is_some_and(|previous| {
            previous.profile == config.profile
                && previous.limits == config.limits
                && previous.history() == config.history()
        });
        if self.engine.is_some() && same {
            return Ok(());
        }
        let engine = EngineHandle::start(
            &config,
            #[cfg(feature = "engine")]
            Arc::clone(&self.publication),
        )
        .map_err(|error| ControlError::Protocol(error.to_string()))?;
        if self.terminal_theme.is_some() {
            engine.set_terminal_theme(self.terminal_theme);
        }
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
        // Names are lookup hints, not identities: a rename may free the old
        // name for another session before this socket reconnects. Numeric IDs
        // are stable within an incarnation and requalified on a server change.
        self.attach_target = Some(AttachTarget::ById(snapshot.focused_session));
        self.topology = Some(self.project_topology(snapshot));
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
        // Manual embedders own request ids. An autonomous project-tag GET
        // would steal the first metadata read their fixtures answer.
        if self.options.automatic_lifecycle {
            self.watch_session_project();
        }
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
        self.attach_ended = false;
        self.error = None;
        self.set_status(Status::Attached);
        self.push_event(Event::Attached { attach_id });
        self.publish_viewport();
        self.replay_preserving_subscriptions();
        self.replay_pending_spawns();
        self.retire_unadmitted_agent_subscriptions();
        self.release_agent_delivery();
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
        if let Some(id) = request_id
            && self.approval_error(id, code, &message)
        {
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
        if request_id.is_some_and(|id| self.project_tag_pending == Some(id)) {
            self.project_tag_pending = None;
            return Ok(());
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
        // Revoked, expired, replaced or killed: the server ended this attach.
        // Input sent before the socket closes would only reach a refusal.
        self.attach_ended = true;
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
        if !matches!(self.pending.get(&request_id), Some(Pending::Spawn { .. })) {
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
        self.retire_unadmitted_agent_subscriptions();
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
        if self.owns_org_request(request_id) {
            self.resolve_org_request(request_id, &result);
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
        if self.resolve_subscription_result(request_id, &pending, error.as_deref())? {
            self.queue_durable_frames();
            self.retire_unadmitted_agent_subscriptions();
            return Ok(());
        }
        match pending {
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
    pub(super) fn apply_topology_refresh(
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

fn attached_terminals(
    snapshot: &SessionSnapshot,
) -> Result<(Vec<ResourceId>, HashSet<ResourceId>), ControlError> {
    // Only the focused session's Terminal-kind resources take part in the
    // attach barrier: the server bootstraps only that session, and an
    // AgentSession paints nothing.
    let terminals: Vec<ResourceId> = topology::terminal_resources(snapshot)
        .filter(|pane| topology::session_of(snapshot, pane) == Some(snapshot.focused_session))
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
