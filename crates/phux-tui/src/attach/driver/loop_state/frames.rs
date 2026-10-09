//! Inbound frame bursts and what one outcome does to the view.

mod clipboard;
use super::{
    AttachEnd, AttachError, Connection, FRAME_COALESCE_CAP, FrameEnv, FrameKind, FrameOutcome,
    FrameStep, HashMap, MAX_PENDING_STREAM_BINDS, Notice, RepaintAccumulator, ResourceId,
    SidebarReservation, Step, attach_participants, burst_settles_debt, coalesce_defer_flags,
    detached_loop_exit, drain_frame_batch, fleet_projection_dirty, frame_defers_paint,
    frame_paint_target, handle_server_frame, path_picker, send_unless_peer_gone,
    should_emit_frame_ack, spawned_satellite_panes, undelivered_notices,
};

/// Whether this burst may paint, and whether this frame lost the per-pane race.
struct BurstPaint {
    now: bool,
    coalesced: bool,
}

impl super::SessionLoop {
    /// Hand one inbound frame to the shared server-frame handler.
    pub(super) fn handle_frame<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        frame: FrameKind,
        sidebar: Option<SidebarReservation>,
        defer_paint: bool,
    ) -> Result<FrameOutcome, AttachError> {
        let retired_terminal = match &frame {
            FrameKind::ResourceClosed { terminal_id, .. } => Some(terminal_id.clone()),
            _ => None,
        };
        let env = FrameEnv {
            focused_session: self.peers.focused_session,
            status_bar: self.settings.status_bar.as_mut(),
            sidebar,
            viewport_dims: self.viewport_dims,
            pending_layout_request: self.layout_get_request_id,
            overlay_active: self.overlays.is_active(),
            defer_paint,
        };
        let mut outcome = handle_server_frame(&mut self.mirror, env, out, frame)?;
        self.deliver_clipboard_writes(out, &mut outcome)?;
        for terminal_id in &outcome.authoritative_damage {
            let fenced = self
                .input_replay
                .as_ref()
                .is_some_and(|journal| journal.borrow().delivery_fenced(terminal_id));
            if fenced {
                self.delivery_fence_paint_pending
                    .insert(terminal_id.clone());
                if outcome.painted_output.as_ref() != Some(terminal_id) {
                    self.pacer.withhold(terminal_id);
                }
            }
        }
        if let Some(terminal_id) = outcome.painted_output.as_ref() {
            self.clear_delivery_fence_after_paint(terminal_id);
        }
        if let Some(terminal_id) = retired_terminal {
            self.review.forget(&terminal_id);
            self.delivery_fence_paint_pending.remove(&terminal_id);
            if let Some(journal) = self.input_replay.as_ref() {
                let reports = journal
                    .borrow_mut()
                    .retire_terminal(&terminal_id, "the terminal closed");
                outcome.notices.extend(undelivered_notices(reports));
            }
        }
        if outcome.layout_get_answered {
            self.layout_read_complete = true;
            self.layout_get_request_id = None;
        }
        Ok(outcome)
    }

    // ---- inbound frames -------------------------------------------------

    /// One `recv` wake-up: a frame to handle, or the end of the connection.
    pub(super) async fn on_server_frame<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        frame: Result<FrameKind, AttachError>,
    ) -> Result<Step, AttachError> {
        match frame {
            Ok(first) => self.handle_frame_burst(conn, out, sidebar, first).await,
            Err(AttachError::Disconnected) if self.detach_pending => {
                // The socket closed after we asked to detach: clean shutdown.
                Ok(Step::Exit(detached_loop_exit(
                    AttachEnd::Detached { reason: None },
                    true,
                )))
            }
            Err(err) => Err(err),
        }
    }

    /// Apply one coalesced burst of inbound frames and paint it exactly once.
    pub(super) async fn handle_frame_burst<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        first: FrameKind,
    ) -> Result<Step, AttachError> {
        let batch = drain_frame_batch(conn, first)?;
        phux_client::perf::BURST_FRAMES.record(u64::try_from(batch.len()).unwrap_or(u64::MAX));
        if batch.len() == FRAME_COALESCE_CAP {
            phux_client::perf::BURST_CAPPED.incr();
        }
        // Per-pane last-wins: a frame defers its paint iff a later frame in
        // the burst repaints the same pane.
        let defer_flags = coalesce_defer_flags(&batch, frame_paint_target);
        // ADR-0029: triggers raise a level and the accumulator drains once,
        // so a burst of metadata changes costs one chrome paint.
        let mut repaint = RepaintAccumulator::default();
        // One pacing decision per burst: refused, every output frame defers
        // and the `paint_deadline` arm settles them in one composited frame.
        let now = tokio::time::Instant::now();
        let is_reply = self
            .pacer
            .observe_reply(now, batch.iter().filter_map(frame_paint_target));
        let paint_now = self.pacer.admit(now, is_reply);
        for (frame_idx, frame) in batch.into_iter().enumerate() {
            if let Some(exit) = self
                .apply_burst_frame(
                    conn,
                    out,
                    sidebar,
                    frame,
                    BurstPaint {
                        now: paint_now,
                        coalesced: defer_flags[frame_idx],
                    },
                    &mut repaint,
                )
                .await?
            {
                return Ok(exit);
            }
        }
        self.finish_frame_burst(conn, out, sidebar, paint_now, &mut repaint)
            .await
    }

    /// One frame of a burst: bind streams, fold peer replies, then apply it.
    /// `Some` ends the attach; `None` keeps the burst going.
    async fn apply_burst_frame<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        frame: FrameKind,
        paint: BurstPaint,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<Step>, AttachError> {
        let Some(frame) = self.orphan_kills.observe(frame) else {
            return Ok(None);
        };
        if self.coordinate_multistream_frame(conn, &frame).await? {
            self.pending_attach_ready = Some(frame);
            return Ok(None);
        }
        let Some(frame) = self.intercept_peer_reply(conn, frame, repaint).await? else {
            return Ok(None);
        };
        let defer_paint = !paint.now || frame_defers_paint(paint.coalesced, &frame);
        if !paint.now
            && let Some(target) = frame_paint_target(&frame)
        {
            self.pacer.withhold(target);
        }
        if let Some(exit) = self
            .apply_frame_step(conn, out, sidebar, frame, defer_paint, repaint)
            .await?
        {
            return Ok(Some(exit));
        }
        self.release_held_attach_ready(conn, out, sidebar, repaint)
            .await
    }

    /// Apply one frame and surface an exit, if this frame ended the attach.
    async fn apply_frame_step<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        frame: FrameKind,
        defer_paint: bool,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<Step>, AttachError> {
        match self
            .apply_server_frame(conn, out, sidebar, frame, defer_paint, repaint)
            .await?
        {
            FrameStep::Done | FrameStep::Rebootstrap => Ok(None),
            FrameStep::Exit(exit) => Ok(Some(Step::Exit(exit))),
        }
    }

    /// Release an `ATTACH_READY` held until every Terminal stream settled.
    async fn release_held_attach_ready<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<Step>, AttachError> {
        if self.pending_attach_ready.is_some()
            && self.mirror.engine_kernel.attach_ready_pending() == Some(0)
            && let Some(ready) = self.pending_attach_ready.take()
        {
            return self
                .apply_frame_step(conn, out, sidebar, ready, false, repaint)
                .await;
        }
        Ok(None)
    }

    /// Paint the burst once, settle withheld panes, and send the deferred sweep.
    async fn finish_frame_burst<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        paint_now: bool,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Step, AttachError> {
        // Only a burst that did not end the attach may spend.
        self.emit_deferred_bootstrap_outbound(conn).await?;
        self.drain_repaint(out, sidebar, repaint);
        // Settle the pacer's debt here when the timer cannot be trusted (see
        // [`burst_settles_debt`]). After `drain_repaint`, so a full repaint
        // has already cleared what it redrew.
        if burst_settles_debt(
            paint_now,
            self.pacer
                .deadline()
                .is_some_and(|at| at <= tokio::time::Instant::now()),
        ) {
            self.settle_withheld_panes(out, sidebar);
        }
        // The first paint is behind us: send the deferred peer sweep. Gated on
        // reaching the drain, not on a paint, so a quiet attach still sweeps.
        if self.peers.sweep_pending {
            self.peers.sweep_pending = false;
            self.sweep_peer_layouts(conn).await?;
        }
        Ok(Step::Continue)
    }

    /// Apply the QUIC stream lifecycle barriers carried by one control frame.
    /// Returns true when `ATTACH_READY` must be held until every Terminal
    /// stream publishes its own READY/CLOSED outcome.
    pub(super) async fn coordinate_multistream_frame(
        &mut self,
        conn: &mut Connection,
        frame: &FrameKind,
    ) -> Result<bool, AttachError> {
        self.retire_attach_generation(frame);
        if self.bind_answered_terminal_stream(conn, frame).await? || !conn.multistream_enabled() {
            return Ok(false);
        }
        self.bind_open_multistream_frame(conn, frame).await
    }

    /// A new `ATTACHED` drops the previous generation's binds. A failed
    /// command drops the stream it would have bound.
    fn retire_attach_generation(&mut self, frame: &FrameKind) {
        if matches!(frame, FrameKind::Attached { .. }) {
            self.pending_attach_ready = None;
            // A new aggregate generation: retired replies must not bind
            // streams or fold leaves.
            self.pending_stream_binds.clear();
            // Replies to the retired generation's commands can no longer be
            // attributed; a stale entry would fold a live leaf.
            self.mirror.pending_resource_ops.clear();
        }
        if let FrameKind::Error {
            request_id: Some(request_id),
            ..
        } = frame
        {
            self.pending_stream_binds.remove(request_id);
        }
    }

    /// Bind the Terminal stream a command reply just confirmed.
    /// `Ok(true)` means this frame was that reply and the caller should stop.
    async fn bind_answered_terminal_stream(
        &mut self,
        conn: &mut Connection,
        frame: &FrameKind,
    ) -> Result<bool, AttachError> {
        if let FrameKind::CommandResult { request_id, result } = frame
            && let Some(terminal_id) = self.pending_stream_binds.remove(request_id)
        {
            if conn.multistream_enabled()
                && matches!(result, phux_protocol::wire::frame::CommandResult::Ok)
            {
                conn.bind_terminal(&terminal_id).await?;
                // The bootstrap reflow skipped this pane while it had no
                // stream; size it now that it has one.
                self.bind_reflow_owed = true;
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// Bind streams opened by an attach or a local spawn, or report that
    /// `ATTACH_READY` must wait. The caller has established multistream is on.
    async fn bind_open_multistream_frame(
        &self,
        conn: &mut Connection,
        frame: &FrameKind,
    ) -> Result<bool, AttachError> {
        match frame {
            FrameKind::Attached { snapshot, .. } => {
                for terminal_id in attach_participants(snapshot) {
                    conn.bind_terminal(&terminal_id).await?;
                }
            }
            // A local spawn: bind its Terminal stream before the frame's
            // reflow. Satellite spawns bind after their ATTACH_RESOURCE reply.
            FrameKind::ResourceSpawned { request_id, result }
                if self.mirror.pending_splits.contains_key(request_id)
                    || self.mirror.pending_windows.contains_key(request_id)
                    || self.mirror.pending_floating.contains_key(request_id) =>
            {
                if let Some(terminal_id) = result.spawned_id()
                    && terminal_id.is_local()
                {
                    conn.bind_terminal(terminal_id).await?;
                }
            }
            FrameKind::AttachReady { .. } => {
                return Ok(self
                    .mirror
                    .engine_kernel
                    .attach_ready_pending()
                    .is_some_and(|pending| pending > 0));
            }
            _ => {}
        }
        Ok(false)
    }

    /// Fold a peer-scoped reply into the foreign caches (`None`), or hand the
    /// frame on to the general handler.
    pub(super) async fn intercept_peer_reply(
        &mut self,
        conn: &mut Connection,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<FrameKind>, AttachError> {
        let Some(frame) = self
            .intercept_sidebar_metadata(conn, frame, repaint)
            .await?
        else {
            return Ok(None);
        };
        self.intercept_inventory_reply(conn, frame, repaint).await
    }

    /// Correlate the sidebar's metadata reads independently of command replies.
    pub(super) async fn intercept_sidebar_metadata(
        &mut self,
        conn: &mut Connection,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<FrameKind>, AttachError> {
        let Some(frame) = self.take_serving_host_metadata(frame) else {
            return Ok(None);
        };
        let Some(frame) = self.take_project_tag_metadata(frame) else {
            return Ok(None);
        };
        let Some(frame) = self
            .take_foreign_layout_metadata(conn, frame, repaint)
            .await?
        else {
            return Ok(None);
        };
        Ok(self.take_foreign_pane_metadata(frame, repaint))
    }

    /// The sidebar's serving-host read. `None` means this frame was that reply.
    fn take_serving_host_metadata(&mut self, frame: FrameKind) -> Option<FrameKind> {
        match frame {
            FrameKind::MetadataValue { request_id, value }
                if self.peers.serving_host_pending == Some(request_id) =>
            {
                self.fold_serving_host(value.as_deref());
                None
            }
            FrameKind::Error {
                request_id: Some(request_id),
                ..
            } if self.peers.serving_host_pending == Some(request_id) => {
                self.peers.serving_host_pending = None;
                None
            }
            other => Some(other),
        }
    }

    /// The session picker's project-tag read. `None` means this frame was that reply.
    fn take_project_tag_metadata(&mut self, frame: FrameKind) -> Option<FrameKind> {
        match frame {
            FrameKind::MetadataValue { request_id, value }
                if self.peers.project_tag_pending == Some(request_id) =>
            {
                self.fold_project_tag(value.as_deref());
                None
            }
            FrameKind::Error {
                request_id: Some(request_id),
                ..
            } if self.peers.project_tag_pending == Some(request_id) => {
                self.peers.project_tag_pending = None;
                None
            }
            other => Some(other),
        }
    }

    /// A peer session's persisted-layout GET. `None` means this frame was it.
    async fn take_foreign_layout_metadata(
        &mut self,
        conn: &mut Connection,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<FrameKind>, AttachError> {
        match frame {
            FrameKind::MetadataValue { request_id, value }
                if self.peers.foreign_layout_pending.contains_key(&request_id) =>
            {
                self.fold_peer_layout(conn, request_id, value.as_deref(), repaint)
                    .await?;
                Ok(None)
            }
            other => Ok(Some(other)),
        }
    }

    /// Foreign agent-record and attention reads, plus a refused peer read.
    fn take_foreign_pane_metadata(
        &mut self,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Option<FrameKind> {
        match frame {
            // A foreign pane's agent-record GET reply.
            FrameKind::MetadataValue { request_id, value }
                if self.peers.foreign_agent_pending.contains_key(&request_id) =>
            {
                if let Some(id) = self.peers.foreign_agent_pending.remove(&request_id)
                    && self.fold_foreign_agent(&id, value.as_deref())
                {
                    self.peers.chrome_dirty = true;
                    repaint.raise_fleet();
                }
                None
            }
            FrameKind::MetadataValue { request_id, value }
                if self.peers.foreign_asked_pending.contains_key(&request_id) =>
            {
                self.fold_foreign_asked(request_id, value.as_deref(), repaint);
                None
            }
            // A refused peer read (`proto.md` §9): drop the pending entry so
            // the map does not grow and the ERROR does not reach the handler.
            FrameKind::Error {
                request_id: Some(request_id),
                ..
            } if self.peers.foreign_layout_pending.contains_key(&request_id)
                || self.peers.foreign_agent_pending.contains_key(&request_id)
                || self.peers.foreign_asked_pending.contains_key(&request_id) =>
            {
                self.peers.foreign_layout_pending.remove(&request_id);
                self.peers.foreign_agent_pending.remove(&request_id);
                self.peers.foreign_asked_pending.remove(&request_id);
                None
            }
            other => Some(other),
        }
    }

    /// Fold one foreign pane's "asked" bit and raise the fleet when it moved.
    fn fold_foreign_asked(
        &mut self,
        request_id: u32,
        value: Option<&[u8]>,
        repaint: &mut RepaintAccumulator,
    ) {
        let Some(id) = self.peers.foreign_asked_pending.remove(&request_id) else {
            return;
        };
        let asked = value == Some(b"1");
        let changed = if asked {
            self.peers.foreign_attention.insert(id)
        } else {
            self.peers.foreign_attention.remove(&id)
        };
        if changed {
            self.peers.chrome_dirty = true;
            repaint.raise_fleet();
        }
    }

    /// Hand one frame to the server-frame handler and act on everything its
    /// outcome asks for.
    pub(super) async fn apply_server_frame<W: crate::attach::RenderSink>(
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
    pub(super) async fn fold_frame_outcome<W: crate::attach::RenderSink>(
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
        self.fold_project_tag_outcome(&mut outcome);
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
    pub(super) fn leaf_rects(
        &self,
        sidebar: Option<SidebarReservation>,
    ) -> Option<HashMap<ResourceId, crate::layout::Rect>> {
        let ls = self
            .mirror
            .workspace
            .render_window(self.mirror.zoomed.as_ref())?;
        ls.tree.as_ref().map(|_| {
            crate::attach::multi_pane::compute_layout_in(
                ls.as_ref(),
                self.content(sidebar),
                self.viewport_dims,
            )
            .rects
        })
    }

    pub(super) fn track_pending_stream_bind(
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
    pub(super) fn note_chrome_change(&mut self, repaint: &mut RepaintAccumulator) {
        if self.refresh_chrome() && !self.overlays.is_active() {
            repaint.raise_chrome();
        }
    }

    /// Fold the frame's chrome-dirtying signals and its transient notices.
    pub(super) fn fold_chrome_and_notices(
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
    pub(super) fn apply_notices(&mut self, notices: Vec<Notice>, repaint: &mut RepaintAccumulator) {
        if self.show_notices(notices) && !self.overlays.is_active() {
            repaint.raise_chrome();
        }
    }

    /// Send every request the handled frame asked the driver to emit.
    pub(super) async fn emit_outcome_requests(
        &mut self,
        conn: &mut Connection,
        outcome: &mut FrameOutcome,
        sidebar: Option<SidebarReservation>,
        prev_rects: Option<&HashMap<ResourceId, crate::layout::Rect>>,
    ) -> Result<(), AttachError> {
        self.emit_stream_cursor_requests(conn, outcome).await?;
        self.emit_layout_followups(conn, outcome, sidebar, prev_rects)
            .await?;
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
    pub(super) fn settle_frame_view<W: crate::attach::RenderSink>(
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
