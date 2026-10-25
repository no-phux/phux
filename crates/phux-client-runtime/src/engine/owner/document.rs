//! Document anchors, selections, and search.

use super::{
    DocumentAnchorId, DocumentPoint, DocumentSpace, EngineDocumentPoint, EngineDocumentSelection,
    EngineError, EngineOutcome, Owner, ResourceId, Scroll, SearchMatch, SearchResults,
    anchor_unavailable, apply_terminal_selection, document_space, engine_error,
};

impl Owner {
    #[cfg(feature = "engine")]
    pub(super) fn track_anchor(
        &mut self,
        id: &ResourceId,
        point: EngineDocumentPoint,
    ) -> Result<u64, EngineError> {
        let space = document_space(point.space)?;
        let anchor = self
            .kernel
            .track_document_anchor(
                id,
                DocumentPoint {
                    space,
                    x: point.column,
                    y: point.row,
                },
            )
            .map_err(|error| engine_error(error.to_string()))?;
        self.register_anchor(id, anchor)
    }

    #[cfg(feature = "engine")]
    pub(super) fn register_anchor(
        &mut self,
        id: &ResourceId,
        anchor: DocumentAnchorId,
    ) -> Result<u64, EngineError> {
        let handle = crate::view::allocate_handle()
            .ok_or_else(|| engine_error("document anchor handle space exhausted"))?;
        self.anchors
            .insert(handle, (id.clone(), self.active_view, anchor));
        Ok(handle)
    }

    #[cfg(feature = "engine")]
    pub(super) fn resolve_anchor(
        &self,
        id: &ResourceId,
        handle: u64,
    ) -> Result<DocumentAnchorId, EngineError> {
        let anchor = self.owned_anchor(id, handle)?;
        if self.anchor_point(id, anchor)?.is_none() {
            return Err(anchor_unavailable(
                "document anchor was pruned or invalidated",
            ));
        }
        Ok(anchor)
    }

    #[cfg(feature = "engine")]
    fn owned_anchor(&self, id: &ResourceId, handle: u64) -> Result<DocumentAnchorId, EngineError> {
        let (owner, view, anchor) = self
            .anchors
            .get(&handle)
            .ok_or_else(|| anchor_unavailable("document anchor is stale or unknown"))?;
        if owner != id || *view != self.active_view {
            return Err(engine_error("document anchor belongs to another view"));
        }
        Ok(*anchor)
    }

    #[cfg(feature = "engine")]
    pub(super) fn release_anchor(
        &mut self,
        id: &ResourceId,
        handle: u64,
    ) -> Result<(), EngineError> {
        // Pruning invalidates the location, not the owner's obligation to
        // release its budget registration. Unknown/cross-view handles fail.
        let anchor = self.owned_anchor(id, handle)?;
        self.release_tracked_anchor(id, anchor)?;
        self.anchors.remove(&handle);
        Ok(())
    }

    #[cfg(feature = "engine")]
    pub(super) fn clear_presentation(
        &mut self,
        id: &ResourceId,
        stream_id: u64,
        bootstrap_id: u64,
    ) -> Result<(), EngineError> {
        let info = self.replica_info(id)?;
        if (info.stream_id, info.bootstrap_id) != (stream_id, bootstrap_id) {
            return Err(engine_error("clear targets a stale terminal generation"));
        }
        self.kernel
            .clear_presentation(id)
            .map_err(|error| engine_error(error.to_string()))?;
        self.invalidate_handles(id);
        if let Some((_, geometry, ..)) = self.replica_identity(id) {
            self.reset_generation_presentations(id, geometry);
        }
        self.bump_document_revision(id)?;
        self.render_views(id).map(|_| ())
    }

    #[cfg(feature = "engine")]
    pub(super) fn pin_viewport(
        &mut self,
        id: &ResourceId,
        handle: u64,
    ) -> Result<EngineOutcome, EngineError> {
        self.require_projection(id)?;
        let anchor = self.resolve_anchor(id, handle)?;
        let point = self
            .kernel
            .document_anchor_point(id, anchor, DocumentSpace::History)
            .map_err(|error| engine_error(error.to_string()))?
            .ok_or_else(|| anchor_unavailable("viewport anchor is no longer available"))?;
        self.scroll(id, Scroll::Row(u64::from(point.y)))
    }

    #[cfg(feature = "engine")]
    pub(super) fn follow_live(&mut self, id: &ResourceId) -> Result<EngineOutcome, EngineError> {
        self.scroll(id, Scroll::Bottom)
    }

    #[cfg(feature = "engine")]
    pub(super) fn set_selection(
        &mut self,
        id: &ResourceId,
        start_handle: u64,
        end_handle: u64,
        rectangle: bool,
    ) -> Result<(), EngineError> {
        let start = self.resolve_anchor(id, start_handle)?;
        let end = self.resolve_anchor(id, end_handle)?;
        let start_point = self
            .kernel
            .document_anchor_point(id, start, DocumentSpace::History)
            .map_err(|error| engine_error(error.to_string()))?;
        let end_point = self
            .kernel
            .document_anchor_point(id, end, DocumentSpace::History)
            .map_err(|error| engine_error(error.to_string()))?;
        apply_terminal_selection(self.terminal(id)?, start_point, end_point, rectangle)?;
        self.presentation_mut(id)?.selection = Some(EngineDocumentSelection {
            start,
            end,
            rectangle,
        });
        self.render_and_publish(id).map(|_| ())
    }

    #[cfg(feature = "engine")]
    pub(super) fn clear_selection(&mut self, id: &ResourceId) -> Result<(), EngineError> {
        self.reset_gesture(id);
        self.terminal(id)?
            .set_selection(None)
            .map_err(|error| engine_error(error.to_string()))?;
        self.presentation_mut(id)?.selection = None;
        self.render_and_publish(id).map(|_| ())
    }

