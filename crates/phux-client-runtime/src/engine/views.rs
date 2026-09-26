//! Explicit-view requests reuse the default-view operation implementation.

use super::{
    Command, EngineDocumentPoint, EngineError, EngineHandle, EngineOutcome, Query, ReplicaInfo,
    ResourceId, Scroll, SearchMatch, SelectionGestureEvent, SelectionGestureResult, Sender,
};
use crate::ViewId;

pub(super) enum ViewCommand {
    Create(ResourceId, Sender<Result<ViewId, EngineError>>),
    Destroy(ViewId, Sender<Result<(), EngineError>>),
    Query(
        ViewId,
        Box<dyn FnOnce(ResourceId) -> Query + Send>,
        Sender<Result<(), EngineError>>,
    ),
}

impl EngineHandle {
    /// Create a tail-following view without attaching or resizing the terminal.
    pub fn create_view(&self, terminal: &ResourceId) -> Result<ViewId, EngineError> {
        self.request(|reply| Command::View(ViewCommand::Create(terminal.clone(), reply)))?
    }

    /// Destroy just this view. The terminal and other views remain attached.
    pub fn destroy_view(&self, view: ViewId) -> Result<(), EngineError> {
        self.request(|reply| Command::View(ViewCommand::Destroy(view, reply)))?
    }

    fn request_view<T: Send + 'static>(
        &self,
        view: ViewId,
        query: impl FnOnce(ResourceId, Sender<T>) -> Query + Send + 'static,
    ) -> Result<T, EngineError> {
        let (reply, response) = super::mpsc::channel();
        self.request(|done| {
            Command::View(ViewCommand::Query(
                view,
                Box::new(move |id| query(id, reply)),
                done,
            ))
        })??;
        response.recv().map_err(|_| EngineError::Stopped)
    }

    /// Scroll only this view, returning effects for control-plane routing.
    pub(crate) fn scroll_view(
        &self,
        view: ViewId,
        scroll: Scroll,
    ) -> Result<EngineOutcome, EngineError> {
        self.request_view(view, move |id, reply| Query::Scroll(id, scroll, reply))?
    }

    /// Pin only this view, returning effects for control-plane routing.
    pub(crate) fn pin_view(&self, view: ViewId, anchor: u64) -> Result<EngineOutcome, EngineError> {
        self.request_view(view, move |id, reply| Query::PinViewport(id, anchor, reply))?
    }

    /// Publish this view without modifying the other views' frame slots.
    pub fn republish_view(&self, view: ViewId) -> Result<bool, EngineError> {
        self.request_view(view, Query::Republish)?
    }

    /// Shared replica facts with this view's unread-history count.
    pub fn view_replica_info(&self, view: ViewId) -> Result<ReplicaInfo, EngineError> {
        self.request_view(view, Query::ReplicaInfo)?
    }

    /// Add predictive text to this view only.
    pub fn predict_view_text(&self, view: ViewId, text: String) -> Result<bool, EngineError> {
        self.request_view(view, move |id, reply| Query::PredictText(id, text, reply))?
    }

    /// Remove predictive text from this view only.
    pub fn clear_view_predictions(&self, view: ViewId) -> Result<bool, EngineError> {
        self.request_view(view, Query::ClearPredictions)?
    }

    /// Track a coordinate in this view. Handles cannot cross view boundaries.
    pub fn track_view_anchor(
        &self,
        view: ViewId,
        point: EngineDocumentPoint,
    ) -> Result<u64, EngineError> {
        self.request_view(view, move |id, reply| Query::TrackAnchor(id, point, reply))?
    }

    /// Release an anchor owned by this view.
    pub fn release_view_anchor(&self, view: ViewId, anchor: u64) -> Result<(), EngineError> {
        self.request_view(view, move |id, reply| {
            Query::ReleaseAnchor(id, anchor, reply)
        })?
    }

    /// Select between this view's tracked endpoints.
    pub fn set_view_selection(
        &self,
        view: ViewId,
        start: u64,
        end: u64,
        rectangle: bool,
    ) -> Result<(), EngineError> {
        self.request_view(view, move |id, reply| {
            Query::SetSelection(id, start, end, rectangle, reply)
        })?
    }

    /// Clear only this view's selection.
    pub fn clear_view_selection(&self, view: ViewId) -> Result<(), EngineError> {
        self.request_view(view, Query::ClearSelection)?
    }

    /// Copy this view's active selection.
    pub fn view_selection_text(&self, view: ViewId) -> Result<Vec<u8>, EngineError> {
        self.request_view(view, Query::SelectionText)?
    }

    /// Copy complete UTF-8 with at most `max_bytes + 1` output-buffer bytes.
    ///
    /// Refuses oversized output rather than truncating. The native adapter
    /// also rejects ranges exceeding 1,048,576 cells, conservatively counting
    /// whole selected rows plus one wide-glyph boundary row, before invoking
    /// Ghostty's formatter (which counts the entire range on buffer overflow).
    /// `WorkLimitExceeded` is distinct from proof of oversized text: trimming
    /// can make a large range small. Tracked-anchor lookup is still engine-owned;
    /// this is a formatting-work bound, not a wall-clock deadline.
    pub fn selection_text_view_bounded(
        &self,
        view: ViewId,
        max_bytes: usize,
    ) -> Result<super::BoundedSelectionText, EngineError> {
        self.request_view(view, move |id, reply| {
            Query::SelectionTextBounded(id, max_bytes, reply)
        })?
    }

    /// Search loaded history, replacing this view's previous search handles.
    /// Endpoints used by its active selection remain valid until released.
    pub fn search_view(
        &self,
        view: ViewId,
        query: String,
        case_sensitive: bool,
    ) -> Result<Vec<SearchMatch>, EngineError> {
        self.request_view(view, move |id, reply| {
            Query::Search(id, query, case_sensitive, reply)
        })?
    }

    /// Apply a gesture whose handle and coordinates belong to this view.
    pub fn view_selection_gesture(
        &self,
        view: ViewId,
        event: SelectionGestureEvent,
    ) -> Result<SelectionGestureResult, EngineError> {
        self.request_view(view, move |id, reply| Query::Gesture(id, event, reply))?
    }
}
