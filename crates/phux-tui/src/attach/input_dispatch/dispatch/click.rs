//! What a click into a pane does to focus and to held typing.

use super::{EventEnv, ResourceId, reanchor_predict_to_pane};
use crate::attach::actions::Adopt;

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

    /// The user clicked into a pane: they type there, whatever is opening.
    pub(super) fn release_held_typing(&mut self) {
        let windows = self.ctx.pending_windows.values_mut().map(|w| &mut w.adopt);
        let splits = self.ctx.pending_splits.values_mut().map(|s| &mut s.adopt);
        for adopt in windows.chain(splits) {
            if let Some(Adopt::Existing { over, .. }) = adopt {
                *over = None;
            }
        }
    }
}
