//! Modal overlay capture, commits, and async picker queries.

use super::{
    AttachError, EventEnv, FrameKind, InputEvent, MouseEvent, OverlayOutcome, StageOutcome,
    copy_search, focused_pane_rect, published_terminal, scroll_focused_pane_viewport,
};

#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
impl<W: crate::attach::RenderSink> EventEnv<'_, '_, W> {
    /// While a (non-passthrough) overlay is up it captures all input: keys and
    /// mouse go to the top overlay, pastes fill its text, focus is dropped.
    /// The resolver is bypassed and reset, so a leader chord typed into a
    /// prompt lands verbatim and a half-chord cannot leak past dismissal.
    pub(super) async fn capture_into_overlay(
        &mut self,
        ev: &InputEvent,
    ) -> Result<StageOutcome, AttachError> {
        if !self.ctx.overlays.is_active() || self.ctx.overlays.top_is_passthrough() {
            return Ok(StageOutcome::PASS);
        }
        let layout_changed = match ev {
            InputEvent::Key(key_event) => self.handle_overlay_key(key_event).await?,
            InputEvent::Mouse(mouse) => self.handle_overlay_mouse(mouse).await?,
            InputEvent::Paste(paste) => {
                if let Some(resolver) = self.ctx.resolver.as_deref_mut() {
                    resolver.reset();
                }
                if let Ok(text) = std::str::from_utf8(&paste.data) {
                    self.ctx.overlays.handle_paste(text);
                }
                self.send_changed_path_query().await?;
                false
            }
            // Focus events are consumed without reaching the pane underneath.
            _ => false,
        };
        Ok(StageOutcome::consumed(layout_changed))
    }

    /// Feed one key event to the top overlay and run whatever it commits.
    pub(super) async fn handle_overlay_key(
        &mut self,
        key_event: &phux_protocol::input::key::KeyEvent,
    ) -> Result<bool, AttachError> {
        if let Some(resolver) = self.ctx.resolver.as_deref_mut() {
            resolver.reset();
        }
        let was_active = self.ctx.overlays.is_active();
        // An overlay may commit an action (e.g. the
        // rename prompt returning `rename-window { name }`); run
        // it through the same path as a keybinding.
        let outcome = self.ctx.overlays.handle_key(key_event);
        self.release_abandoned_listing();
        let ran = self.apply_overlay_outcome(outcome).await?;
        self.send_changed_path_query().await?;
        self.release_abandoned_path();
        // On dismiss, repaint everything: the overlay scribbled
        // over pane cells and we need a coherent base for the
        // next RESOURCE_OUTPUT.
        let dismissed = was_active && !self.ctx.overlays.is_active();
        Ok(ran || dismissed)
    }

    /// Escape on the `go-to-directory` placeholder cancels its listing:
    /// once no stacked overlay awaits the pending request, forget it, so the
    /// late reply is dropped as stale instead of opening a picker.
    pub(super) fn release_abandoned_listing(&mut self) {
        let pending = self
            .ctx
            .pending_directory
            .as_ref()
            .map(|pending| pending.request_id);
        if pending.is_some_and(|id| !self.ctx.overlays.awaits(id)) {
            *self.ctx.pending_directory = None;
        }
    }

    /// Query the serving/satellite host as the search field changes. Every
    /// edit gets a fresh request id, so delayed results cannot replace newer
    /// results or resurrect a dismissed picker.
    pub(super) async fn send_changed_path_query(&mut self) -> Result<(), AttachError> {
        let Some((root, query)) = self.ctx.overlays.path_search() else {
            return Ok(());
        };
        let (root, query) = (root.to_owned(), query.to_owned());
        let Some(pending) = self.ctx.pending_path.as_mut() else {
            return Ok(());
        };
        if pending.root == root && pending.query == query {
            return Ok(());
        }
        let request_id = *self.ctx.next_request_id;
        *self.ctx.next_request_id = request_id.wrapping_add(1);
        pending.request_id = request_id;
        pending.root.clone_from(&root);
        pending.query.clone_from(&query);
        self.conn
            .send(&FrameKind::PathQuery {
                request_id,
                root,
                recursive: !query.is_empty(),
                query,
                host: crate::attach::path_picker::host(pending),
            })
            .await
    }

    pub(super) fn release_abandoned_path(&mut self) {
        if self.ctx.overlays.path_search().is_none() {
            *self.ctx.pending_path = None;
        }
    }

    /// Feed one mouse event to the top overlay and run whatever it commits.
    pub(super) async fn handle_overlay_mouse(
        &mut self,
        mouse: &MouseEvent,
    ) -> Result<bool, AttachError> {
        // Copy mode tracks pane-local cells; modals keep viewport coords.
        let routed = if self.ctx.overlays.copy_selection().is_some() {
            let rect = focused_pane_rect(self.ctx, self.focused_resource.as_ref());
            let mut m = *mouse;
            m.x = (m.x - f64::from(rect.x)).max(0.0);
            m.y = (m.y - f64::from(rect.y)).max(0.0);
            m
        } else {
            *mouse
        };
        let was_active = self.ctx.overlays.is_active();
        let outcome = self.ctx.overlays.handle_mouse(&routed);
        // A pointer-driven copy commit repaints: the selection highlight
        // has to come back off the pane's cells.
        let copy_commit = matches!(outcome, OverlayOutcome::Copy(_));
        let ran = self.apply_overlay_outcome(outcome).await? || copy_commit;
        // Clicking a menu away must repaint the cells it covered.
        let dismissed = was_active && !self.ctx.overlays.is_active();
        Ok(ran || dismissed)
    }

    /// Run one [`OverlayOutcome`] the overlay stack produced, returning
    /// `true` iff it changed layout state the caller must repaint.
    pub(super) async fn apply_overlay_outcome(
        &mut self,
        outcome: OverlayOutcome,
    ) -> Result<bool, AttachError> {
        match outcome {
            OverlayOutcome::RunAction(resolved) => self.run_resolved(&resolved).await,
            OverlayOutcome::Copy(req) => {
                // ADR-0030: resolve against the focused pane and copy via
                // OSC 52; client-local.
                if let Some(fid) = self.focused_resource.as_ref()
                    && let Some(terminal) = published_terminal(self.ctx.engine_kernel, fid)
                {
                    crate::attach::copy::copy_to_host_clipboard(self.out, terminal, req)?;
                }
                Ok(false)
            }
            OverlayOutcome::ScrollViewport(delta) => Ok(scroll_focused_pane_viewport(
                self.ctx.engine_kernel,
                self.panes,
                self.focused_resource.as_ref(),
                delta,
            )),
            // ADR-0101: the settings page wrote the file. The driver owns
            // the settings this batch is still borrowing, so hand the
            // reload up exactly as the `reload-config` action does.
            OverlayOutcome::ReloadConfig => {
                *self.ctx.reload_request = true;
                Ok(false)
            }
            OverlayOutcome::Search(req) => {
                let result = copy_search(
                    self.ctx.engine_kernel,
                    self.panes,
                    self.focused_resource.as_ref(),
                    &req,
                );
                self.ctx.overlays.apply_copy_search(result);
                // The cursor, selection, or viewport moved: repaint.
                Ok(true)
            }
            OverlayOutcome::Paste(paste) => {
                self.forward_to_focused_pane(InputEvent::Paste(paste)).await
            }
            OverlayOutcome::SetClipboard(text) => {
                crate::attach::copy::write_host_clipboard(self.out, &text.0)?;
                Ok(false)
            }
            // Overlay consumed the event but nothing else to do.
            OverlayOutcome::None => Ok(false),
        }
    }
}
