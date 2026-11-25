//! Host inventory and the serving host.

use super::{
    AttachError, CommandResult, CommandValue, Connection, FrameKind, HostAnswers, Notice,
    RepaintAccumulator, federation_notices, host_answers, sync_agent_meta_subscriptions,
    unexplained_unreachable_notices,
};

pub(super) mod answers;
mod graph;

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
        .await?;
        self.watch_notify_modes(conn).await
    }

    /// Refresh peer arrangements (read off the loop with a peer reader, else
    /// each legacy layout key), then peer agent records, serving host, and
    /// host inventory.
    pub(super) async fn sweep_peer_layouts(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        if self.peer_reader.is_some() {
            self.peers
                .arrangement_reads_owed
                .merge(crate::attach::peer_arrangements::PeerReads::All);
        } else {
            self.peers
                .sweep_layouts(conn, &mut self.next_request_id)
                .await?;
        }
        // Agent watches must not wait for a persisted TUI layout.
        self.reconcile_peer_agents(conn).await?;
        // The fleet's other half. Rides the same deferred
        // sweep, so it costs the first paint nothing.
        self.request_sidebar_facts(conn).await
    }

    /// Read the server's identity after first paint without blocking the frame loop.
    pub(super) async fn request_serving_host(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        self.peers
            .request_fact(
                conn,
                &mut self.next_request_id,
                super::super::subscriptions::SidebarFact::ServingHost,
            )
            .await
    }

    /// Invalid or unsupported identity leaves the honest `this server` label.
    pub(super) fn fold_serving_host(&mut self, value: Option<&[u8]>) {
        use phux_protocol::wire::frame::{WHOAMI_SCHEMA_VERSION, WhoamiRecord};
        self.peers.serving_host = value
            .and_then(|bytes| serde_json::from_slice::<WhoamiRecord>(bytes).ok())
            .filter(|r| r.schema_version == WHOAMI_SCHEMA_VERSION && !r.host.trim().is_empty())
            // The hosts provider's spelling, so the machine header does not
            // flip between `mac.local` and `mac` when its listing lands.
            .map(|r| crate::attach::hosts::short_host_label(&r.host).to_owned());
        self.peer_presentation.chrome_dirty = true;
    }

    /// Ask for the federation host inventory (one `GET_STATE`). Skipped
    /// while one is already in flight.
    pub(super) async fn request_host_inventory(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        self.peers
            .request_inventory(conn, &mut self.next_request_id)
            .await
    }

    /// Fold a host-inventory reply. A refusal keeps the previous inventory.
    /// Held unreachable notices the reply explains are dropped; the rest
    /// surface. Returns which satellites it reached and which it could not.
    pub(super) fn fold_host_inventory(
        &mut self,
        result: &phux_protocol::wire::frame::CommandResult,
        held: Vec<String>,
        repaint: &mut RepaintAccumulator,
    ) -> HostAnswers {
        let explained_by: &[phux_protocol::wire::info::HostInventory] = match result {
            phux_protocol::wire::frame::CommandResult::OkWith(
                phux_protocol::wire::frame::CommandValue::State(snapshot),
            ) => {
                self.peers.hosts = snapshot.hosts().to_vec();
                let sessions_changed = self.peers.sessions != snapshot.sessions;
                if sessions_changed {
                    self.adopt_listed_sessions(&snapshot.sessions);
                }
                // Sweep only when the graph the sweep reads actually moved.
                if sessions_changed || self.snapshot_graph_changed(snapshot) {
                    self.peers.sweep_pending = true;
                }
                if self.adopt_snapshot_graph(snapshot) && !self.overlays.is_active() {
                    repaint.raise_full();
                }
                self.peer_presentation.chrome_dirty = true;
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

    /// Whether a snapshot carries windows or resources not yet adopted
    /// (windows only when resources do not name their sessions).
    pub(super) fn snapshot_graph_changed(
        &self,
        snapshot: &phux_protocol::wire::info::SessionSnapshot,
    ) -> bool {
        let joins_by_session = snapshot.resources.iter().any(|r| r.session.is_some());
        (!joins_by_session
            && !snapshot.windows.is_empty()
            && self.peers.windows != snapshot.windows)
            || (!snapshot.resources.is_empty() && self.peers.resources != snapshot.resources)
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
                self.adopt_listed_sessions(&snapshot.sessions);
                if self.adopt_snapshot_graph(snapshot) && !self.overlays.is_active() {
                    repaint.raise_full();
                }
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
                self.peer_presentation.chrome_dirty = true;
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

    /// The reply to a `SESSION_RENAME` (ADR-0161): `COMMITTED` applies the
    /// new name to the status line and the roster; a refusal keeps the
    /// current name and says why.
    pub(super) fn settle_org_session_rename(
        &mut self,
        result: &CommandResult,
        repaint: &mut RepaintAccumulator,
    ) {
        use phux_client_core::org_ops::{OrgOutcome, OrgReplyExpectation};
        let Some(operation) = self.rename_pending.as_ref().and_then(|pending| pending.operation) else {
            self.fail_session_rename("the original rename operation is unavailable", repaint);
            return;
        };
        let outcome = OrgReplyExpectation::Mutation(operation).classify(result);
        let reason = match &outcome {
            OrgOutcome::Committed(_) => None,
            OrgOutcome::Refused(phux_protocol::wire::frame::Refusal::NotFound) => {
                Some("the session no longer exists".to_owned())
            }
            OrgOutcome::Refused(refusal) => Some(phux_client::org::refusal_sentence(refusal)),
            OrgOutcome::Error { message, .. } => Some(message.clone()),
            OrgOutcome::Unknown { reason } => Some(format!(
                "{reason}; operation {} remains unknown; reconcile before retrying",
                operation.to_hex(),
            )),
            _ => Some("the server did not confirm the session rename".to_owned()),
        };
        if let Some(reason) = reason {
            self.fail_session_rename(&reason, repaint);
            return;
        }
        let Some(pending) = self.rename_pending.take() else {
            return;
        };
        if let Some(id) = pending.session_id {
            if let Some(info) = self.peers.sessions.iter_mut().find(|info| info.id == id) {
                info.name.clone_from(&pending.new_name);
            }
            if Some(id) == self.peers.focused_session {
                self.mirror.session_name.clone_from(&pending.new_name);
            }
        }
        self.apply_stored_project_tag();
        self.peer_presentation.chrome_dirty = true;
        self.session_picker_dirty = true;
        self.note_chrome_change(repaint);
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
        if let Some(held) = self.peers.expire_inventory(std::time::Instant::now()) {
            self.show_notices(federation_notices(held));
        }
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
                if self
                    .rename_pending
                    .as_ref()
                    .is_some_and(|pending| pending.organization)
                {
                    self.settle_org_session_rename(&result, repaint);
                } else {
                    self.confirm_session_rename(&result, repaint);
                }
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
        use super::super::subscriptions::InventoryReply;
        // Grey panes immediately; the inventory owner retains the notice until
        // its correlated reply, refusal or deadline explains it.
        if self.peers.inventory_in_flight()
            && let FrameKind::Error {
                request_id: None,
                code: phux_protocol::wire::frame::ErrorCode::SatelliteUnreachable,
                message,
            } = &frame
            && crate::attach::pane_state::note_satellite_unreachable(
                &mut self.mirror.panes,
                message,
            )
        {
            self.peer_presentation.chrome_dirty = true;
        }
        match self.peers.take_inventory_reply(frame) {
            Ok(InventoryReply::Completed {
                result,
                asked_at,
                held,
            }) => {
                let answers = self.fold_host_inventory(&result, held, repaint);
                self.replay_returned_satellites(conn, &answers).await?;
                self.retry_after_inventory(conn, &answers, Some(asked_at))
                    .await?;
                Ok(None)
            }
            Ok(InventoryReply::Refused(held)) => {
                self.apply_notices(federation_notices(held), repaint);
                Ok(None)
            }
            Ok(InventoryReply::HeldUnreachable) => Ok(None),
            Err(frame) => Ok(Some(frame)),
        }
    }

    /// ADR-0053: the reply to a journal `APPLY_INPUT` attempt.
    async fn take_input_replay_reply(
        &mut self,
        conn: &mut Connection,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<FrameKind>, AttachError> {
        // A correlated ERROR settles it too: the journal reads UNSUPPORTED as
        // refused and UNREADABLE_REPLY or a newer code as unknown.
        let owned =
            |id: u32| (self.input_replay.as_ref()).is_some_and(|journal| journal.borrow().owns(id));
        let (request_id, result) = match frame {
            FrameKind::CommandResult { request_id, result } if owned(request_id) => {
                (request_id, result)
            }
            FrameKind::Error {
                request_id: Some(request_id),
                code,
                message,
            } if owned(request_id) => (request_id, CommandResult::Error { code, message }),
            other => return Ok(Some(other)),
        };
        self.resolve_input_replay(conn, request_id, &result, repaint)
            .await?;
        Ok(None)
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
