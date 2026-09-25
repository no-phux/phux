//! View records and scoped installation on the one terminal/dirty reader.

use super::{
    CanonicalGeometry, ClosedReplica, DocumentAnchorId, DocumentPoint, DocumentSpace,
    EngineDocumentSelection, EngineError, GridBuffer, Owner, PointerGesture, Predictor, ResourceId,
    Scroll, apply_terminal_selection, engine_error, viewport_scroll,
};
use crate::ViewId;
use crate::engine::views::ViewCommand;
use phux_client_core::engine::EngineDocumentAdapter;

pub(super) struct Presentation {
    pub(super) predictor: Predictor,
    pub(super) spare: Option<GridBuffer>,
    pub(super) viewport: Option<DocumentAnchorId>,
    pub(super) selection: Option<EngineDocumentSelection>,
    pub(super) gesture: Option<PointerGesture>,
    pub(super) search: Vec<u64>,
    pub(super) gesture_anchors: Vec<u64>,
    pub(super) unread_rows: u64,
    tail_distance: Option<u64>,
    // Closed replicas cannot allocate kernel document anchors. Their content
    // is immutable, so an offset is sufficient for retained-close scrolling.
    closed_offset: Option<u64>,
    /// The screen on which this view's tracked presentation was established.
    alternate: Option<bool>,
}

impl Presentation {
    fn new(geometry: CanonicalGeometry) -> Self {
        Self {
            predictor: Predictor::new(geometry.cols, geometry.rows),
            spare: None,
            viewport: None,
            selection: None,
            gesture: None,
            search: Vec::new(),
            gesture_anchors: Vec::new(),
            unread_rows: 0,
            tail_distance: None,
            closed_offset: None,
            alternate: None,
        }
    }
}

pub(super) struct View {
    pub(super) terminal: ResourceId,
    presentation: Presentation,
}

impl Owner {
    pub(super) fn note_history_damage(
        &mut self,
        effect: &super::KernelEffect,
        damaged: &mut Vec<ResourceId>,
    ) {
        let super::KernelEffect::Status(super::super::KernelStatus::HistoryUnavailable {
            key, ..
        }) = effect
        else {
            return;
        };
        let id = &key.terminal_id;
        if !self.has_replica(id) {
            return;
        }
        // Cursor tombstones can invalidate every document anchor without
        // emitting grid damage. Reconcile the default immediately; the
        // regular fanout then reconciles and republishes the other views.
        let reconciled = self
            .bump_document_revision(id)
            .and_then(|()| self.install_presentation(id));
        if let Err(error) = reconciled {
            tracing::warn!(terminal = %id, %error, "history presentation reconciliation failed");
        }
        if !damaged.contains(id) {
            damaged.push(id.clone());
        }
    }

