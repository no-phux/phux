//! Live overlays: the session picker and agent fleet, rebuilt in place, and
//! the overlay stack kept in step with the view.

use super::super::{SidebarReservation, sync_overlays_to_focused_pane};

impl super::super::SessionLoop {
    /// Rebuild and repaint the session picker, if it is open.
    pub(in crate::attach::driver::loop_state) fn refresh_session_picker<
        W: crate::attach::RenderSink,
    >(
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
    pub(in crate::attach::driver::loop_state) fn refresh_fleet<W: crate::attach::RenderSink>(
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

    /// Hand every surviving overlay the focused pane's current rect.
    pub(in crate::attach::driver::loop_state) fn sync_overlays(
        &mut self,
        sidebar: Option<SidebarReservation>,
    ) {
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
}
