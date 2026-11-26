//! Published-grid and engine query methods.

#[cfg(feature = "engine")]
use super::{Arc, GridFrame, Publication, Scroll, TerminalPublication, lock};
use super::{Client, ResourceId};

/// Exact immutable publication and delivery fence captured for a presentation.
///
/// Acquisition is not visibility: only a completed, visible and unclipped
/// platform draw may submit this evidence for acknowledgement.
#[cfg(feature = "engine")]
#[derive(Debug, Clone)]
pub struct PresentationEvidence {
    frame: Arc<GridFrame>,
    view: Option<crate::ViewId>,
    fence: crate::control::ProjectionFence,
}

#[cfg(feature = "engine")]
impl PartialEq for PresentationEvidence {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.frame, &other.frame)
            && self.view == other.view
            && self.fence == other.fence
    }
}

#[cfg(feature = "engine")]
impl Eq for PresentationEvidence {}

impl Client {
    /// Capture a fence for this exact current frame without acknowledging it.
    /// `view` is `None` for the default terminal presentation.
    #[cfg(feature = "engine")]
    #[must_use]
    pub fn presentation_evidence(
        &self,
        view: Option<crate::ViewId>,
        frame: &Arc<GridFrame>,
    ) -> Option<PresentationEvidence> {
        let control = lock(&self.inner.control);
        if control.status() != crate::control::Status::Attached || !frame.scrollbar.at_tail() {
            return None;
        }
        let current = self
            .inner
            .publication
            .current_frame(&frame.terminal_id, view)?;
        if !Arc::ptr_eq(&current, frame) {
            return None;
        }
        Some(PresentationEvidence {
            frame: Arc::clone(frame),
            view,
            fence: control.projection_fence(&frame.terminal_id)?,
        })
    }

    /// Acknowledge a completed visible, unclipped draw, never mere acquisition.
    /// Validate the exact publication (including stream/bootstrap), connection
    /// epoch and latest terminal-wide fence under the runtime owner.
    #[cfg(feature = "engine")]
    #[must_use]
    pub fn acknowledge_presentation(
        &self,
        evidence: &PresentationEvidence,
        visible: bool,
        unclipped: bool,
    ) -> bool {
        if !visible || !unclipped || !evidence.frame.scrollbar.at_tail() {
            return false;
        }
        self.inner.with(|control| {
            let current = self
                .inner
                .publication
                .current_frame(&evidence.frame.terminal_id, evidence.view);
            if control.status() != crate::control::Status::Attached
                || !current.is_some_and(|frame| Arc::ptr_eq(&frame, &evidence.frame))
            {
                return false;
            }
            control.acknowledge_projection_if(&evidence.frame.terminal_id, evidence.fence)
        })
    }

