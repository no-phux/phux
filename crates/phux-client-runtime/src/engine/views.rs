//! Explicit-view requests reuse the default-view operation implementation.

use super::{
    Command, EngineDocumentPoint, EngineError, EngineHandle, EngineOutcome, Query, ReplicaInfo,
    ResourceId, Scroll, SearchMatch, SearchResults, SelectionGestureEvent, SelectionGestureResult,
    Sender,
};
use crate::{ViewId, publication::{Publication, TerminalPublication}};

/// Original renderable source of an independent view, not a reusable numeric id.
///
/// Capture once at the intent's qualified source. Retain this value across
/// deferred admission; a stale source is never permission to capture its
/// replacement. Slot identity binds the owner/incarnation, and the token binds
/// the exact logical stream and bootstrap generation.
#[derive(Clone, Debug)]
pub struct ViewSource {
    pub(super) terminal: ResourceId,
    pub(super) token: u128,
    pub(super) publication: TerminalPublication,
}

impl ViewSource {
    /// The terminal in the source's original id space.
    #[must_use]
    pub const fn terminal_id(&self) -> &ResourceId {
        &self.terminal
    }

    pub(crate) fn capture(publication: &Publication, terminal: &ResourceId) -> Result<Self, EngineError> {
        let slot = publication.slot(terminal).ok_or(EngineError::ProjectionUnavailable)?;
        let token = slot.source_generation().ok_or(EngineError::ProjectionUnavailable)?;
        Ok(Self { terminal: terminal.clone(), token, publication: slot })
    }
}

pub(super) enum ViewCommand {
    Source(ResourceId, Sender<Result<ViewSource, EngineError>>),
    Create(ViewSource, Sender<Result<ViewId, EngineError>>),
    Destroy(ViewId, Sender<Result<(), EngineError>>),
    Query(
        ViewId,
        Box<dyn FnOnce(ResourceId) -> Query + Send>,
        Sender<Result<(), EngineError>>,
    ),
}

impl EngineHandle {
    /// Capture the currently observed engine-only source for a fresh intent.
    /// Network consumers use `Client::view_source` with their original server
    /// identity instead. Do not call this again to revive a stale continuation.
    pub fn view_source(&self, terminal: &ResourceId) -> Result<ViewSource, EngineError> {
        self.request(|reply| Command::View(ViewCommand::Source(terminal.clone(), reply)))?
    }

    /// Create a tail-following view only if its original source still exists.
    pub fn create_view(&self, source: &ViewSource) -> Result<ViewId, EngineError> {
        self.request(|reply| Command::View(ViewCommand::Create(source.clone(), reply)))?
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
        response.recv().map_err(|_| {
            crate::perf::REQUEST_ERRORS.incr();
            EngineError::Stopped
        })
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

    /// Track a coordinate in this view. Handles cannot cross view boundaries.
    pub fn track_view_anchor(
        &self,
        view: ViewId,
        point: EngineDocumentPoint,
    ) -> Result<u64, EngineError> {
        self.request_view(view, move |id, reply| Query::TrackAnchor(id, point, reply))?
    }

    /// Where an anchor of this view is now, in history coordinates (space
    /// `0`, the row a [`Scroll::Row`] puts at the viewport's top); `None`
    /// once the history holding it was pruned.
    pub fn view_anchor_point(
        &self,
        view: ViewId,
        anchor: u64,
    ) -> Result<Option<EngineDocumentPoint>, EngineError> {
        self.request_view(view, move |id, reply| Query::AnchorPoint(id, anchor, reply))?
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
        self.search_view_bounded(view, query, case_sensitive)
            .map(|found| found.matches)
    }

    /// [`Self::search_view`] that also says whether a bound hid more matches.
    pub fn search_view_bounded(
        &self,
        view: ViewId,
        query: String,
        case_sensitive: bool,
    ) -> Result<SearchResults, EngineError> {
        self.request_view(view, move |id, reply| {
            Query::Search(id, query, case_sensitive, super::SEARCH_MATCH_LIMIT, reply)
        })?
    }

    /// Release this view's search handles without moving its viewport.
    /// Endpoints its active selection uses stay valid.
    pub fn clear_view_search(&self, view: ViewId) -> Result<(), EngineError> {
        self.request_view(view, Query::ClearSearch)?
    }

    /// Apply a gesture whose handle and coordinates belong to this view.
    pub fn view_selection_gesture(
        &self,
        view: ViewId,
        event: SelectionGestureEvent,
    ) -> Result<SelectionGestureResult, EngineError> {
        self.request_view(view, move |id, reply| Query::Gesture(id, event, reply))?
    }

    /// Select everything this view's screen holds (loaded scrollback and the
    /// active area), as Ghostty's `select_all`. Returns false, clearing the
    /// selection, when there is nothing to select.
    pub fn select_all_view(&self, view: ViewId) -> Result<bool, EngineError> {
        self.request_view(view, Query::SelectAll)?
    }
}
