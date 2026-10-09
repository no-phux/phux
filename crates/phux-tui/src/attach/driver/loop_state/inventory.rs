//! Host inventory, the serving host, and session rename.

use super::{
    AttachError, Command, CommandResult, CommandValue, Connection, FrameKind, FrameOutcome,
    HostAnswers, Notice, ProjectTagFrame, RepaintAccumulator, Scope, apply_graph_rename,
    federation_notices, host_answers, host_inventory_overdue, sync_agent_meta_subscriptions,
    unexplained_unreachable_notices,
};

impl super::SessionLoop {
    /// ADR-0040: keep every live pane's `phux.agent/v1` watch in step with the
    /// pane set.
    pub(super) async fn sync_agent_meta(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        sync_agent_meta_subscriptions(
            conn,
            self.mirror.panes.keys().cloned().collect(),
            &mut self.mirror.agent_meta,
            &mut self.next_request_id,
        )
        .await
    }

    /// Fetch and subscribe each peer session's persisted layout (for the
    /// picker's one-step rows and the live roster), then the peer agent
    /// records, serving host, and host inventory. Fire-and-forget.
    pub(super) async fn sweep_peer_layouts(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        self.peers
            .sweep_layouts(conn, &mut self.next_request_id)
            .await?;
        // Agent watches must not wait for a persisted TUI layout.
        self.reconcile_peer_agents(conn).await?;
        // The fleet's other half. Rides the same deferred
        // sweep, so it costs the first paint nothing.
        self.request_serving_host(conn).await?;
        self.request_project_tag(conn).await?;
        self.request_host_inventory(conn).await
    }

    /// Read the one stored project tag. The subscribe in bootstrap only
    /// delivers later writes.
    pub(super) async fn request_project_tag(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        if self.peers.project_tag_pending.is_some() {
            return Ok(());
        }
        let request_id = self.take_request_id();
        self.peers.project_tag_pending = Some(request_id);
        super::super::session_io::send_unless_peer_gone(
            conn,
            &FrameKind::GetMetadata {
                request_id,
                scope: Scope::Global,
                key: phux_protocol::wire::frame::SESSION_PROJECT_KEY.to_owned(),
            },
        )
        .await
    }

    /// Join the stored project tag to the session that still has that name.
    pub(super) fn fold_project_tag(&mut self, value: Option<&[u8]>) {
        self.peers.project_tag_pending = None;
        self.peers.stored_project_tag = value
            .and_then(phux_protocol::wire::frame::decode_session_project)
            .map(|(name, project)| (name.to_owned(), project.to_owned()));
        self.apply_stored_project_tag();
    }

    /// Rebuild `project_tags` from the stored tag and the current session names.
    pub(super) fn apply_stored_project_tag(&mut self) {
        let mut next = std::collections::HashMap::new();
        if let Some((name, project)) = &self.peers.stored_project_tag
            && let Some(session) = self
                .peers
                .sessions
                .iter()
                .find(|session| session.name == *name)
        {
            next.insert(session.id, project.clone());
        }
        if next == self.peers.project_tags {
            return;
        }
        self.peers.project_tags = next;
        self.peers.chrome_dirty = true;
        self.session_picker_dirty = true;
    }

    /// Read the server's identity after first paint without blocking the frame loop.
    pub(super) async fn request_serving_host(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        if !self.whoami_supported || self.peers.serving_host_attempted {
            return Ok(());
        }
        self.peers.serving_host_attempted = true;
        let request_id = self.take_request_id();
        self.peers.serving_host_pending = Some(request_id);
        super::super::session_io::send_unless_peer_gone(
            conn,
            &FrameKind::GetMetadata {
                request_id,
                scope: Scope::Global,
                key: phux_protocol::wire::frame::WHOAMI_KEY.to_owned(),
            },
        )
        .await
    }

