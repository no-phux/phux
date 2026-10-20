//! Frame outcome effects, follow-up requests, and repaint signals.

use super::super::{
    AttachEnd, AttachError, Connection, FrameKind, FrameOutcome, FrameStep, HashMap,
    MAX_PENDING_STREAM_BINDS, Notice, RepaintAccumulator, ResourceId, SidebarReservation,
    detached_loop_exit, fleet_projection_dirty, path_picker, send_unless_peer_gone,
    should_emit_frame_ack, spawned_satellite_panes,
};

impl super::super::SessionLoop {
    /// Hand one frame to the server-frame handler and act on everything its
    /// outcome asks for.
    pub(in crate::attach::driver::loop_state) async fn apply_server_frame<
        W: crate::attach::RenderSink,
    >(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        frame: FrameKind,
        defer_paint: bool,
        repaint: &mut RepaintAccumulator,
    ) -> Result<FrameStep, AttachError> {
        // Pre-frame, zoom-honoring leaf rects, so a close/spawn can resize
        // survivors whose dims changed.
        let prev_rects = self.leaf_rects(sidebar);
        let focused_before_frame = self.mirror.focused_resource.clone();
        let outcome = self.handle_frame(out, frame, sidebar, defer_paint)?;
        self.focus_history
            .observe(focused_before_frame, self.mirror.focused_resource.as_ref());
        self.focus_history.repair(
            self.mirror.focused_resource.as_ref(),
            &self.mirror.workspace,
        );
        if outcome.exit {
            let end = outcome
                .exit_reason
                .unwrap_or(AttachEnd::Detached { reason: None });
            return Ok(FrameStep::Exit(detached_loop_exit(
                end,
                self.detach_pending,
            )));
        }
        if outcome.resync_required {
            return self.request_rebootstrap(conn).await;
        }
        self.fold_frame_outcome(conn, out, sidebar, outcome, prev_rects.as_ref(), repaint)
            .await?;
        Ok(FrameStep::Done)
    }

    /// Fold one non-terminal frame outcome into the loop: attach what it
    /// discovered, fold peer, rename, watch, and chrome changes, send the
    /// requests it owes, and raise (never paint) what the burst drain must
    /// repaint. `prev_rects` is the pre-frame leaf geometry a reflow diffs.
    pub(in crate::attach::driver::loop_state) async fn fold_frame_outcome<
        W: crate::attach::RenderSink,
    >(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        mut outcome: FrameOutcome,
        prev_rects: Option<&HashMap<ResourceId, crate::layout::Rect>>,
        repaint: &mut RepaintAccumulator,
    ) -> Result<(), AttachError> {
        self.attach_discovered_panes(conn, &outcome.attach_panes)
            .await?;
        let answered = spawned_satellite_panes(&outcome.adopt_spawned);
        self.attach_spawned_panes(conn, std::mem::take(&mut outcome.adopt_spawned))
            .await?;
        self.settle_orphans(conn, &mut outcome, &answered).await?;
        let fleet_dirty = fleet_projection_dirty(&outcome);
        self.fold_peer_outcome(conn, &mut outcome, repaint).await?;
        self.fold_session_rename(&mut outcome, repaint);
        self.finish_paint(outcome.status_bar_painted);
        self.resync_watches(conn, &mut outcome).await?;
        self.fold_chrome_and_notices(&mut outcome, repaint);
        self.open_directory_picker(out, sidebar, outcome.directory_listing.take());
        let reply = outcome.path_results.take();
        if path_picker::accept_reply(self.path.pending.as_ref(), &mut self.overlays, reply) {
            self.paint_overlay(out, sidebar);
        }
        self.emit_outcome_requests(conn, &mut outcome, sidebar, prev_rects)
            .await?;
        self.settle_frame_view(out, &outcome, sidebar, fleet_dirty, repaint);
        Ok(())
    }

