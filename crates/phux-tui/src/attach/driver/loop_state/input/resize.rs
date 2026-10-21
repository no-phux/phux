//! Outer-terminal resize handling.

use super::super::{
    AttachError, Connection, SidebarReservation, current_viewport_or_default, host_cell_px,
    resize_cell_px, viewport_resize_frame,
};

impl super::super::SessionLoop {
    /// Adopt the outer terminal's new size: tell the server, reflow every
    /// PTY, and rebuild the viewport from the authoritative snapshot.
    pub(in crate::attach::driver::loop_state) async fn on_resize<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) -> Result<(), AttachError> {
        let previous_dims = self.viewport_dims;
        let viewport = current_viewport_or_default();
        self.viewport_dims = (viewport.cols.max(1), viewport.rows.max(1));
        // Same cell grid: the server updates pixels without a resync, and the
        // mirror still matches the screen. Clearing here would leave it blank.
        let grid_unchanged = self.viewport_dims == previous_dims;
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
        // A real reflow clears rather than repainting a stale mirror; the
        // server's resync snapshot fills the new size. A held switch keeps
        // the old frame up instead. A same-grid resize is left on screen:
        // there is no snapshot coming to refill a clear.
        if !grid_unchanged && !self.paint_held() {
            let _ = out.write_all(b"\x1b[2J\x1b[H");
        }
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