    pub(super) fn view_command(&mut self, command: ViewCommand) {
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
    fn with_view<T>(
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

    pub(super) fn presentation_mut(
        &mut self,
        id: &ResourceId,
    ) -> Result<&mut Presentation, EngineError> {
        self.presentations
            .get_mut(id)
            .ok_or_else(|| engine_error("terminal has no presentation"))
    }

    pub(super) fn reset_generation_presentations(
        &mut self,
        id: &ResourceId,
        geometry: CanonicalGeometry,
    ) {
        self.anchors.retain(|_, (terminal, _, _)| terminal != id);
        self.presentations
            .insert(id.clone(), Presentation::new(geometry));
        for view in self.views.values_mut().filter(|view| &view.terminal == id) {
            view.presentation = Presentation::new(geometry);
        }
    }

    pub(super) fn render_views(&mut self, id: &ResourceId) -> Result<bool, EngineError> {
        let published = self.render_and_publish(id)?;
        let views: Vec<_> = self
            .views
            .iter()
            .filter(|(_, view)| &view.terminal == id)
            .map(|(id, _)| *id)
            .collect();
        let mut failure = None;
        for view in views {
            if let Err(error) = self.with_view(view, Self::render_and_publish) {
                failure = Some(error);
            }
        }
        failure.map_or(Ok(published), Err)
    }

    pub(super) fn forget_views(&mut self, id: &ResourceId) {
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

    fn release_presentation(&mut self, id: &ResourceId) {
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

    pub(super) fn install_presentation(&mut self, id: &ResourceId) -> Result<(), EngineError> {
        self.reconcile_screen(id)?;
        let (viewport, selection, closed_offset) = self
            .presentations
            .get(id)
            .map(|state| (state.viewport, state.selection, state.closed_offset))
            .ok_or_else(|| engine_error("missing presentation"))?;
        let point = viewport
            .map(|anchor| self.anchor_point(id, anchor))
            .transpose()?
            .flatten();
        let offset = closed_offset.or_else(|| point.map(|point| u64::from(point.y)));
        self.restore_viewport_offset(id, offset)?;
        if viewport.is_some() && point.is_none() {
            self.follow_history_tail(id)?;
        }
        self.install_selection(id, selection)
    }

    fn restore_viewport_offset(
        &mut self,
        id: &ResourceId,
        offset: Option<u64>,
    ) -> Result<(), EngineError> {
        let bar = self
            .terminal(id)?
            .scrollbar()
            .map_err(|error| engine_error(error.to_string()))?;
        let target = offset.unwrap_or_else(|| bar.total.saturating_sub(bar.len));
        // Restore the resolved offset without dirtying every row when the
        // installed viewport already agrees.
        if bar.offset != target {
            self.scroll_replica(id, Scroll::Row(target))?;
        }
        Ok(())
    }

    fn reconcile_screen(&mut self, id: &ResourceId) -> Result<(), EngineError> {
        let alternate = matches!(
            self.terminal(id)?
                .active_screen()
                .map_err(|error| engine_error(error.to_string()))?,
            libghostty_vt::screen::Screen::Alternate
        );
        let previous = self.presentation_mut(id)?.alternate;
        if previous.is_some_and(|previous| previous != alternate) {
            // A tracked main-screen reference may still resolve on main while
            // alt is active. Installing those coordinates on alt can fail or
            // select unrelated text. Retire only the scoped view's resources
            // before querying its old anchors, then publish the current screen.
            self.release_presentation(id);
            let (_, geometry, ..) = self
                .replica_identity(id)
                .ok_or_else(|| engine_error("screen changed without a replica"))?;
            let spare = self.presentation_mut(id)?.spare.take();
            let mut state = Presentation::new(geometry);
            state.spare = spare;
            self.presentations.insert(id.clone(), state);
            self.bump_document_revision(id)?;
        }
        self.presentation_mut(id)?.alternate = Some(alternate);
        Ok(())
    }

    fn install_selection(
        &mut self,
        id: &ResourceId,
        selection: Option<EngineDocumentSelection>,
    ) -> Result<(), EngineError> {
        let Some(selection) = selection else {
            return self
                .terminal(id)?
                .set_selection(None)
                .map(|_| ())
                .map_err(|error| engine_error(error.to_string()));
        };
        let start = self.anchor_point(id, selection.start)?;
        let end = self.anchor_point(id, selection.end)?;
        if start.is_none() || end.is_none() {
            self.presentation_mut(id)?.selection = None;
            self.reset_gesture(id);
        }
        apply_terminal_selection(self.terminal(id)?, start, end, selection.rectangle)
    }

    pub(super) fn anchor_point(
        &self,
        id: &ResourceId,
        anchor: DocumentAnchorId,
    ) -> Result<Option<DocumentPoint>, EngineError> {
        if let Some(closed) = self.closed.get(id) {
            return self
                .kernel
                .adapter()
                .document_anchor_point(closed.engine(), anchor, DocumentSpace::History)
                .map_err(|error| engine_error(error.to_string()));
        }
        self.kernel
            .document_anchor_point(id, anchor, DocumentSpace::History)
            .map_err(|error| engine_error(error.to_string()))
    }

    pub(super) fn release_tracked_anchor(
        &mut self,
        id: &ResourceId,
        anchor: DocumentAnchorId,
    ) -> Result<(), EngineError> {
        if let Some(closed) = self.closed.get_mut(id) {
            self.kernel
                .adapter_mut()
                .release_document_anchor(closed.engine_mut(), anchor);
            return Ok(());
        }
        self.kernel
            .release_document_anchor(id, anchor)
            .map_err(|error| engine_error(error.to_string()))
    }

    pub(super) fn scroll_replica(
        &mut self,
        id: &ResourceId,
        scroll: Scroll,
    ) -> Result<(), EngineError> {
        let viewport = viewport_scroll(scroll)?;
        let replica = if let Some(replica) = self.kernel.published_engine_mut(id) {
            replica
        } else {
            self.closed
                .get_mut(id)
                .map(ClosedReplica::engine_mut)
                .ok_or_else(|| engine_error("terminal has no scrollable replica"))?
        };
        replica
            .scroll_viewport(viewport)
            .map_err(|error| engine_error(error.to_string()))
    }

    pub(super) fn remember_closed_scroll(&mut self, id: &ResourceId) -> Result<(), EngineError> {
        let offset = self
            .terminal(id)?
            .scrollbar()
            .map_err(|error| engine_error(error.to_string()))?
            .offset;
        self.presentation_mut(id)?.closed_offset = Some(offset);
        Ok(())
    }

    pub(super) fn clear_search_handles(&mut self, id: &ResourceId) -> Result<(), EngineError> {
        let state = self.presentation_mut(id)?;
        let selection = state.selection;
        let handles = std::mem::take(&mut state.search);
        for handle in handles {
            let keep = self.anchors.get(&handle).is_some_and(|(_, _, anchor)| {
                selection
                    .is_some_and(|selection| selection.start == *anchor || selection.end == *anchor)
            });
            if !keep {
                self.drop_anchor_handle(id, handle);
            }
        }
        Ok(())
    }

    pub(super) fn drop_anchor_handle(&mut self, id: &ResourceId, handle: u64) {
        if let Some((_, _, anchor)) = self.anchors.remove(&handle) {
            let _ = self.release_tracked_anchor(id, anchor);
        }
    }

    pub(super) fn reset_unread(&mut self, id: &ResourceId) -> Result<(), EngineError> {
        let anchor = self.presentation_mut(id)?.viewport;
        let distance = anchor.and_then(|anchor| self.anchor_distance(id, anchor));
        let state = self.presentation_mut(id)?;
        state.tail_distance = distance;
        state.unread_rows = 0;
        Ok(())
    }

    fn anchor_distance(&self, id: &ResourceId, anchor: DocumentAnchorId) -> Option<u64> {
        use phux_client_core::engine::EngineAdapter;
        self.kernel
            .adapter()
            .history_anchor_tail_distance(self.replica(id)?, anchor)
            .ok()
            .flatten()
    }

    pub(super) fn note_view_output(&mut self, id: &ResourceId) {
        let distance = self
            .presentations
            .get(id)
            .and_then(|state| state.viewport)
            .and_then(|anchor| self.anchor_distance(id, anchor));
        if let Some(state) = self.presentations.get_mut(id) {
            state.note_output(distance);
        }
        let distances: Vec<_> = self
            .views
            .iter()
            .filter(|(_, view)| &view.terminal == id)
            .map(|(view_id, view)| {
                (
                    *view_id,
                    view.presentation
                        .viewport
                        .and_then(|anchor| self.anchor_distance(id, anchor)),
                )
            })
            .collect();
        for (view, distance) in distances {
            if let Some(view) = self.views.get_mut(&view) {
                view.presentation.note_output(distance);
            }
        }
    }
}

impl Presentation {
    const fn note_output(&mut self, distance: Option<u64>) {
        if let (Some(before), Some(after)) = (self.tail_distance, distance) {
            self.unread_rows = self
                .unread_rows
                .saturating_add(after.saturating_sub(before));
        }
        self.tail_distance = distance;
    }
}
