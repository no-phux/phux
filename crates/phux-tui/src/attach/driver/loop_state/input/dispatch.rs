//! Dispatch context wiring and sidebar requests.

use super::super::{
    AttachError, Connection, DispatchCtx, Notice, SidebarReservation, StatusBarPainter,
    dispatch_input_events, undelivered_notices,
};

impl super::super::SessionLoop {
    /// Build the dispatch context and run the batch through the resolver,
    /// the overlay stack, and the pane input pipe.
    pub(in crate::attach::driver::loop_state) async fn dispatch_input<
        W: crate::attach::RenderSink,
    >(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        events: &mut Vec<phux_protocol::input::InputEvent>,
    ) -> Result<bool, AttachError> {
        let sidebar_targets = self.input_sidebar_targets(events);
        let arrangement_unloaded = self.arrangement_unloaded();
        let mut ctx = DispatchCtx {
            control_dial: self.control_dial.as_deref(),
            engine_kernel: &mut self.mirror.engine_kernel,
            resolver: self.settings.resolver.as_mut(),
            focus_history: self.focus_history.clone(),
            workspace: &mut self.mirror.workspace,
            layout_read_complete: self.layout_read_complete,
            unloaded_arrangement_read: arrangement_unloaded
                .then_some(&mut self.arrangement_read_owed),
            deferred_arrangement_edits: Some(&mut self.deferred_arrangement_edits),
            arrangement_write_owed: self
                .arrangement
                .is_some()
                .then_some(&mut self.arrangement_write_owed),
            viewport: self.viewport_dims,
            cell_px: self.cell_px_dims,
            next_request_id: &mut self.next_request_id,
            spawn_initial_size_supported: self.spawn_initial_size_supported,
            organization_supported: self.organization_supported,
            pending_splits: &mut self.mirror.pending_splits,
            pending_windows: &mut self.mirror.pending_windows,
            pending_floating: &mut self.mirror.pending_floating,
            directory_support: self.directory_support,
            pending_directory: &mut self.pending_directory,
            path_query_supported: self.path.supported,
            pending_path: &mut self.path.pending,
            own_client_id: self.own_client_id,
            expected_closes: &mut self.mirror.expected_closes,
            session_members: &self.mirror.members,
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
            jobs: &self.jobs,
            reload_request: &mut self.reload_request,
            host_switch_request: &mut self.host_switch_request,
            project_toggle_request: &mut self.project_toggle_request,
            agents_filter_request: &mut self.agents_filter_request,
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
        layout_changed |= self.apply_sidebar_requests();
        if std::mem::take(&mut self.arrangement_write_owed) {
            self.broadcast_layout(conn).await?;
        }
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

    /// Only mouse batches hit-test the sidebar; other input avoids cloning
    /// its targets.
    fn input_sidebar_targets(
        &self,
        events: &[phux_protocol::input::InputEvent],
    ) -> crate::render::chrome::sidebar::SidebarTargets {
        if events
            .iter()
            .any(|ev| matches!(ev, phux_protocol::input::InputEvent::Mouse(_)))
        {
            self.sidebar_painter.click_targets()
        } else {
            crate::render::chrome::sidebar::SidebarTargets::default()
        }
    }

    /// Apply the sidebar-only requests a batch dispatched (a project group
    /// toggle, an Agents filter change); true when the strip changed.
    fn apply_sidebar_requests(&mut self) -> bool {
        let toggled = self
            .project_toggle_request
            .take()
            .map(|key| self.toggle_project_group(key))
            .is_some();
        let filtered = self
            .agents_filter_request
            .take()
            .is_some_and(|request| self.apply_agents_filter(&request));
        toggled || filtered
    }
}