    /// The per-leaf rect map of the zoom- and sidebar-honoring view, or
    /// `None` when there is no window or its tree is unseeded.
    pub(in crate::attach::driver::loop_state) fn leaf_rects(
        &self,
        sidebar: Option<SidebarReservation>,
    ) -> Option<HashMap<ResourceId, crate::layout::Rect>> {
        let ls = self
            .mirror
            .workspace
            .render_group(self.mirror.zoomed.as_ref())?;
        ls.tree.as_ref().map(|_| {
            crate::attach::multi_pane::compute_layout_in(
                ls.as_ref(),
                self.content(sidebar),
                self.viewport_dims,
            )
            .rects
        })
    }

    pub(in crate::attach::driver::loop_state) fn track_pending_stream_bind(
        &mut self,
        request_id: u32,
        terminal_id: ResourceId,
    ) -> Result<(), AttachError> {
        if self.pending_stream_binds.len() >= MAX_PENDING_STREAM_BINDS {
            return Err(AttachError::Protocol(format!(
                "QUIC Terminal stream cap exceeded ({MAX_PENDING_STREAM_BINDS})"
            )));
        }
        self.pending_stream_binds.insert(request_id, terminal_id);
        Ok(())
    }

    /// Refresh the chrome and raise an in-place paint only when it changed.
    pub(in crate::attach::driver::loop_state) fn note_chrome_change(
        &mut self,
        repaint: &mut RepaintAccumulator,
    ) {
        if self.refresh_chrome() && !self.overlays.is_active() {
            repaint.raise_chrome();
        }
    }

    /// Fold the frame's chrome-dirtying signals and its transient notices.
    pub(in crate::attach::driver::loop_state) fn fold_chrome_and_notices(
        &mut self,
        outcome: &mut FrameOutcome,
        repaint: &mut RepaintAccumulator,
    ) {
        // A control/asked event changed chrome only (ADR-0029).
        if outcome.chrome_dirty {
            self.note_chrome_change(repaint);
        }
        if !outcome.notices.is_empty() {
            self.apply_notices(std::mem::take(&mut outcome.notices), repaint);
        }
    }

    /// Drain the frame's transient notices into the bar (expiry rides the
    /// status tick); with no bar they degrade to tracing.
    pub(in crate::attach::driver::loop_state) fn apply_notices(
        &mut self,
        notices: Vec<Notice>,
        repaint: &mut RepaintAccumulator,
    ) {
        if self.show_notices(notices) && !self.overlays.is_active() {
            repaint.raise_chrome();
        }
    }

    /// Send every request the handled frame asked the driver to emit.
    pub(in crate::attach::driver::loop_state) async fn emit_outcome_requests(
        &mut self,
        conn: &mut Connection,
        outcome: &mut FrameOutcome,
        sidebar: Option<SidebarReservation>,
        prev_rects: Option<&HashMap<ResourceId, crate::layout::Rect>>,
    ) -> Result<(), AttachError> {
        self.emit_stream_cursor_requests(conn, outcome).await?;
        self.emit_layout_followups(conn, outcome, sidebar, prev_rects)
            .await?;
        // Another writer changed this session's arrangement record: read it
        // once the burst settles.
        if outcome.arrangement_changed && self.arrangement.is_some() {
            self.arrangement_read_owed = true;
        }
        // A peer's arrangement record changed: re-read it for the peer caches.
        if let Some(session) = outcome.peer_arrangement_changed.take()
            && self.peer_reader.is_some()
        {
            self.peers.arrangement_reads_owed.add(session);
        }
        // Organization events were lost: re-read every arrangement and the
        // session list (names, membership) rather than trust the caches.
        if outcome.organization_resync {
            if self.arrangement.is_some() {
                self.arrangement_read_owed = true;
            }
            if self.peer_reader.is_some() {
                self.peers
                    .arrangement_reads_owed
                    .merge(crate::attach::peer_arrangements::PeerReads::All);
            }
            self.request_host_inventory(conn).await?;
        }
        // The server moved a resource into or out of this session: a fresh
        // graph recomputes the members.
        if outcome.members_dirty {
            self.request_host_inventory(conn).await?;
        }
        // ADR-0147: a floating overlay just opened; state its box size, as a
        // split's reflow does for a new tile.
        if outcome.size_floating {
            self.size_floating_pane(conn, sidebar).await?;
        }
        Ok(())
    }

