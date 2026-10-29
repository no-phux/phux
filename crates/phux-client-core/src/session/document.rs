//! Document-space history projection, search, and selection.

use super::{
    DocumentAnchorId, DocumentPoint, DocumentSpace, EngineDocumentAdapter, EngineDocumentSelection,
    EngineHistoryProjection, EngineProjectionOrigin, HistoryCacheError, KernelError,
    LoadedHistorySearch, ResourceId, SessionKernel, ViewportAnchor,
};

impl<E: EngineDocumentAdapter> SessionKernel<E> {
    /// Ask the engine to project loaded history at a frontend-only width.
    ///
    /// This never resizes the canonical terminal replica.
    pub fn project_history(
        &mut self,
        terminal_id: &ResourceId,
        width: u16,
        max_rows: usize,
    ) -> Result<EngineHistoryProjection, KernelError<E::Error>> {
        let replica = self
            .terminals
            .get_mut(terminal_id)
            .and_then(|state| state.published.as_mut())
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        replica.history.reproject(width);
        let width = replica.history.projection_width();
        let origin = match replica.history.viewport_anchor() {
            ViewportAnchor::Tail => EngineProjectionOrigin::Tail,
            ViewportAnchor::Pinned(anchor) => {
                let valid = self
                    .adapter
                    .document_anchor_point(&replica.engine, anchor, DocumentSpace::History)
                    .map_err(KernelError::Engine)?
                    .is_some();
                if valid {
                    EngineProjectionOrigin::Anchor(anchor)
                } else {
                    replica.history.mark_pruned();
                    self.adapter.clear_document_state(&mut replica.engine);
                    EngineProjectionOrigin::Tail
                }
            }
        };
        let mut projection = self
            .adapter
            .project_history(&mut replica.engine, width, origin, max_rows)
            .map_err(KernelError::Engine)?;
        projection.has_older |= replica.history.has_continuation();
        Ok(projection)
    }

    /// Create one engine-owned anchor in the published document.
    pub fn track_document_anchor(
        &mut self,
        terminal_id: &ResourceId,
        point: DocumentPoint,
    ) -> Result<DocumentAnchorId, KernelError<E::Error>> {
        let replica = self
            .terminals
            .get_mut(terminal_id)
            .and_then(|state| state.published.as_mut())
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        replica.history.ensure_anchor_capacity(1)?;
        let anchor = self
            .adapter
            .track_document_anchor(&mut replica.engine, point)
            .map_err(KernelError::Engine)?;
        replica
            .history
            .register_anchor_pages(anchor, std::iter::empty())?;
        Ok(anchor)
    }

    /// Resolve an engine-owned anchor after output, reflow, or history import.
    pub fn document_anchor_point(
        &self,
        terminal_id: &ResourceId,
        anchor: DocumentAnchorId,
        space: DocumentSpace,
    ) -> Result<Option<DocumentPoint>, KernelError<E::Error>> {
        let replica = self
            .terminals
            .get(terminal_id)
            .and_then(|state| state.published.as_ref())
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        self.adapter
            .document_anchor_point(&replica.engine, anchor, space)
            .map_err(KernelError::Engine)
    }

    /// Pin the client viewport to a valid engine-owned document anchor.
    pub fn pin_history_viewport(
        &mut self,
        terminal_id: &ResourceId,
        anchor: DocumentAnchorId,
    ) -> Result<(), KernelError<E::Error>> {
        let replica = self
            .terminals
            .get_mut(terminal_id)
            .and_then(|state| state.published.as_mut())
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        if self
            .adapter
            .document_anchor_point(&replica.engine, anchor, DocumentSpace::History)
            .map_err(KernelError::Engine)?
            .is_none()
        {
            return Err(KernelError::HistoryCache(
                HistoryCacheError::AnchorUnavailable,
            ));
        }
        replica.history.pin_viewport(anchor)?;
        Ok(())
    }

    /// Resume following the live tail without resizing canonical state.
    pub fn follow_history_tail(
        &mut self,
        terminal_id: &ResourceId,
    ) -> Result<(), KernelError<E::Error>> {
        let replica = self
            .terminals
            .get_mut(terminal_id)
            .and_then(|state| state.published.as_mut())
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        replica.history.follow_tail();
        Ok(())
    }

    /// Release one engine-owned anchor.
    pub fn release_document_anchor(
        &mut self,
        terminal_id: &ResourceId,
        anchor: DocumentAnchorId,
    ) -> Result<(), KernelError<E::Error>> {
        let replica = self
            .terminals
            .get_mut(terminal_id)
            .and_then(|state| state.published.as_mut())
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        self.adapter
            .release_document_anchor(&mut replica.engine, anchor);
        replica.history.remove_anchor(anchor);
        Ok(())
    }

    /// Search only state already loaded into the engine.
    ///
    /// Returns at most `max_matches`, further capped by the replica's
    /// remaining anchor budget. `truncated` says whether either bound hid a
    /// match: the adapter is asked for one more than the bound and that
    /// probe match is released before anything registers it.
    pub fn search_loaded_history(
        &mut self,
        terminal_id: &ResourceId,
        needle: &str,
        max_matches: usize,
    ) -> Result<LoadedHistorySearch, KernelError<E::Error>> {
        let replica = self
            .terminals
            .get_mut(terminal_id)
            .and_then(|state| state.published.as_mut())
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        let limit = max_matches.min(replica.history.remaining_anchor_capacity() / 2);
        let mut matches = self
            .adapter
            .search_loaded(&mut replica.engine, needle, limit.saturating_add(1))
            .map_err(KernelError::Engine)?;
        let truncated = matches.len() > limit;
        for probe in matches.split_off(limit.min(matches.len())) {
            self.adapter
                .release_document_anchor(&mut replica.engine, probe.start);
            self.adapter
                .release_document_anchor(&mut replica.engine, probe.end);
        }
        let registration = matches.iter().try_for_each(|found| {
            replica
                .history
                .register_anchor_pages(found.start, std::iter::empty())?;
            replica
                .history
                .register_anchor_pages(found.end, std::iter::empty())
        });
        if let Err(error) = registration {
            for found in &matches {
                self.adapter
                    .release_document_anchor(&mut replica.engine, found.start);
                self.adapter
                    .release_document_anchor(&mut replica.engine, found.end);
                replica.history.remove_anchor(found.start);
                replica.history.remove_anchor(found.end);
            }
            return Err(KernelError::HistoryCache(error));
        }
        Ok(LoadedHistorySearch { matches, truncated })
    }

    /// Format a selection through the engine's canonical text semantics.
    pub fn format_document_selection(
        &self,
        terminal_id: &ResourceId,
        selection: EngineDocumentSelection,
    ) -> Result<Option<String>, KernelError<E::Error>> {
        let replica = self
            .terminals
            .get(terminal_id)
            .and_then(|state| state.published.as_ref())
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        self.adapter
            .format_selection(&replica.engine, selection)
            .map_err(KernelError::Engine)
    }

    /// Copy with a hard output budget and the adapter's bounded-work policy.
    /// Never falls back to allocating the complete selection.
    pub fn format_document_selection_bounded(
        &self,
        terminal_id: &ResourceId,
        selection: EngineDocumentSelection,
        max_bytes: usize,
    ) -> Result<crate::engine::BoundedSelectionText, KernelError<E::Error>> {
        let replica = self
            .terminals
            .get(terminal_id)
            .and_then(|state| state.published.as_ref())
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        self.adapter
            .format_selection_bounded(&replica.engine, selection, max_bytes)
            .map_err(KernelError::Engine)
    }
}
