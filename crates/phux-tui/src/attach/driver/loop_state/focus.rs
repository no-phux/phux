//! Keyboard focus as the panes hear it (DEC 1004).

use phux_protocol::input::InputEvent;

use super::Connection;
use crate::attach::pane_state::{pane_exited, pane_satellite_down};
use crate::attach::surface_leaf::SurfaceLeaves;

impl super::SessionLoop {
    /// Tell the panes where keyboard focus went since the last call: a pane,
    /// tab or session switch, a floating pane opening or closing, a pane
    /// closing, or the outer window's own focus. `leaving` is the switch or
    /// detach that takes focus from every pane here. Best effort: a report
    /// the connection won't carry is logged, never fatal.
    pub(in super::super) async fn report_focus(&mut self, conn: &mut Connection, leaving: bool) {
        let keyboard = if leaving {
            None
        } else {
            crate::attach::floating::floating_pane(&self.mirror.panes)
                .or_else(|| {
                    self.mirror
                        .workspace
                        .active_group()
                        .and_then(|group| group.focus.as_ref())
                })
                .cloned()
        };
        let surfaces = SurfaceLeaves::new(&self.mirror.engine_kernel, &self.mirror.leaves);
        let owed = self.focus_reports.owed(keyboard.as_ref(), |id| {
            conn.can_route_terminal(id)
                && !pane_exited(&self.mirror.panes, id)
                && !pane_satellite_down(&self.mirror.panes, id)
                && !surfaces.contains(id)
        });
        for (pane, event) in owed {
            if let Err(err) = conn.send(&InputEvent::Focus(event).into_frame(pane)).await {
                tracing::debug!(?err, "focus report not sent");
            }
        }
    }
}
