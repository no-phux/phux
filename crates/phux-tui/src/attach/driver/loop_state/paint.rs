//! Chrome, overlays, and the status bar.

use super::{
    ChromeCtx, PaneScene, PluginRunResult, RepaintAccumulator, RepaintLevel, ResourceId,
    SYNC_OUTPUT_WATCHDOG, SidebarReservation, StatusBarPaint, StatusBarPainter, ToastOverlay,
    adopt_config_reload, content_rect, finish_return_onboarding_after_paint, paint_active_overlay,
    paint_chrome_in_place, paint_full_frame, plugin_actions, push_which_key_overlay,
    refresh_window_chrome, sidebar_reservation, sync_overlays_to_focused_pane,
};

impl super::SessionLoop {
    // ---- small shared projections -------------------------------------

    /// The per-frame sidebar reservation; `None` keeps the full viewport.
    pub(super) const fn sidebar(&self) -> Option<SidebarReservation> {
        sidebar_reservation(
            self.viewport_dims.0,
            self.sidebar_enabled,
            self.settings.sidebar.width,
            self.settings.sidebar.edge,
            self.settings.chrome.min_pane_cols,
        )
    }

    /// The row the status bar reserves, if any.
    pub(super) fn bar(&self) -> Option<crate::render::chrome::status_bar::Position> {
        self.settings
            .status_bar
            .as_ref()
            .map(StatusBarPainter::position)
    }

    /// The residual rect panes tile into once the bar and strip are folded off.
    pub(super) fn content(&self, sidebar: Option<SidebarReservation>) -> crate::layout::Rect {
        content_rect(self.viewport_dims, self.bar(), sidebar)
    }

    /// The single chrome-refresh chokepoint, with this driver's inputs bound.
    pub(super) fn refresh_chrome(&mut self) -> bool {
        let rows = crate::attach::agent_rows::agent_session_rows(&self.mirror.engine_kernel);
        self.review
            .observe_streams(&rows, self.mirror.focused_resource.as_ref());
        let mut changed = self.project_window_chrome(self.sidebar(), &rows);
        let (tab_drop, sidebar_drop) = self.drag.as_ref().map_or((None, None), |drag| {
            (drag.tab_drop_at(), drag.sidebar_drop_at())
        });
        if let Some(status_bar) = self.settings.status_bar.as_mut() {
            changed |= status_bar.set_drop_index(tab_drop);
        }
        changed |= self.sidebar_painter.set_drop_index(sidebar_drop);
        changed
    }

    /// Project the window tabs, agent rows, and peer zones into the chrome
    /// painters; true when any painter input changed.
    pub(super) fn project_window_chrome(
        &mut self,
        sidebar: Option<SidebarReservation>,
        rows: &crate::attach::agent_rows::AgentSessionRows,
    ) -> bool {
        let mut chrome = ChromeCtx::new(
            &mut self.settings,
            &mut self.sidebar_painter,
            &self.mirror.session_name,
            self.viewport_dims,
            sidebar,
        );
        let scene = PaneScene {
            workspace: &self.mirror.workspace,
            panes: &self.mirror.panes,
            focused: self.mirror.focused_resource.as_ref(),
            zoomed: self.mirror.zoomed.as_ref(),
        };
        refresh_window_chrome(
            &mut chrome,
            scene,
            self.own_client_id,
            &self.mirror.agent_meta,
            &mut self.vcs,
            rows,
            self.peers.inputs(&self.review),
        )
    }

    /// Commit attach onboarding once its notice has reached the render sink.
    pub(super) fn finish_paint(&mut self, painted: StatusBarPaint) {
        finish_return_onboarding_after_paint(
            &mut self.onboarding_claim,
            self.settings.status_bar.as_ref(),
            painted,
        );
    }