    /// Invalid or unsupported identity leaves the honest `this server` label.
    pub(super) fn fold_serving_host(&mut self, value: Option<&[u8]>) {
        use phux_protocol::wire::frame::{WHOAMI_SCHEMA_VERSION, WhoamiRecord};
        self.peers.serving_host_pending = None;
        self.peers.serving_host = value
            .and_then(|bytes| serde_json::from_slice::<WhoamiRecord>(bytes).ok())
            .filter(|r| r.schema_version == WHOAMI_SCHEMA_VERSION && !r.host.trim().is_empty())
            // The hosts provider's spelling, so the machine header does not
            // flip between `mac.local` and `mac` when its listing lands.
            .map(|r| crate::attach::hosts::short_host_label(&r.host).to_owned());
        self.peers.chrome_dirty = true;
    }

    /// Ask for the federation host inventory (one `GET_STATE`). Skipped
    /// without the feature or while one is already in flight.
    pub(super) async fn request_host_inventory(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        if !self.host_sessions_supported || self.peers.hosts_pending.is_some() {
            return Ok(());
        }
        let request_id = self.take_request_id();
        self.peers.hosts_pending = Some(request_id);
        self.peers.hosts_pending_since = Some(std::time::Instant::now());
        super::super::session_io::send_unless_peer_gone(
            conn,
            &FrameKind::Command {
                request_id,
                command: Command::GetState {
                    scope: phux_protocol::wire::frame::StateScope::Server,
                },
            },
        )
        .await
    }

    /// Fold a host-inventory reply. A refusal keeps the previous inventory.
    /// Held unreachable notices the reply explains are dropped; the rest
    /// surface. Returns which satellites it reached and which it could not.
    pub(super) fn fold_host_inventory(
        &mut self,
        result: &phux_protocol::wire::frame::CommandResult,
        repaint: &mut RepaintAccumulator,
    ) -> HostAnswers {
        let held = self.end_host_inventory_request();
        let explained_by: &[phux_protocol::wire::info::HostInventory] = match result {
            phux_protocol::wire::frame::CommandResult::OkWith(
                phux_protocol::wire::frame::CommandValue::State(snapshot),
            ) => {
                self.peers.hosts = snapshot.hosts().to_vec();
                let sessions_changed = self.peers.sessions != snapshot.sessions;
                if sessions_changed {
                    self.peers.sessions.clone_from(&snapshot.sessions);
                    self.apply_stored_project_tag();
                }
                // Sweep only when the graph the sweep reads actually moved.
                if sessions_changed || self.snapshot_graph_changed(snapshot) {
                    self.peers.sweep_pending = true;
                }
                self.adopt_snapshot_graph(snapshot);
                self.peers.chrome_dirty = true;
                self.session_picker_dirty = true;
                &self.peers.hosts
            }
            _ => &[],
        };
        let answers = host_answers(explained_by);
        let surfaced = unexplained_unreachable_notices(held, explained_by);
        self.apply_notices(federation_notices(surfaced), repaint);
        answers
    }

    /// Close the in-flight host-inventory request and hand back the
    /// notices held for it.
    pub(super) fn end_host_inventory_request(&mut self) -> Vec<String> {
        self.peers.hosts_pending = None;
        self.peers.hosts_pending_since = None;
        std::mem::take(&mut self.peers.held_unreachable)
    }

    /// True when a snapshot carries windows or resources not yet adopted.
    /// Empty lists are not a change (inventory replies often omit the graph).
    pub(super) fn snapshot_graph_changed(
        &self,
        snapshot: &phux_protocol::wire::info::SessionSnapshot,
    ) -> bool {
        (!snapshot.windows.is_empty() && self.peers.windows != snapshot.windows)
            || (!snapshot.resources.is_empty() && self.peers.resources != snapshot.resources)
    }

    /// Cache windows/resources from a snapshot when it actually carries them.
    pub(super) fn adopt_snapshot_graph(
        &mut self,
        snapshot: &phux_protocol::wire::info::SessionSnapshot,
    ) {
        if !snapshot.windows.is_empty() {
            self.peers.windows.clone_from(&snapshot.windows);
        }
        if !snapshot.resources.is_empty() {
            self.peers.resources.clone_from(&snapshot.resources);
        }
    }

