//! Clearing a pane's input-delivery fence once a paint has shown the user
//! its fresh authoritative output: ambiguous delivery fenced new input for
//! it until then.

use super::super::ResourceId;

impl super::super::SessionLoop {
    pub(in crate::attach::driver::loop_state) fn clear_focused_delivery_fence_after_paint(
        &mut self,
    ) {
        let Some(terminal_id) = self.mirror.focused_resource.clone() else {
            return;
        };
        self.clear_delivery_fence_after_paint(&terminal_id);
    }

    pub(in crate::attach::driver::loop_state) fn clear_visible_delivery_fences_after_paint(
        &mut self,
    ) {
        let Some(layout) = self
            .mirror
            .workspace
            .render_group(self.mirror.zoomed.as_ref())
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

    pub(in crate::attach::driver::loop_state) fn clear_delivery_fence_after_paint(
        &mut self,
        terminal_id: &ResourceId,
    ) {
        if !self.delivery_fence_paint_pending.remove(terminal_id) {
            return;
        }
        if let Some(journal) = self.input_replay.as_ref() {
            journal.borrow_mut().clear_delivery_fence(terminal_id);
        }
    }
}