    /// Paint the view at `level` (`Chrome` in place, `Full` recomposite),
    /// unless an overlay owns the screen.
    pub(super) fn repaint_view<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        level: RepaintLevel,
    ) {
        if self.overlays.is_active() {
            return;
        }
        if let Some(painted) = self.paint_view(out, sidebar, level) {
            self.finish_paint(painted);
            if matches!(level, RepaintLevel::Full) {
                self.clear_visible_delivery_fences_after_paint();
            }
        }
    }

    /// The paint half of [`Self::repaint_view`]. `None` ⇒ there was no window
    /// to render.
    pub(super) fn paint_view<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        level: RepaintLevel,
    ) -> Option<StatusBarPaint> {
        let Some(ls) = self
            .mirror
            .workspace
            .render_window(self.mirror.zoomed.as_ref())
        else {
            return self.paint_empty_state(out, sidebar, level);
        };
        let mut chrome = ChromeCtx::new(
            &mut self.settings,
            &mut self.sidebar_painter,
            &self.mirror.session_name,
            self.viewport_dims,
            sidebar,
        );
        let focus = self.mirror.paint_focus();
        let focused = focus.as_ref();
        Some(match level {
            RepaintLevel::None => StatusBarPaint::NotPublished,
            RepaintLevel::Chrome => {
                paint_chrome_in_place(out, ls.as_ref(), &self.mirror.panes, focused, &mut chrome)
            }
            RepaintLevel::Full => paint_full_frame(
                out,
                ls.as_ref(),
                &mut self.mirror.panes,
                &self.mirror.engine_kernel,
                focused,
                &mut chrome,
            ),
        })
    }

    /// ADR-0105: a keep-empty session with no window paints its empty state.
    pub(super) fn paint_empty_state<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        level: RepaintLevel,
    ) -> Option<StatusBarPaint> {
        let showing = self.mirror.keep_empty_session && self.mirror.workspace.windows.is_empty();
        if !showing || matches!(level, RepaintLevel::None) {
            return None;
        }
        let new_window = phux_config::keybind::ResolvedAction {
            action: "new-window".to_owned(),
            args: std::collections::BTreeMap::new(),
        };
        let chord = crate::attach::action_registry::bound_chord_for(
            self.settings.keybindings.as_ref(),
            &new_window,
        );
        let lines = crate::attach::paint::empty_session_lines(chord.as_deref());
        let mut chrome = ChromeCtx::new(
            &mut self.settings,
            &mut self.sidebar_painter,
            &self.mirror.session_name,
            self.viewport_dims,
            sidebar,
        );
        Some(crate::attach::paint::paint_empty_session(
            out,
            &mut chrome,
            &lines,
        ))
    }

    /// Paint the active overlay layer over the current pane composition.
    pub(super) fn paint_overlay<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        let painted = self.paint_overlay_layer(out, sidebar);
        self.finish_paint(painted);
    }

    /// The paint half of [`Self::paint_overlay`]; committing the onboarding
    /// claim is the caller's.
    pub(super) fn paint_overlay_layer<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) -> StatusBarPaint {
        let base = self
            .mirror
            .workspace
            .render_window(self.mirror.zoomed.as_ref());
        let mut chrome = ChromeCtx::new(
            &mut self.settings,
            &mut self.sidebar_painter,
            &self.mirror.session_name,
            self.viewport_dims,
            sidebar,
        );
        let focus = self.mirror.paint_focus();
        paint_active_overlay(
            out,
            &self.overlays,
            base.as_deref(),
            &mut self.mirror.panes,
            &self.mirror.engine_kernel,
            focus.as_ref(),
            &mut chrome,
        )
    }

    /// Repaint the overlay stack when the live overlay tagged `key` took the
    /// fresh `items`; a no-op when it is not open.
    pub(super) fn refresh_live_overlay<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        key: &str,
        items: &[crate::render::overlay::SelectItem],
    ) {
        if self.overlays.refresh_items(key, items) {
            self.paint_overlay(out, sidebar);
        }
    }

    /// Open the directory picker on the listing the latest `go-to-directory`
    /// asked for; a reply to an older request is dropped.
    pub(super) fn open_directory_picker<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        reply: Option<(u32, phux_protocol::wire::frame::DirectoryListingResult)>,
    ) {
        let Some((request_id, result)) = reply else {
            return;
        };
        let Some(pending) = self
            .pending_directory
            .take_if(|pending| pending.request_id == request_id)
        else {
            tracing::debug!(request_id, "dropping stale DIRECTORY_LISTING");
            return;
        };
        let picker = crate::render::overlay::SelectList::new(
            crate::attach::directory_picker::picker_title(&result, &pending.host),
            crate::attach::directory_picker::picker_items(&result, &pending.host),
            &self.settings.theme,
        );
        // Only over its own placeholder: if the user cancelled it or opened
        // something else on top, the reply is dropped.
        if !self.overlays.replace_pending(request_id, Box::new(picker)) {
            tracing::debug!(request_id, "dropping DIRECTORY_LISTING: placeholder gone");
            return;
        }
        self.paint_overlay(out, sidebar);
    }

    /// Re-run the layered config loader and swap the config-derived state in
    /// place; failures keep the previous config and toast the error.
    pub(super) fn reload_config<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        let reloaded = adopt_config_reload(
            &mut self.settings,
            &mut self.overlays,
            &mut self.sidebar_painter,
            &phux_config::loader::config_path(),
        );
        if reloaded {
            // A reload rebuilds the sidebar painter cache-cold, so the
            // cross-session zones must be re-projected with it or the strip
            // comes back with an empty queue and roster until the next push.
            let rows = crate::attach::agent_rows::agent_session_rows(&self.mirror.engine_kernel);
            self.project_window_chrome(sidebar, &rows);
        }
        let has_window = self
            .mirror
            .workspace
            .render_window(self.mirror.zoomed.as_ref())
            .is_some();
        // A failed reload always leaves its toast on top.
        let painted = if self.overlays.is_active() {
            self.paint_overlay_layer(out, sidebar)
        } else if reloaded && has_window {
            self.paint_view(out, sidebar, RepaintLevel::Full)
                .unwrap_or(StatusBarPaint::NotPublished)
        } else {
            StatusBarPaint::NotPublished
        };
        self.finish_paint(painted);
    }

    /// ADR-0029 §2: the ONE drain. Every loop-level repaint trigger in this
    /// batch has raised; the highest level wins and paints exactly once.
    pub(super) fn drain_repaint<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        repaint: &mut RepaintAccumulator,
    ) {
        if std::mem::take(&mut self.peers.chrome_dirty) {
            self.note_chrome_change(repaint);
        }
        let drained = repaint.drain();
        if drained.fleet_dirty {
            self.refresh_fleet(out, sidebar);
        }
        // The same in-place refresh for a session picker that
        // was opened before its host inventory landed.
        if std::mem::take(&mut self.session_picker_dirty) {
            self.refresh_session_picker(out, sidebar);
        }
        // A full repaint discharges every paint the pacer was holding.
        if matches!(drained.level, RepaintLevel::Full) {
            self.pacer.clear_pending();
        }
        self.repaint_view(out, sidebar, drained.level);
    }

    /// Paint every pane the pacer withheld, as one composited frame.
    /// Suppression is re-evaluated here: a pane now under an overlay or a
    /// sync-output transaction is dropped (both have their own recovery).
    pub(super) fn settle_withheld_panes<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        let owed = self.pacer.take_pending();
        if owed.is_empty() {
            return;
        }
        // Arm the next window from the settle, not from the burst that filled
        // it, so a sustained producer paints on a steady cadence.
        self.pacer.rearm(tokio::time::Instant::now());
        if self.overlays.is_active() {
            return;
        }
        // ADR-0147: under the floating overlay only the overlay paints; the
        // rest repaint whole when it closes.
        let focus = self.mirror.paint_focus();
        let floating = crate::attach::floating::floating_pane(&self.mirror.panes).cloned();
        let live: Vec<ResourceId> = owed
            .into_iter()
            .filter(|id| {
                floating.as_ref().is_none_or(|floating| floating == id)
                    && self
                        .mirror
                        .panes
                        .get(id)
                        .is_some_and(|slot| slot.sync_output_since.is_none())
            })
            .collect();
        if live.is_empty() {
            return;
        }
        let painted = crate::attach::server_frame::paint_output_frame(
            crate::attach::server_frame::OutputFrame {
                out,
                kernel: &self.mirror.engine_kernel,
                panes: &mut self.mirror.panes,
                workspace: &self.mirror.workspace,
                zoomed: self.mirror.zoomed.as_ref(),
                focused_resource: focus.as_ref(),
                status_bar: self.settings.status_bar.as_mut(),
                sidebar,
                viewport_dims: self.viewport_dims,
                session_name: &self.mirror.session_name,
                predict: &mut self.mirror.predict,
            },
            &live,
        );
        self.finish_paint(painted);
        if self
            .mirror
            .focused_resource
            .as_ref()
            .is_some_and(|focused| live.contains(focused))
        {
            self.clear_focused_delivery_fence_after_paint();
        }
    }

    pub(super) fn clear_focused_delivery_fence_after_paint(&mut self) {
        let Some(terminal_id) = self.mirror.focused_resource.clone() else {
            return;
        };
        self.clear_delivery_fence_after_paint(&terminal_id);
    }

    pub(super) fn clear_visible_delivery_fences_after_paint(&mut self) {
        let Some(layout) = self
            .mirror
            .workspace
            .render_window(self.mirror.zoomed.as_ref())
        else {
            return;
        };
        let visible = layout
            .tree
            .as_ref()
            .map_or_else(Vec::new, crate::layout::leaves);
        for terminal_id in visible {
            self.clear_delivery_fence_after_paint(&terminal_id);
        }
    }

    pub(super) fn clear_delivery_fence_after_paint(&mut self, terminal_id: &ResourceId) {
        if !self.delivery_fence_paint_pending.remove(terminal_id) {
            return;
        }
        if let Some(journal) = self.input_replay.as_ref() {
            journal.borrow_mut().clear_delivery_fence(terminal_id);
        }
    }

    /// Rebuild and repaint the session picker, if it is open.
    pub(super) fn refresh_session_picker<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        if !self.overlays.is_active() {
            return;
        }
        let items = crate::attach::input_dispatch::session_picker_rows(
            &self.peers.inputs(&self.review),
            &self.mirror.workspace,
        );
        let key = crate::attach::input_dispatch::SESSION_PICKER_LIVE_KEY;
        self.refresh_live_overlay(out, sidebar, key, &items);
    }

    /// Rebuild and repaint the agent-fleet dashboard, if it is open.
    pub(super) fn refresh_fleet<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        if !self.overlays.is_active() {
            return;
        }
        let items = crate::attach::fleet::live_fleet_items(
            &self.mirror.workspace,
            &self.mirror.panes,
            &crate::attach::agent_rows::agent_session_rows(&self.mirror.engine_kernel),
            &self.mirror.agent_meta.records,
            &mut self.vcs,
            &self.peers.inputs(&self.review),
        );
        let key = crate::attach::fleet::FLEET_LIVE_KEY;
        self.refresh_live_overlay(out, sidebar, key, &items);
    }

    // ---- timers, signals, and the periodic paints -----------------------

    /// An application that never sent `?2026l`: expose the mirror once.
    pub(super) fn on_sync_output_timeout<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        let now = tokio::time::Instant::now();
        let mut expired = false;
        for slot in self.mirror.panes.values_mut() {
            if slot.sync_output_dirty
                && slot.sync_output_since.is_some_and(|since| {
                    now.saturating_duration_since(since) >= SYNC_OUTPUT_WATCHDOG
                })
            {
                slot.sync_output_since = None;
                slot.sync_output_dirty = false;
                expired = true;
            }
        }
        if expired {
            self.repaint_view(out, sidebar, RepaintLevel::Full);
        }
    }

    /// Push the which-key popup listing the pending prefix's continuations.
    pub(super) fn on_which_key_timeout<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        self.which_key_deadline = None;
        if push_which_key_overlay(
            &mut self.overlays,
            self.settings.resolver.as_ref(),
            self.settings.keybindings.as_ref(),
            &self.settings.theme,
        ) {
            self.paint_overlay(out, sidebar);
        }
    }

    /// Hand every surviving overlay the focused pane's current rect.
    pub(super) fn sync_overlays(&mut self, sidebar: Option<SidebarReservation>) {
        let bar = self.bar();
        sync_overlays_to_focused_pane(
            &mut self.overlays,
            &self.mirror.workspace,
            self.mirror.zoomed.as_ref(),
            self.mirror.focused_resource.as_ref(),
            self.viewport_dims,
            bar,
            sidebar,
        );
    }

    /// The periodic status-bar repaint.
    pub(super) fn on_status_tick<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        // Expire the transient notice (even under an overlay).
        if let Some(sb) = self.settings.status_bar.as_mut() {
            let _ = sb.clear_expired_notice(std::time::Instant::now());
        }
        self.expire_overdue_host_inventory();
        // Surface a background update check once it lands.
        self.poll_update_notice(out, sidebar);
        // An overlay would be partly overwritten by the bar paint.
        if self.overlays.is_active() {
            return;
        }
        // Restore the cursor to wherever the focused pane left it
        // so an idle tick doesn't strand the cursor in the bar.
        let focused_cursor = self
            .mirror
            .focused_resource
            .as_ref()
            .and_then(|fid| self.mirror.panes.get(fid))
            .and_then(|slot| slot.renderer.last_cursor());
        tracing::trace!(
            focused_pane_set = self.mirror.focused_resource.is_some(),
            has_cursor = focused_cursor.is_some(),
            "status_tick: repaint bar"
        );
        let fallback_origin = Some(self.bar_fallback_origin(sidebar));
        // A frame block opens only if the bar writes, so an unchanged tick
        // emits nothing.
        let mut chrome = ChromeCtx::new(
            &mut self.settings,
            &mut self.sidebar_painter,
            &self.mirror.session_name,
            self.viewport_dims,
            sidebar,
        );
        let painted = crate::attach::paint::close_frame_with_chrome(
            crate::attach::paint::FrameBlock::begin(out),
            &mut chrome,
            focused_cursor,
            fallback_origin,
            // The tick refreshes what the painter cannot observe (clock, exec).
            crate::render::chrome::status_bar::ComposePolicy::Always,
        );
        self.finish_paint(painted);
    }

    /// Where the cursor lands after a bar paint: the focused pane's origin,
    /// else (0, 0), so it is never stranded in the bar.
    pub(super) fn bar_fallback_origin(&self, sidebar: Option<SidebarReservation>) -> (u16, u16) {
        let content = self.content(sidebar);
        self.mirror
            .focused_resource
            .as_ref()
            .and_then(|fid| {
                self.mirror
                    .workspace
                    .render_window(self.mirror.zoomed.as_ref())
                    .and_then(|ls| {
                        crate::attach::paint::tiled_rect(
                            ls.as_ref(),
                            content,
                            self.viewport_dims,
                            fid,
                        )
                    })
            })
            .map_or((0, 0), |r| (r.x, r.y))
    }

    /// ADR-0140: a hosts-provider answer; redraw machines and the picker.
    pub(super) fn on_hosts<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        hosts: Vec<phux_core::host_list::HostJson>,
    ) {
        if self.peers.remote_hosts == hosts {
            return;
        }
        crate::attach::hosts::remember(&hosts);
        self.peers.remote_hosts = hosts;
        self.peers.chrome_dirty = true;
        self.session_picker_dirty = true;
        let mut repaint = RepaintAccumulator::default();
        self.drain_repaint(out, sidebar, &mut repaint);
    }

    /// A spawned plugin action finished: log it, and toast a failure.
    pub(super) fn on_plugin_result<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        result: &PluginRunResult,
    ) {
        tracing::info!(
            plugin = %result.plugin_id,
            action = %result.action_id,
            ok = plugin_actions::run_succeeded(result),
            "plugin action finished",
        );
        if let Some((title, lines)) = plugin_actions::failure_toast(result) {
            self.overlays.push(Box::new(ToastOverlay::new(
                title,
                lines,
                &self.settings.theme,
            )));
            self.paint_overlay(out, sidebar);
        }
    }

    /// Surface the background update check once it lands, polled on the bar
    /// tick for [`UPDATE_POLL_TICKS`] ticks; waits while a modal is up.
    pub(super) fn poll_update_notice<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        if self.update_poll_ticks == 0 || self.overlays.is_active() {
            return;
        }
        self.update_poll_ticks -= 1;
        let Some(notice) = crate::attach::update_notice::take_pending_notice() else {
            return;
        };
        self.update_poll_ticks = 0;
        self.overlays.push(Box::new(ToastOverlay::new(
            notice.title(),
            notice.body(),
            &self.settings.theme,
        )));
        self.paint_overlay(out, sidebar);
    }
}
