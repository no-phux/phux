//! Stdin batches and the outer-terminal resize.

use super::{
    AttachError, CarriedSidebar, Connection, DispatchCtx, FrameKind, HashMap, LoopExit, Notice,
    RepaintLevel, ResourceId, SidebarReservation, StatusBarPainter, Step,
    current_viewport_or_default, dispatch_input_events, emit_moved_tiles, emit_view_reflow,
    host_cell_px, input_expects_a_reply, resize_cell_px, undelivered_notices, view_rects,
    viewport_resize_frame,
};

/// The view geometry captured before a batch, so a later reflow can diff it.
struct PriorView {
    zoomed: Option<ResourceId>,
    sidebar: Option<SidebarReservation>,
    active_window: usize,
    rects: HashMap<ResourceId, crate::layout::Rect>,
}

impl super::SessionLoop {
    // ---- input ---------------------------------------------------------

    /// One stdin read: EOF detaches cleanly, bytes become an input batch.
    pub(super) async fn on_stdin<W: crate::attach::RenderSink>(
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
    pub(super) async fn on_esc_flush<W: crate::attach::RenderSink>(
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
    pub(super) async fn dispatch_batch<W: crate::attach::RenderSink>(
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
        // Computed before dispatch drains `events`; armed after, so it keys to
        // the pane focus landed on.
        let expects_reply = events.iter().any(input_expects_a_reply);
        // Pre-dispatch view, so zoom and sidebar toggles, and a resized or
        // rearranged window, can reflow PTYs.
        let prior = PriorView {
            zoomed: self.mirror.zoomed.clone(),
            sidebar,
            active_window: self.mirror.workspace.active,
            rects: view_rects(
                &self.mirror.workspace,
                self.mirror.zoomed.as_ref(),
                self.content(sidebar),
                self.viewport_dims,
            ),
        };
        // Batch this read's wire writes into one; released before parking.
        conn.cork();
        let dispatched = self.dispatch_input(conn, out, sidebar, &mut events).await;
        // Uncork on both paths so an error never strands emitted frames.
        let shipped = conn.uncork().await;
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
        self.finish_input_side_effects(conn, out, sidebar, layout_changed)
            .await?;
        Ok(Step::Continue)
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
        }))
    }

    /// Resize PTYs whose tile changed because this batch moved the view.
    async fn reflow_after_input(
        &self,
        conn: &mut Connection,
        sidebar: Option<SidebarReservation>,
        prior: &PriorView,
        layout_changed: bool,
    ) -> Result<(), AttachError> {
        // Resize PTYs when client-local geometry (zoom, sidebar) changed.
        if self.mirror.zoomed != prior.zoomed || sidebar != prior.sidebar {
            emit_view_reflow(
                conn,
                &self.mirror.workspace,
                self.mirror.zoomed.as_ref(),
                &prior.rects,
                self.content(sidebar),
                self.resize_cell_px,
            )
            .await?;
        } else if layout_changed
            && self.mirror.workspace.active == prior.active_window
            && !self.detach_pending
        {
            // This window's own layout moved (`resize-pane`, a divider drag,
            // a swap): resize exactly the panes whose tile changed. A switch
            // to another window is not a resize (bootstrap sized its panes),
            // and nothing may follow a sent `DETACH`.
            let rects = view_rects(
                &self.mirror.workspace,
                self.mirror.zoomed.as_ref(),
                self.content(sidebar),
                self.viewport_dims,
            );
            emit_moved_tiles(conn, &prior.rects, &rects, self.resize_cell_px).await?;
        }
        Ok(())
    }

    /// Watches, chrome, overlay, inventory, config reload, and a host handoff.
    async fn finish_input_side_effects<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        layout_changed: bool,
    ) -> Result<(), AttachError> {
        if layout_changed {
            // ADR-0040: an input action may have split/closed panes;
            // keep the agent-metadata watches in step with the set.
            self.sync_agent_meta(conn).await?;
            self.refresh_chrome();
            // Restores pane content under a just-dismissed overlay.
            self.repaint_view(out, sidebar, RepaintLevel::Full);
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
        super::super::terminal::restore_terminal_for_handoff();
        crate::attach::hosts::exec_switch_host(&host, &session);
    }

    /// Build the dispatch context and run the batch through the resolver,
    /// the overlay stack, and the pane input pipe.
    pub(super) async fn dispatch_input<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        events: &mut Vec<phux_protocol::input::InputEvent>,
    ) -> Result<bool, AttachError> {
        // Only a mouse batch can hit-test the sidebar; skip cloning its
        // targets otherwise.
        let sidebar_targets = if events
            .iter()
            .any(|ev| matches!(ev, phux_protocol::input::InputEvent::Mouse(_)))
        {
            self.sidebar_painter.click_targets()
        } else {
            crate::render::chrome::sidebar::SidebarTargets::default()
        };
        let mut ctx = DispatchCtx {
            control_dial: self.control_dial.as_deref(),
            engine_kernel: &mut self.mirror.engine_kernel,
            resolver: self.settings.resolver.as_mut(),
            focus_history: self.focus_history.clone(),
            workspace: &mut self.mirror.workspace,
            layout_read_complete: self.layout_read_complete,
            viewport: self.viewport_dims,
            cell_px: self.cell_px_dims,
            next_request_id: &mut self.next_request_id,
            spawn_initial_size_supported: self.spawn_initial_size_supported,
            pending_splits: &mut self.mirror.pending_splits,
            pending_windows: &mut self.mirror.pending_windows,
            pending_floating: &mut self.mirror.pending_floating,
            directory_support: self.directory_support,
            pending_directory: &mut self.pending_directory,
            path_query_supported: self.path.supported,
            pending_path: &mut self.path.pending,
            own_client_id: self.own_client_id,
            expected_closes: &mut self.mirror.expected_closes,
            pending_kills: &mut self.mirror.pending_resource_ops,
            overlays: &mut self.overlays,
            keybindings: self.settings.keybindings.as_ref(),
            theme: &self.settings.theme,
            peers: self.peers.inputs(&self.review),
            host_refresh_request: &mut self.host_refresh_request,
            session_name: &mut self.mirror.session_name,
            rename_pending: &mut self.rename_pending,
            rename_notice: &mut self.rename_notice,
            switch_request: &mut self.switch_request,
            detach_pending: &mut self.detach_pending,
            zoomed: &mut self.mirror.zoomed,
            sidebar,
            sidebar_enabled: &mut self.sidebar_enabled,
            sidebar_width: &mut self.settings.sidebar.width,
            chrome: self.settings.chrome,
            sidebar_targets: &sidebar_targets,
            bar: self
                .settings
                .status_bar
                .as_ref()
                .map(StatusBarPainter::position),
            status_bar: self.settings.status_bar.as_ref(),
            drag: &mut self.drag,
            mouse_optout: &mut self.mouse_optout,
            attention_navigation: &mut self.attention_navigation,
            plugin_actions: &self.settings.plugin_actions,
            plugin_panes: &self.settings.plugin_panes,
            plugin_tx: Some(&self.plugin_tx),
            reload_request: &mut self.reload_request,
            host_switch_request: &mut self.host_switch_request,
            agent_meta: &self.mirror.agent_meta.records,
            vcs: &mut self.vcs,
            input_replay: self.input_replay.as_deref(),
        };
        let mut layout_changed = dispatch_input_events(
            out,
            conn,
            events,
            &mut self.mirror.focused_resource,
            &mut self.mirror.predict,
            &mut self.mirror.panes,
            &mut ctx,
        )
        .await?;
        self.focus_history = ctx.focus_history;
        let reports = self
            .input_replay
            .as_ref()
            .map_or_else(Vec::new, |journal| journal.borrow_mut().take_reports());
        layout_changed |= self.show_notices(undelivered_notices(reports));
        if let Some(line) = self.rename_notice.take() {
            layout_changed |= self.show_notices([Notice::warn(line)]);
        }
        Ok(layout_changed)
    }

    /// Adopt the outer terminal's new size: tell the server, reflow every
    /// PTY, and rebuild the viewport from the authoritative snapshot.
    pub(super) async fn on_resize<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) -> Result<(), AttachError> {
        let viewport = current_viewport_or_default();
        self.viewport_dims = (viewport.cols.max(1), viewport.rows.max(1));
        self.cell_px_dims = host_cell_px(&viewport);
        // Predictions are pane-local: bound them to the focused pane's grid.
        let (predict_cols, predict_rows) = self
            .mirror
            .focused_resource
            .as_ref()
            .and_then(|fid| self.mirror.panes.get(fid))
            .map_or((viewport.cols, viewport.rows), |slot| slot.geometry);
        self.mirror.predict.set_viewport(predict_cols, predict_rows);
        self.resize_cell_px = resize_cell_px(self.sizes_panes_itself, &viewport);
        // ADR-0145: a client that sizes its own panes casts no vote, so the
        // outer size never reaches a pane before its tile does. An older
        // server still needs the vote for cell pixels (a pixel-only SIGWINCH
        // too); the pane targets below then reassert the tiles.
        if !self.sizes_panes_itself {
            conn.send(&viewport_resize_frame(viewport)).await?;
        }
        self.size_workspace_panes(conn, sidebar).await?;
        // Clear rather than repaint stale pre-resize mirrors; the server's
        // resync snapshot repopulates at the new size.
        let _ = out.write_all(b"\x1b[2J\x1b[H");
        crate::attach::pane_state::invalidate_all_fronts(&mut self.mirror.panes);
        // A pointer-pinned overlay (context menu) is stale now: drop it before
        // the repaint so it cannot capture keys invisibly.
        if self.overlays.dismiss_stale_on_resize() {
            tracing::debug!("resize: dropped a pinned overlay whose geometry went stale");
        }
        if self.overlays.is_active() {
            // Survivors adopt the focused pane's new size (copy mode clamps
            // against it).
            self.sync_overlays(sidebar);
            self.paint_overlay(out, sidebar);
        } else {
            let _ = out.flush();
        }
        Ok(())
    }
}