    /// Fold windows/resources carried on an ATTACHED outcome.
    pub(super) fn fold_inventory(
        &mut self,
        inventory: Option<(
            Vec<phux_protocol::wire::info::WindowInfo>,
            Vec<phux_protocol::wire::info::ResourceInfo>,
        )>,
    ) {
        let Some((windows, resources)) = inventory else {
            return;
        };
        if !windows.is_empty() {
            self.peers.windows = windows;
        }
        if !resources.is_empty() {
            self.peers.resources = resources;
        }
    }

    /// Apply a `phux.session.name/v1` broadcast to the cached graph and
    /// (when it names this client's session) the status-bar name.
    pub(super) fn fold_session_rename(
        &mut self,
        outcome: &mut FrameOutcome,
        repaint: &mut RepaintAccumulator,
    ) {
        let Some((current, new_name)) = outcome.session_rename.take() else {
            return;
        };
        apply_graph_rename(&mut self.peers.sessions, &current, &new_name);
        self.peers.chrome_dirty = true;
        self.session_picker_dirty = true;
        self.note_chrome_change(repaint);
    }

    /// Apply a `phux.session.project/v1` broadcast to the picker tags.
    pub(super) fn fold_project_tag_outcome(&mut self, outcome: &mut FrameOutcome) {
        let stored = match std::mem::take(&mut outcome.project_tag) {
            ProjectTagFrame::Absent => return,
            ProjectTagFrame::Cleared => None,
            ProjectTagFrame::Set { name, project } => Some((name, project)),
        };
        self.peers.stored_project_tag = stored;
        self.apply_stored_project_tag();
    }

    /// The `GET_STATE` barrier after a local rename: the snapshot is
    /// authoritative, so a refused write leaves the current name in place.
    pub(super) fn confirm_session_rename(
        &mut self,
        result: &CommandResult,
        repaint: &mut RepaintAccumulator,
    ) {
        match result {
            CommandResult::OkWith(CommandValue::State(snapshot)) => {
                let Some(pending) = self.rename_pending.take() else {
                    return;
                };
                self.peers.sessions.clone_from(&snapshot.sessions);
                self.adopt_snapshot_graph(snapshot);
                if let Some(id) = pending.session_id.or(self.peers.focused_session) {
                    let roster: Vec<_> = snapshot
                        .sessions
                        .iter()
                        .map(|session| phux_client::rename::NamedSession {
                            id: session.id,
                            name: session.name.as_str(),
                        })
                        .collect();
                    if let Some(info) = snapshot.sessions.iter().find(|session| session.id == id) {
                        self.mirror.session_name.clone_from(&info.name);
                    }
                    if let Some(reason) =
                        phux_client::rename::barrier_verdict(&roster, id, &pending.new_name)
                            .refusal_reason()
                    {
                        self.apply_notices(
                            vec![Notice::warn(format!(
                                "could not rename session to {}: {reason}",
                                pending.new_name
                            ))],
                            repaint,
                        );
                    }
                }
                self.peers.chrome_dirty = true;
                self.session_picker_dirty = true;
                self.note_chrome_change(repaint);
            }
            CommandResult::Error { message, .. } => {
                self.fail_session_rename(message, repaint);
            }
            _ => {
                self.fail_session_rename("the server did not confirm the session rename", repaint);
            }
        }
    }

    /// A refused or unanswered rename barrier: keep the current name.
    pub(super) fn fail_session_rename(&mut self, message: &str, repaint: &mut RepaintAccumulator) {
        let pending = self.rename_pending.take();
        let text = pending.as_ref().map_or_else(
            || format!("could not rename session: {message}"),
            |pending| {
                format!(
                    "could not rename session to {}: {message}",
                    pending.new_name
                )
            },
        );
        self.apply_notices(vec![Notice::warn(text)], repaint);
    }

    /// Past [`HOST_INVENTORY_DEADLINE`], free the inventory slot and surface
    /// every notice held for it.
    pub(super) fn expire_overdue_host_inventory(&mut self) {
        let now = std::time::Instant::now();
        if !host_inventory_overdue(self.peers.hosts_pending_since, now) {
            return;
        }
        let held = self.end_host_inventory_request();
        self.show_notices(federation_notices(held));
    }