    #[cfg(feature = "engine")]
    pub(super) fn selection_text(&self, id: &ResourceId) -> Result<Vec<u8>, EngineError> {
        let selection = self
            .presentations
            .get(id)
            .and_then(|state| state.selection)
            .ok_or_else(|| engine_error("terminal has no active selection"))?;
        self.kernel
            .format_document_selection(id, selection)
            .map(|text| text.unwrap_or_default().into_bytes())
            .map_err(|error| engine_error(error.to_string()))
    }

    #[cfg(feature = "engine")]
    pub(super) fn selection_text_bounded(
        &self,
        id: &ResourceId,
        max_bytes: usize,
    ) -> Result<super::BoundedSelectionText, EngineError> {
        let Some(selection) = self.presentations.get(id).and_then(|state| state.selection) else {
            return Ok(super::BoundedSelectionText::Unavailable);
        };
        self.kernel
            .format_document_selection_bounded(id, selection, max_bytes)
            .map_err(|error| engine_error(error.to_string()))
    }

    #[cfg(feature = "engine")]
    pub(super) fn search(
        &mut self,
        id: &ResourceId,
        query: &str,
        case_sensitive: bool,
        max_matches: usize,
    ) -> Result<SearchResults, EngineError> {
        if query.is_empty() {
            return Err(engine_error("search query is empty"));
        }
        self.clear_search_handles(id)?;
        let max_matches = max_matches.min(self.search_match_capacity(id));
        self.kernel
            .adapter_mut()
            .set_search_case_sensitive(case_sensitive);
        let result = self.kernel.search_loaded_history(id, query, max_matches);
        self.kernel.adapter_mut().set_search_case_sensitive(true);
        let result = result.map_err(|error| engine_error(error.to_string()))?;
        let mut found = Vec::with_capacity(result.matches.len());
        for matched in result.matches {
            let start = self.register_anchor(id, matched.start)?;
            let end = match self.register_anchor(id, matched.end) {
                Ok(end) => end,
                Err(error) => {
                    let _ = self.release_anchor(id, start);
                    return Err(error);
                }
            };
            found.push(SearchMatch { start, end });
            self.presentation_mut(id)?.search.extend([start, end]);
        }
        Ok(SearchResults {
            matches: found,
            truncated: result.truncated,
        })
    }

    #[cfg(feature = "engine")]
    pub(super) fn scroll(
        &mut self,
        id: &ResourceId,
        scroll: Scroll,
    ) -> Result<EngineOutcome, EngineError> {
        let active = self.kernel.published_engine_mut(id).is_some();
        if active {
            self.scroll_active(id, scroll)?;
        } else {
            self.scroll_replica(id, scroll)?;
            self.remember_closed_scroll(id)?;
        }
        self.render_and_publish(id)?;
        Ok(EngineOutcome {
            effects: self.effects.take(),
            error: None,
        })
    }

    #[cfg(feature = "engine")]
    fn scroll_active(&mut self, id: &ResourceId, scroll: Scroll) -> Result<(), EngineError> {
        let previous = self
            .terminal(id)?
            .scrollbar()
            .map_err(|error| engine_error(error.to_string()))?
            .offset;
        self.scroll_replica(id, scroll)?;
        if let Err(error) = self.update_history_viewport(id) {
            // Explicit anchors or newly created views may have consumed the
            // reserved search headroom. Failed pin allocation must not leave
            // the engine viewport disagreeing with its presentation/frame.
            self.scroll_replica(id, Scroll::Row(previous))?;
            return Err(error);
        }
        Ok(())
    }

    #[cfg(feature = "engine")]
    pub(super) fn update_history_viewport(&mut self, id: &ResourceId) -> Result<(), EngineError> {
        let scrollbar = self
            .terminal(id)?
            .scrollbar()
            .map_err(|error| engine_error(format!("scrollbar: {error}")))?;
        let at_tail = scrollbar.offset.saturating_add(scrollbar.len) >= scrollbar.total;
        if at_tail {
            self.follow_history_tail(id)?;
        } else {
            self.pin_history_viewport(id)?;
        }
        let rows_from_oldest = usize::try_from(scrollbar.offset)
            .map_err(|_| engine_error("history viewport offset exceeds usize"))?;
        let _ = self
            .kernel
            .prefetch_history(id, rows_from_oldest, &mut self.effects);
        Ok(())
    }

    #[cfg(feature = "engine")]
    pub(super) fn follow_history_tail(&mut self, id: &ResourceId) -> Result<(), EngineError> {
        if let Some(old) = self.presentation_mut(id)?.viewport.take() {
            self.release_tracked_anchor(id, old)?;
        }
        self.reset_unread(id)
    }

    #[cfg(feature = "engine")]
    pub(super) fn pin_history_viewport(&mut self, id: &ResourceId) -> Result<(), EngineError> {
        // The kernel cache still owns page budgets, cursor continuity, anchor
        // accounting, and prefetch. Its singleton viewport stays at Tail:
        // pinning it here would make pruning one view clear every view's
        // document anchors in SessionKernel::reconcile_pinned_anchor.
        // Presentation pinning belongs exclusively to this view record.
        let anchor = self
            .kernel
            .track_document_anchor(
                id,
                DocumentPoint {
                    space: DocumentSpace::Viewport,
                    x: 0,
                    y: 0,
                },
            )
            .map_err(|error| engine_error(error.to_string()))?;
        if let Some(old) = self.presentation_mut(id)?.viewport.replace(anchor) {
            self.release_tracked_anchor(id, old)?;
        }
        self.reset_unread(id)
    }
}