    /// Capture a source only in the caller's originally observed server and
    /// connection epoch. `None` represents an embedded/offline owner with no HELLO, not
    /// permission to ignore a negotiated server. Capture before deferring work
    /// and retain the returned source; never recapture after stale admission.
    #[cfg(feature = "engine")]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the control lock pins the checked epoch until the capture completes"
    )]
    pub fn view_source(
        &self,
        terminal: &ResourceId,
        expected_server: Option<&[u8]>,
        expected_connection_epoch: u64,
    ) -> Result<crate::engine::ViewSource, crate::engine::EngineError> {
        let control = lock(&self.inner.control);
        if control.connection_epoch() != expected_connection_epoch
            || control.server().map(|server| server.id.as_slice()) != expected_server
        {
            return Err(crate::engine::EngineError::StaleViewSource);
        }
        crate::engine::ViewSource::capture(&self.inner.publication, terminal)
    }

    /// Admit an independent presentation against its exact original source.
    /// The engine owner checks the source again when it processes admission.
    /// Retained offline sources need no live transport or new identity lookup.
    #[cfg(feature = "engine")]
    pub fn create_view(
        &self,
        source: &crate::engine::ViewSource,
    ) -> Result<crate::ViewId, crate::engine::EngineError> {
        self.engine()
            .ok_or(crate::engine::EngineError::Stopped)?
            .create_view(source)
    }

    /// Release only a view's presentation resources, preserving its terminal.
    #[cfg(feature = "engine")]
    pub fn destroy_view(&self, view: crate::ViewId) -> Result<(), crate::engine::EngineError> {
        self.engine()
            .ok_or(crate::engine::EngineError::Stopped)?
            .destroy_view(view)
    }

    /// Acquire the immutable current frame for an independent view.
    #[cfg(feature = "engine")]
    #[must_use]
    pub fn acquire_view(&self, view: crate::ViewId) -> Option<Arc<GridFrame>> {
        self.inner.publication.acquire_view(view)
    }

    /// Poll the view's frame generation: zero while waiting for its first
    /// frame, or `None` when the view is gone.
    #[cfg(feature = "engine")]
    #[must_use]
    pub fn view_generation(&self, view: crate::ViewId) -> Option<u64> {
        self.inner.publication.view_generation(view)
    }

    /// Retain a one-load polling handle for an independent view.
    #[cfg(feature = "engine")]
    #[must_use]
    pub fn view_slot(&self, view: crate::ViewId) -> Option<TerminalPublication> {
        self.inner.publication.view_slot(view)
    }

    /// Scroll only this view while preserving control-plane history routing.
    #[cfg(feature = "engine")]
    pub fn scroll_view(
        &self,
        view: crate::ViewId,
        scroll: Scroll,
    ) -> Result<(), crate::engine::EngineError> {
        self.inner.with(|control| control.scroll_view(view, scroll))
    }

    /// Pin this view to one of its own document anchors.
    #[cfg(feature = "engine")]
    pub fn pin_viewport_view(
        &self,
        view: crate::ViewId,
        anchor: u64,
    ) -> Result<(), crate::engine::EngineError> {
        self.inner
            .with(|control| control.pin_viewport_view(view, anchor))
    }

    // ----- the grid ----------------------------------------------------

    /// The published frames, for a consumer that polls generations itself.
    #[cfg(feature = "engine")]
    #[must_use]
    pub fn publication(&self) -> &Arc<Publication> {
        &self.inner.publication
    }

    /// The terminal's current frame, if one is published.
    #[cfg(feature = "engine")]
    #[must_use]
    pub fn acquire(&self, terminal_id: &ResourceId) -> Option<Arc<GridFrame>> {
        self.inner.publication.acquire(terminal_id)
    }

    /// The newest whole document a document resource published, whatever
    /// wake announced it. A wake consumer reads with
    /// [`Self::acquire_document_at`] instead.
    #[cfg(feature = "engine")]
    #[must_use]
    pub fn acquire_document(
        &self,
        resource_id: &ResourceId,
    ) -> Option<Arc<crate::publication::DocumentPublication>> {
        self.inner.publication.acquire_document(resource_id)
    }

    /// The document an [`crate::control::Event::DocumentPublished`] wake for
    /// `generation` announced; `None` once the slot is gone or moved on, in
    /// which case a newer wake follows.
    #[cfg(feature = "engine")]
    #[must_use]
    pub fn acquire_document_at(
        &self,
        resource_id: &ResourceId,
        generation: u64,
    ) -> Option<Arc<crate::publication::DocumentPublication>> {
        self.inner
            .publication
            .acquire_document_at(resource_id, generation)
    }

    /// The generation of the terminal's current frame.
    #[cfg(feature = "engine")]
    #[must_use]
    pub fn generation(&self, terminal_id: &ResourceId) -> Option<u64> {
        self.inner.publication.generation(terminal_id)
    }

    /// A handle on the terminal's slot for one-load generation polls.
    #[cfg(feature = "engine")]
    #[must_use]
    pub fn slot(&self, terminal_id: &ResourceId) -> Option<TerminalPublication> {
        self.inner.publication.slot(terminal_id)
    }

    /// Add predictive text and publish the resulting presentation.
    #[cfg(feature = "engine")]
    pub fn predict_text(
        &self,
        terminal_id: &ResourceId,
        text: String,
    ) -> Result<bool, crate::engine::EngineError> {
        let predicted = self
            .engine()
            .ok_or(crate::engine::EngineError::Stopped)?
            .predict_text(terminal_id, text);
        // The driver's deadline lane erases a guess the program never
        // echoes; wake it so an idle connection sees this one.
        self.inner.outbound.notify_one();
        predicted
    }

    /// Clear predictive text and republish authoritative state.
    #[cfg(feature = "engine")]
    pub fn clear_predictions(
        &self,
        terminal_id: &ResourceId,
    ) -> Result<bool, crate::engine::EngineError> {
        self.engine()
            .ok_or(crate::engine::EngineError::Stopped)?
            .clear_predictions(terminal_id)
    }

    /// Scroll the terminal's viewport; the new frame is published before
    /// this returns.
    #[cfg(feature = "engine")]
    pub fn scroll(
        &self,
        terminal_id: &ResourceId,
        scroll: Scroll,
    ) -> Result<(), crate::engine::EngineError> {
        self.inner
            .with(|control| control.scroll(terminal_id, scroll))
    }

    /// Search the terminal's loaded history, replacing its previous search
    /// handles. At most `max_matches` (capped at
    /// [`crate::engine::SEARCH_MATCH_LIMIT`]) come back, in document order.
    #[cfg(feature = "engine")]
    pub fn search(
        &self,
        terminal_id: &ResourceId,
        query: String,
        case_sensitive: bool,
        max_matches: usize,
    ) -> Result<crate::engine::SearchResults, crate::engine::EngineError> {
        self.engine()
            .ok_or(crate::engine::EngineError::Stopped)?
            .search_bounded(terminal_id, query, case_sensitive, max_matches)
    }

    /// Release the terminal's search handles; the viewport stays put.
    #[cfg(feature = "engine")]
    pub fn clear_search(&self, terminal_id: &ResourceId) -> Result<(), crate::engine::EngineError> {
        self.engine()
            .ok_or(crate::engine::EngineError::Stopped)?
            .clear_search(terminal_id)
    }

    /// Pin the terminal's viewport at one of its document anchors (a search
    /// match endpoint, say); the new frame is published before this returns.
    #[cfg(feature = "engine")]
    pub fn pin_viewport(
        &self,
        terminal_id: &ResourceId,
        anchor: u64,
    ) -> Result<(), crate::engine::EngineError> {
        self.inner
            .with(|control| control.pin_viewport(terminal_id, anchor))
    }

    /// Keep the final replica after the terminal closes.
    #[cfg(feature = "engine")]
    pub fn set_retain_on_close(&self, terminal_id: &ResourceId, retain: bool) {
        let engine = lock(&self.inner.control).engine().cloned();
        if let Some(engine) = engine {
            engine.set_retain_on_close(terminal_id, retain);
        }
    }

    /// Release a retained closed replica.
    #[cfg(feature = "engine")]
    pub fn release(&self, terminal_id: &ResourceId) {
        let engine = lock(&self.inner.control).engine().cloned();
        if let Some(engine) = engine {
            engine.release(terminal_id);
        }
    }

    /// Whether the terminal's alternate screen is active.
    #[cfg(feature = "engine")]
    #[must_use]
    pub fn is_alt_screen(&self, terminal_id: &ResourceId) -> bool {
        self.engine()
            .is_some_and(|engine| engine.is_alt_screen(terminal_id))
    }

    /// Whether the kernel holds a live (or retained) replica.
    #[must_use]
    pub fn has_projection(&self, terminal_id: &ResourceId) -> bool {
        self.engine()
            .is_some_and(|engine| engine.has_projection(terminal_id))
    }

    /// Whether the kernel has permanently closed the terminal.
    #[must_use]
    pub fn is_closed(&self, terminal_id: &ResourceId) -> bool {
        self.engine()
            .is_some_and(|engine| engine.is_closed(terminal_id))
    }

    /// Drain the bytes the headless replica retained since the last take.
    #[cfg(not(feature = "engine"))]
    #[must_use]
    pub fn take_output(&self, terminal_id: &ResourceId) -> Vec<u8> {
        self.engine()
            .map(|engine| engine.take_output(terminal_id))
            .unwrap_or_default()
    }
}
