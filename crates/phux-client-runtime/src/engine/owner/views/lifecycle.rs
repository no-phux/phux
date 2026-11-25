//! Allocation, scoped use, and retirement of independent presentations.

use super::{
    CanonicalGeometry, EngineError, Owner, Presentation, ResourceId, View, ViewId, engine_error,
};
use crate::engine::views::ViewCommand;

impl Owner {
    pub(in crate::engine::owner) fn view_command(&mut self, command: ViewCommand) {
        match command {
            ViewCommand::Create(id, reply) => {
                let _ = reply.send(self.create_view(&id));
            }
            ViewCommand::Destroy(view, reply) => {
                let _ = reply.send(self.destroy_view(view));
            }
            ViewCommand::Query(view, query, reply) => {
                let result = self.with_view(view, |owner, id| {
                    owner.query(query(id.clone()));
                    Ok(())
                });
                let _ = reply.send(result);
            }
        }
    }

    fn create_view(&mut self, id: &ResourceId) -> Result<ViewId, EngineError> {
        let (token, geometry, ..) = self
            .replica_identity(id)
            .ok_or_else(|| engine_error("terminal has no renderable replica"))?;
        self.ensure_projector(id, token, geometry)?;
        let view = ViewId::allocate().ok_or_else(|| engine_error("view identities exhausted"))?;
        self.views.insert(
            view,
            View {
                terminal: id.clone(),
                presentation: Presentation::new(geometry),
            },
        );
        self.publication.register_view(view);
        if let Err(error) = self.render_views(id) {
            let _ = self.destroy_view(view);
            return Err(error);
        }
        Ok(view)
    }

    fn destroy_view(&mut self, view: ViewId) -> Result<(), EngineError> {
        let id = self
            .views
            .get(&view)
            .map(|view| view.terminal.clone())
            .ok_or_else(|| engine_error("view is stale or belongs to another client"))?;
        // Teardown must not depend on successfully installing the discarded
        // presentation (its anchors or terminal projection may have failed).
        self.swap_presentation(view, &id)?;
        self.active_view = Some(view);
        self.release_presentation(&id);
        self.active_view = None;
        let restored = self.swap_presentation(view, &id);
        self.views.remove(&view);
        self.publication.remove_view(view);
        restored?;
        self.install_presentation(&id)
    }

    /// The default state is restored even when installation or the operation
    /// fails. Only owner-thread code can enter this scope; scopes never nest.
    pub(in crate::engine::owner) fn with_view<T>(
        &mut self,
        view: ViewId,
        operation: impl FnOnce(&mut Self, &ResourceId) -> Result<T, EngineError>,
    ) -> Result<T, EngineError> {
        let id = self
            .views
            .get(&view)
            .map(|view| view.terminal.clone())
            .ok_or_else(|| engine_error("view is stale or belongs to another client"))?;
        let (token, geometry, ..) = self
            .replica_identity(&id)
            .ok_or_else(|| engine_error("view has no renderable replica"))?;
        self.ensure_projector(&id, token, geometry)?;
        self.swap_presentation(view, &id)?;
        self.active_view = Some(view);
        let result = self
            .install_presentation(&id)
            .and_then(|()| operation(self, &id));
        self.active_view = None;
        let restored = self
            .swap_presentation(view, &id)
            .and_then(|()| self.install_presentation(&id));
        match result {
            Err(error) => Err(error),
            Ok(value) => restored.map(|()| value),
        }
    }

    fn swap_presentation(&mut self, view: ViewId, id: &ResourceId) -> Result<(), EngineError> {
        let state = self
            .presentations
            .get_mut(id)
            .ok_or_else(|| engine_error("missing default view"))?;
        let view = self
            .views
            .get_mut(&view)
            .ok_or_else(|| engine_error("missing independent view"))?;
        std::mem::swap(state, &mut view.presentation);
        Ok(())
    }

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