    /// Inventory and replay command replies share the connection, not projection state.
    pub(super) async fn intercept_inventory_reply(
        &mut self,
        conn: &mut Connection,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<FrameKind>, AttachError> {
        let Some(frame) = self.take_rename_reply(frame, repaint) else {
            return Ok(None);
        };
        let Some(frame) = self.take_host_inventory_reply(conn, frame, repaint).await? else {
            return Ok(None);
        };
        self.take_input_replay_reply(conn, frame, repaint).await
    }

    /// The session-rename barrier's confirmation or refusal.
    fn take_rename_reply(
        &mut self,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Option<FrameKind> {
        match frame {
            FrameKind::CommandResult { request_id, result }
                if self
                    .rename_pending
                    .as_ref()
                    .is_some_and(|pending| pending.barrier == request_id) =>
            {
                self.confirm_session_rename(&result, repaint);
                None
            }
            FrameKind::Error {
                request_id: Some(request_id),
                message,
                ..
            } if self
                .rename_pending
                .as_ref()
                .is_some_and(|pending| pending.barrier == request_id) =>
            {
                self.fail_session_rename(&message, repaint);
                None
            }
            other => Some(other),
        }
    }

    /// The host-inventory GET, its refusal, and unreachable pushes held for it.
    async fn take_host_inventory_reply(
        &mut self,
        conn: &mut Connection,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<FrameKind>, AttachError> {
        match frame {
            // The reply to our own host-inventory GET_STATE.
            FrameKind::CommandResult { request_id, result }
                if self.peers.hosts_pending == Some(request_id) =>
            {
                // A satellite this inventory could not list
                // forgets its strays; one it reached gets their kills.
                let asked_at = self.peers.hosts_pending_since;
                let answers = self.fold_host_inventory(&result, repaint);
                self.replay_returned_satellites(conn, &answers).await?;
                self.retry_after_inventory(conn, &answers, asked_at).await?;
                Ok(None)
            }
            // Its refusal: keep the old inventory, free the slot, surface every
            // held notice.
            FrameKind::Error {
                request_id: Some(request_id),
                ..
            } if self.peers.hosts_pending == Some(request_id) => {
                let held = self.end_host_inventory_request();
                self.apply_notices(federation_notices(held), repaint);
                Ok(None)
            }
            // Un-correlated `SatelliteUnreachable` pushes while our inventory is
            // in flight are held (see `PeerWatch::held_unreachable`).
            FrameKind::Error {
                request_id: None,
                code: phux_protocol::wire::frame::ErrorCode::SatelliteUnreachable,
                message,
            } if self.peers.hosts_pending.is_some() => {
                self.hold_unreachable_during_inventory(message);
                Ok(None)
            }
            other => Ok(Some(other)),
        }
    }

    /// Grey panes a push says are down, and hold the notice for the inventory.
    fn hold_unreachable_during_inventory(&mut self, message: String) {
        // Grey the panes now. The inventory reply still
        // decides the notice, but the layout slot is already down.
        if crate::attach::pane_state::note_satellite_unreachable(&mut self.mirror.panes, &message) {
            self.peers.chrome_dirty = true;
        }
        self.peers.held_unreachable.push(message);
    }

    /// ADR-0053: the reply to a journal `APPLY_INPUT` attempt.
    async fn take_input_replay_reply(
        &mut self,
        conn: &mut Connection,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<FrameKind>, AttachError> {
        match frame {
            FrameKind::CommandResult { request_id, result }
                if self
                    .input_replay
                    .as_ref()
                    .is_some_and(|journal| journal.borrow().owns(request_id)) =>
            {
                self.resolve_input_replay(conn, request_id, &result, repaint)
                    .await?;
                Ok(None)
            }
            other => Ok(Some(other)),
        }
    }

    pub(super) async fn retry_after_inventory(
        &mut self,
        conn: &mut Connection,
        answers: &HostAnswers,
        asked_at: Option<std::time::Instant>,
    ) -> Result<(), AttachError> {
        self.orphan_kills.forget_hosts(&answers.unreachable);
        let Some(asked_at) = asked_at else {
            return Ok(());
        };
        self.retry_stray_kills(conn, &answers.reachable, asked_at)
            .await
    }
}