    /// Send the frame-ack and history-fetch the outcome recorded.
    async fn emit_stream_cursor_requests(
        &self,
        conn: &mut Connection,
        outcome: &mut FrameOutcome,
    ) -> Result<(), AttachError> {
        if let Some((terminal_id, stream_id, bootstrap_id, seq)) =
            should_emit_frame_ack(self.wants_state_sync, outcome.ack.take())
        {
            send_unless_peer_gone(
                conn,
                &FrameKind::FrameAck {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    seq,
                },
            )
            .await?;
        }
        if let Some((terminal_id, stream_id, bootstrap_id, cursor, max_bytes, max_rows)) =
            outcome.history_request.take()
        {
            send_unless_peer_gone(
                conn,
                &FrameKind::HistoryRequest {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    cursor,
                    max_bytes,
                    max_rows,
                },
            )
            .await?;
        }
        Ok(())
    }

    /// Broadcast, clear, or resize after a layout the frame changed.
    async fn emit_layout_followups(
        &mut self,
        conn: &mut Connection,
        outcome: &FrameOutcome,
        sidebar: Option<SidebarReservation>,
        prev_rects: Option<&HashMap<ResourceId, crate::layout::Rect>>,
    ) -> Result<(), AttachError> {
        // A server-driven layout change broadcasts like a local action.
        if outcome.emit_set_metadata {
            self.broadcast_layout(conn).await?;
        }
        if outcome.clear_layout {
            self.clear_stored_layout(conn).await?;
        }
        // `ATTACHED` initially exposes a one-pane fallback. The persisted
        // multi-window layout lands later as this correlated metadata reply.
        // Reflow every restored window here, before `settle_frame_view`
        // schedules its full paint. Doing it earlier can only see the fallback;
        // doing it on first window selection turns that selection into a
        // corrective resize instead of an ordinary paint.
        let bind_reflow_owed = std::mem::take(&mut self.bind_reflow_owed);
        if outcome.layout_get_answered || bind_reflow_owed {
            self.size_workspace_panes(conn, sidebar).await?;
        } else if outcome.reflow_panes
            && let Some(prev_rects) = prev_rects
        {
            self.emit_reflow_resizes(conn, prev_rects, sidebar).await?;
        }
        Ok(())
    }

    /// Fold the frame's view-level consequences: a replaced layout, a changed
    /// agent record, a config-reload doorbell, and the fleet projection.
    pub(in crate::attach::driver::loop_state) fn settle_frame_view<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        outcome: &FrameOutcome,
        sidebar: Option<SidebarReservation>,
        fleet_dirty: bool,
        repaint: &mut RepaintAccumulator,
    ) {
        if outcome.layout_replaced {
            self.on_layout_replaced(sidebar, repaint);
        } else if outcome.layout_get_answered {
            // No persisted layout: still resolve a resource-identity pick
            // against the ATTACHED graph.
            self.resolve_cross_session_pick();
        }
        // ADR-0040: an agent record changed. Fold it into the review index even
        // when identical (so a repeat GET cannot re-arm a reviewed completion);
        // repaint chrome in place only when something moved.
        let review_changed = outcome.agent_meta_terminal.as_ref().is_some_and(|id| {
            self.review.observe_record(
                id,
                self.mirror.agent_meta.records.get(id),
                self.mirror.focused_resource.as_ref(),
            )
        });
        if outcome.agent_meta_changed || review_changed {
            self.note_chrome_change(repaint);
        }
        // The `phux config reload` doorbell rang.
        if outcome.config_reload {
            self.reload_config(out, sidebar);
        }
        // Raised, not called: the fleet refresh repaints over a full frame, so
        // the accumulator collapses a burst into one.
        if fleet_dirty {
            repaint.raise_fleet();
        }
    }
}
