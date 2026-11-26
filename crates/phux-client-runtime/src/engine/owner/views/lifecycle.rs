//! Generation resets and retirement of a terminal's presentations. Admission
//! and scoped use of independent views live in `admission`.

use super::{CanonicalGeometry, EngineError, Owner, Presentation, ResourceId, engine_error};

impl Owner {
    pub(in crate::engine::owner) fn reset_generation_presentations(
        &mut self,
        id: &ResourceId,
        geometry: CanonicalGeometry,
    ) {
        if let Some(slot) = self.projectors.get_mut(id) {
            slot.valid = false;
        }
        self.anchors.retain(|_, (terminal, _, _)| terminal != id);
        self.presentations
            .insert(id.clone(), Presentation::new(geometry));
        for view in self.views.values_mut().filter(|view| &view.terminal == id) {
            view.presentation = Presentation::new(geometry);
        }
    }

    pub(in crate::engine::owner) fn forget_views(&mut self, id: &ResourceId) {
        let removed: Vec<_> = self
            .views
            .iter()
            .filter(|(_, view)| &view.terminal == id)
            .map(|(id, _)| *id)
            .collect();
        for view in removed {
            self.views.remove(&view);
            self.publication.remove_view(view);
        }
        self.anchors.retain(|_, (terminal, _, _)| terminal != id);
    }

    pub(in crate::engine::owner) fn presentation_mut(
        &mut self,
        id: &ResourceId,
    ) -> Result<&mut Presentation, EngineError> {
        self.presentations
            .get_mut(id)
            .ok_or_else(|| engine_error("terminal has no presentation"))
    }

    pub(super) fn release_presentation(&mut self, id: &ResourceId) {
        self.reset_gesture(id);
        let handles: Vec<_> = self
            .anchors
            .iter()
            .filter(|(_, (terminal, view, _))| terminal == id && *view == self.active_view)
            .map(|(handle, _)| *handle)
            .collect();
        for handle in handles {
            self.drop_anchor_handle(id, handle);
        }
        if let Some(anchor) = self
            .presentations
            .get_mut(id)
            .and_then(|state| state.viewport.take())
        {
            let _ = self.release_tracked_anchor(id, anchor);
        }
        self.invalidate_handles(id);
    }
}
