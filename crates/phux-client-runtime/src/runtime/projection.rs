//! Published-grid and engine query methods.

#[cfg(feature = "engine")]
use super::{Arc, GridFrame, Publication, Scroll, TerminalPublication};
use super::{Client, ResourceId, lock};

impl Client {
    /// Create an independent tail-following presentation at canonical PTY size.
    #[cfg(feature = "engine")]
    pub fn create_view(
        &self,
        terminal: &ResourceId,
    ) -> Result<crate::ViewId, crate::engine::EngineError> {
        self.engine()
            .ok_or(crate::engine::EngineError::Stopped)?
            .create_view(terminal)
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

    /// Poll the independent view's frame generation.
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

    /// Resume following the live tail in this view only.
    #[cfg(feature = "engine")]
    pub fn follow_live_view(&self, view: crate::ViewId) -> Result<(), crate::engine::EngineError> {
        self.scroll_view(view, Scroll::Bottom)
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
        lock(&self.inner.control)
            .engine()
            .cloned()
            .ok_or(crate::engine::EngineError::Stopped)?
            .predict_text(terminal_id, text)
    }

    /// Clear predictive text and republish authoritative state.
    #[cfg(feature = "engine")]
    pub fn clear_predictions(
        &self,
        terminal_id: &ResourceId,
    ) -> Result<bool, crate::engine::EngineError> {
        lock(&self.inner.control)
            .engine()
            .cloned()
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
        lock(&self.inner.control)
            .engine()
            .cloned()
            .is_some_and(|engine| engine.is_alt_screen(terminal_id))
    }

    /// Whether the kernel holds a live (or retained) replica.
    #[must_use]
    pub fn has_projection(&self, terminal_id: &ResourceId) -> bool {
        lock(&self.inner.control)
            .engine()
            .cloned()
            .is_some_and(|engine| engine.has_projection(terminal_id))
    }

    /// Whether the kernel has permanently closed the terminal.
    #[must_use]
    pub fn is_closed(&self, terminal_id: &ResourceId) -> bool {
        lock(&self.inner.control)
            .engine()
            .cloned()
            .is_some_and(|engine| engine.is_closed(terminal_id))
    }

    /// Drain the bytes the headless replica retained since the last take.
    #[cfg(not(feature = "engine"))]
    #[must_use]
    pub fn take_output(&self, terminal_id: &ResourceId) -> Vec<u8> {
        lock(&self.inner.control)
            .engine()
            .cloned()
            .map(|engine| engine.take_output(terminal_id))
            .unwrap_or_default()
    }
}
