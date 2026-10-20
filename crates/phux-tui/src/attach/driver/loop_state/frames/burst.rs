//! Inbound burst coalescing and paint settlement.

use super::super::{
    AttachEnd, AttachError, Connection, FRAME_COALESCE_CAP, FrameKind, FrameStep,
    RepaintAccumulator, SidebarReservation, Step, burst_settles_debt, coalesce_defer_flags,
    detached_loop_exit, drain_frame_batch, frame_defers_paint, frame_paint_target,
};

/// Whether this burst may paint, and whether this frame lost the per-pane race.
struct BurstPaint {
    now: bool,
    coalesced: bool,
}

impl super::super::SessionLoop {
    // ---- inbound frames -------------------------------------------------

    /// One `recv` wake-up: a frame to handle, or the end of the connection.
    pub(in crate::attach::driver::loop_state) async fn on_server_frame<
        W: crate::attach::RenderSink,
    >(
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
    pub(in crate::attach::driver::loop_state) async fn handle_frame_burst<
        W: crate::attach::RenderSink,
    >(
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
    pub(in crate::attach::driver::loop_state) async fn apply_frame_step<
        W: crate::attach::RenderSink,
    >(
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
        if let Some(exit) = self
            .settle_arrangement_read(conn, out, sidebar, repaint)
            .await?
        {
            return Ok(exit);
        }
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
        // Hand the peer arrangement reads this burst owes to the reader; the
        // answer arrives on its own `select!` arm.
        self.request_peer_arrangement_reads();
        Ok(Step::Continue)
    }
}
