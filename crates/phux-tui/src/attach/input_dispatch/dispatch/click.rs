//! What a click into a pane does to focus.

use super::{EventEnv, ResourceId, reanchor_predict_to_pane};

impl<W: crate::attach::RenderSink> EventEnv<'_, '_, W> {
    /// Move client-local focus to the clicked pane.
    pub(super) fn focus_pane_from_click(&mut self, target: &ResourceId) {
        if let Some(ls) = self.ctx.workspace.active_group_mut() {
            ls.focus = Some(target.clone());
        }
        self.ctx
            .focus_history
            .transition(self.focused_resource, Some(target.clone()));
        // Re-anchor predict so the next keystroke echoes in the new pane.
        reanchor_predict_to_pane(self.predict, self.panes, target);
    }
}
