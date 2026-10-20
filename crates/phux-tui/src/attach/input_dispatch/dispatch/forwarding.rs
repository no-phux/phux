//! Pane input forwarding, paste protection, and predictive echo.

use super::{
    AttachError, EventEnv, InputEvent, Mode, PasteEvent, ResourceId, clear_attention_on_input,
    mint_input_operation_id, predict_now_ms, published_replica, published_terminal, render_overlay,
    send_replay_frames, snap_scrolled_viewport, terminal_in_alt_screen,
};

#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
impl<W: crate::attach::RenderSink> EventEnv<'_, '_, W> {
    /// A key press headed for the pane snaps a scrolled viewport back to the
    /// live screen (tmux), before the predict peek reads the grid.
    pub(super) fn snap_focused_viewport(&mut self) -> bool {
        let target = self.input_target();
        snap_scrolled_viewport(self.ctx.engine_kernel, self.panes, target.as_ref())
    }

    /// The pane keyboard input reaches: the floating overlay while one is
    /// open (ADR-0147), else the active window's focused leaf.
    pub(super) fn input_target(&self) -> Option<ResourceId> {
        crate::attach::floating::floating_pane(self.panes)
            .or_else(|| {
                self.ctx
                    .workspace
                    .active_group()
                    .and_then(|w| w.focus.as_ref())
            })
            .cloned()
    }
    /// Feed a key to predictive echo (mouse/paste/focus bypass it), peeking
    /// the focused pane's grid so arrows can size the grapheme they cross.
    /// ADR-0090: predictions queue on both screens; on the alternate screen
    /// display waits for echo evidence.
    pub(super) fn feed_predict(&mut self, ev: &InputEvent) -> bool {
        use crate::predict::PredictionOutcome;
        let InputEvent::Key(key_event) = ev else {
            return false;
        };
        if !self.predict.is_enabled()
            || crate::attach::floating::floating_pane(self.panes).is_some()
        {
            return false;
        }
        let Some(fid) = self
            .ctx
            .workspace
            .active_group()
            .and_then(|w| w.focus.as_ref())
        else {
            return false;
        };
        let Some(walk) = published_replica(self.ctx.engine_kernel, fid) else {
            return false;
        };
        let Some(slot) = self.panes.get_mut(fid) else {
            return false;
        };
        self.predict
            .set_alt_screen(terminal_in_alt_screen(walk.terminal));
        let outcome = self
            .predict
            .predict_key_with_grid_at(key_event, predict_now_ms(), |r, c| {
                slot.renderer.read_grapheme_at(walk, r, c).ok().flatten()
            });
        matches!(outcome, PredictionOutcome::Predicted)
    }

    /// Ghostty's paste protection: a paste the focused pane would take as
    /// typed input (see [`crate::attach::paste_safety`]) waits behind a
    /// confirmation overlay instead of reaching the pane.
    pub(super) fn hold_unsafe_paste(&mut self, paste: &PasteEvent) -> bool {
        let Some(pane) = self.input_target() else {
            return false;
        };
        let bracketed = published_terminal(self.ctx.engine_kernel, &pane)
            .and_then(|terminal| terminal.mode(Mode::BRACKETED_PASTE).ok())
            .unwrap_or(false);
        if !crate::attach::paste_safety::needs_confirmation(&paste.data, bracketed) {
            return false;
        }
        let lines = crate::attach::paste_safety::line_count(&paste.data);
        self.ctx
            .overlays
            .push(Box::new(crate::render::overlay::PasteConfirmOverlay::new(
                paste.clone(),
                lines,
                bracketed,
                self.ctx.theme,
            )));
        true
    }

    /// Forward key/focus/paste input to the focused pane (ADR-0019). Input
    /// before ATTACHED, or to an exited or unreachable pane, is dropped.
    pub(super) async fn forward_to_focused_pane(
        &mut self,
        ev: InputEvent,
    ) -> Result<bool, AttachError> {
        let Some(pane) = self.input_target() else {
            tracing::debug!("dropping input received before ATTACHED");
            return Ok(false);
        };
        // ADR-0124: a retained pane's process exited; there is nothing to
        // type into, and the server would refuse it anyway.
        if crate::attach::pane_state::pane_exited(self.panes, &pane) {
            tracing::debug!(terminal = ?pane, "dropping input: the pane's process exited");
            return Ok(false);
        }
        // A down satellite keeps its slot and its last
        // snapshot, and takes no new input until the link returns.
        if crate::attach::pane_state::pane_satellite_down(self.panes, &pane) {
            tracing::debug!(terminal = ?pane, "dropping input: satellite unreachable");
            return Ok(false);
        }
        // Typing into a pane answers its pending question; looking does not.
        let layout_changed = matches!(ev, InputEvent::Key(_) | InputEvent::Paste(_))
            && clear_attention_on_input(self.panes, &pane);
        let acknowledged = matches!(ev, InputEvent::Paste(_));
        self.send_terminal_input(pane, ev, acknowledged).await?;
        Ok(layout_changed)
    }

    /// Keep later input behind an acknowledged operation for this Terminal.
    /// Outside that short ordering window, latency-sensitive atoms retain the
    /// ordinary fire-and-forget path.
    pub(super) async fn send_terminal_input(
        &mut self,
        pane: ResourceId,
        event: InputEvent,
        acknowledged: bool,
    ) -> Result<(), AttachError> {
        let Some(journal) = self.ctx.input_replay else {
            return self.conn.send(&event.into_frame(pane)).await;
        };
        let should_queue = pane.host().is_none()
            && ((journal.borrow().active() && acknowledged)
                || journal.borrow().must_order_after(&pane));
        if !should_queue {
            return self.conn.send(&event.into_frame(pane)).await;
        }

        let (reports, frames) = {
            let mut journal = journal.borrow_mut();
            if let Err(report) = journal.submit(mint_input_operation_id(), pane, vec![event]) {
                tracing::warn!(line = %report.notice_line(), "acknowledged input refused locally");
                return Ok(());
            }
            journal.next_frames(&mut *self.ctx.next_request_id)
        };
        journal.borrow_mut().defer_reports(reports);
        send_replay_frames(self.conn, journal, &frames).await
    }

    /// Paint queued predictions at the focused pane's origin when the
    /// ADR-0090 display policy allows.
    pub(super) fn paint_predictions(&mut self) {
        if !self.predict.should_display(predict_now_ms()) {
            return;
        }
        let focused = self
            .ctx
            .workspace
            .active_group()
            .and_then(|w| w.focus.as_ref());
        let origin = focused
            .and_then(|fid| self.panes.get(fid))
            .map_or((0, 0), |s| s.renderer.last_origin());
        let _ = render_overlay(self.predict, origin, self.out);
        // The guesses now sit over the focused pane's cells; its
        // front buffer must not keep claiming what was there before them.
        if let Some(slot) = focused.and_then(|fid| self.panes.get_mut(fid)) {
            crate::attach::pane_state::invalidate_predicted_rows(slot, self.predict);
        }
    }
}
