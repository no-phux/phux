//! Independent-view admission and teardown on the serialized engine owner.

use super::super::{EngineError, Owner, Presentation, ResourceId, View, engine_error};
use crate::ViewId;
use crate::engine::views::{ViewCommand, ViewSource};

impl Owner {
    pub(in crate::engine::owner) fn view_command(&mut self, command: ViewCommand) {
        match command {
            ViewCommand::Source(id, reply) => {
                let _ = reply.send(ViewSource::capture(&self.publication, &id));
            }
            ViewCommand::Create(source, reply) => {
                let _ = reply.send(self.create_view(&source));
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

    fn create_view(&mut self, source: &ViewSource) -> Result<ViewId, EngineError> {
        let id = source.terminal_id();
        let (token, geometry, ..) = self
            .replica_identity(id)
            .ok_or(EngineError::StaleViewSource)?;
        if token != source.token
            || !self
                .publication
                .slot(id)
                .is_some_and(|slot| slot.same_slot(&source.publication))
        {
            return Err(EngineError::StaleViewSource);
        }
        // No other owner command can replace the validated source before the
        // view is installed. In particular RESET + reused ids cannot race it.
        self.ensure_projector(id, token, geometry)?;
        let view = ViewId::allocate().ok_or_else(|| engine_error("view identities exhausted"))?;
        self.views.insert(
            view,
            View {
                terminal: id.clone(),
                presentation: Presentation::new(geometry),
            },
        );
        // A live view can wait for synchronized output without a frame yet.
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
        // Teardown releases only this presentation, even if installation fails.
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
}
