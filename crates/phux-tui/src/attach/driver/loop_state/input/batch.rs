//! Stdin parsing, dispatch batches, and post-input side effects.

use super::super::{
    AttachError, CarriedSidebar, Connection, FrameKind, LoopExit, SidebarReservation, Step,
    input_expects_a_reply, view_rects,
};
use super::PriorView;

impl super::super::SessionLoop {
    /// One stdin read: EOF detaches cleanly, bytes become an input batch.
    pub(in crate::attach::driver::loop_state) async fn on_stdin<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        read: std::io::Result<usize>,
    ) -> Result<Step, AttachError> {
        let n = read.map_err(AttachError::Io)?;
        if n == 0 {
            // Stdin EOF — outer terminal closed. Detach cleanly.
            if !self.detach_pending {
                conn.send(&FrameKind::Detach).await?;
                conn.unbind_all_terminals();
                self.pending_stream_binds.clear();
                self.detach_pending = true;
            }
            return Ok(Step::Continue);
        }
        let mut events = std::mem::take(&mut self.input_events);
        self.parser.feed_into(&self.stdin_buf[..n], &mut events);
        self.dispatch_batch(conn, out, sidebar, events).await
    }

    /// The bare-ESC flush runs the same batch handling as a stdin read.
    pub(in crate::attach::driver::loop_state) async fn on_esc_flush<
        W: crate::attach::RenderSink,
    >(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) -> Result<Step, AttachError> {
        let mut events = std::mem::take(&mut self.input_events);
        self.parser.flush_into(&mut events);
        self.dispatch_batch(conn, out, sidebar, events).await
    }

    /// Dispatch one batch of decoded input events, then reflow and repaint
    /// whatever it moved. The buffer comes back drained (see `input_events`).
    pub(in crate::attach::driver::loop_state) async fn dispatch_batch<
        W: crate::attach::RenderSink,
    >(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        mut events: Vec<phux_protocol::input::InputEvent>,
    ) -> Result<Step, AttachError> {
        // Common (a partial CSI, an idle flush); nothing below can change.
        if events.is_empty() {
            self.input_events = events;
            return Ok(Step::Continue);
        }
        self.note_host_focus(&events);
        // Computed before dispatch drains `events`; armed after, so it keys to
        // the pane focus landed on.
        let expects_reply = events.iter().any(input_expects_a_reply);
        // Pre-dispatch view, so zoom and sidebar toggles, and a resized or
        // rearranged window, can reflow PTYs.
        let rects = view_rects(
            &self.mirror.workspace,
            self.mirror.zoomed.as_ref(),
            self.content(sidebar),
            self.viewport_dims,
        );
        let prior = PriorView {
            zoomed: self.mirror.zoomed.clone(),
            sidebar,
            active_window: self.mirror.workspace.active,
            floating: crate::attach::floating::floating_pane(&self.mirror.panes).is_some(),
            overlay: self.overlays.is_active(),
            viewports: self.viewport_offsets(&rects),
            rects,
        };
        // Batch this read's wire writes into one; released before parking.
        conn.cork();
        let dispatched = self.dispatch_input(conn, out, sidebar, &mut events).await;
        // Uncork on both paths so an error never strands emitted frames.
        let shipped = conn.uncork().await;
        if shipped.is_ok() {
            phux_perf::echo_hop::record("send");
        }
        self.input_events = events;
        let layout_changed = dispatched?;
        shipped?;
        // Reply grace keyed to the focused pane only, so a flood elsewhere
        // stays paced. Cleared by time, never by a paint.
        if expects_reply {
            self.pacer.note_input(
                self.mirror.focused_resource.as_ref(),
                tokio::time::Instant::now(),
            );
        }
        // A `toggle-sidebar` in this batch takes effect this iteration.
        let sidebar = self.sidebar();
        if let Some(exit) = self.take_session_switch() {
            return Ok(exit);
        }
        self.reflow_after_input(conn, sidebar, &prior, layout_changed)
            .await?;
        self.finish_input_side_effects(conn, out, sidebar, &prior, layout_changed)
            .await?;
        self.settle_input_arrangement(conn, out, sidebar).await
    }

    /// Tell the bar whether the resolver is waiting at the prefix; true when
    /// that flipped since the last batch.
    fn sync_prefix_chip(&mut self) -> bool {
        let pending = self
            .settings
            .resolver
            .as_ref()
            .is_some_and(phux_config::keybind::Resolver::pending_at_prefix);
        self.settings
            .status_bar
            .as_mut()
            .is_some_and(|bar| bar.set_prefix_pending(pending))
    }

    /// A committed `switch-session` ends this loop before any repaint.
    fn take_session_switch(&mut self) -> Option<Step> {
        let target = self.switch_request.take()?;
        Some(Step::Exit(LoopExit::SwitchTo {
            target,
            sidebar: CarriedSidebar {
                enabled: self.sidebar_enabled,
                width: (self.settings.sidebar.width != self.configured_sidebar_width)
                    .then_some(self.settings.sidebar.width),
            },
            orphan_kills: self.orphans_for_switch(),
            review: std::mem::take(&mut self.review),
            peers: self.peers_for_switch(),
        }))
    }
    /// Watches, chrome, overlay, inventory, config reload, and a host handoff.
    async fn finish_input_side_effects<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        prior: &PriorView,
        layout_changed: bool,
    ) -> Result<(), AttachError> {
        // The prefix chip follows the resolver: a bar-only paint when nothing
        // else in this batch repaints the row.
        let chip_changed = self.sync_prefix_chip();
        if layout_changed {
            // ADR-0040: an input action may have split/closed panes;
            // keep the agent-metadata watches in step with the set.
            self.sync_agent_meta(conn).await?;
            self.refresh_chrome();
            // Redraw only what the batch actually moved: a focus change is
            // chrome, a same-shaped window switch redraws the panes without a
            // whole-screen clear, and only a moved layout clears.
            let level = self.classify_repaint(prior, sidebar);
            self.repaint_view(out, sidebar, level);
        } else if chip_changed {
            self.paint_bar_row(out, sidebar);
        }
        if self.overlays.is_active() {
            self.paint_overlay(out, sidebar);
        }
        // One `GET_STATE` per batch when an action wanted a fresher inventory.
        if std::mem::take(&mut self.host_refresh_request) {
            self.request_host_inventory(conn).await?;
        }
        // Last, so the repaint reflects the new theme/bar.
        if self.reload_request {
            self.reload_request = false;
            self.reload_config(out, sidebar);
        }
        self.handoff_requested_host(conn).await;
        Ok(())
    }

    /// ADR-0140: leave for another machine. Detach first so the server
    /// records a detach rather than a dropped client, then restore the
    /// terminal exactly as a detach does and become `phux attach` there.
    async fn handoff_requested_host(&mut self, conn: &mut Connection) {
        let Some((host, session)) = self.host_switch_request.take() else {
            return;
        };
        let _ = conn.send(&FrameKind::Detach).await;
        super::super::super::terminal::hand_off_to_host(&host, &session);
    }
}
