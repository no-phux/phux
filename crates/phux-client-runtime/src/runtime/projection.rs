//! Published-grid and engine query methods.

#[cfg(feature = "engine")]
use super::{Arc, GridFrame, Publication, Scroll, TerminalPublication};
use super::{Client, ResourceId, lock};

impl Client {
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

    /// Scroll the terminal's viewport; the new frame is published before
    /// this returns.
    #[cfg(feature = "engine")]
    pub fn scroll(
        &self,
        terminal_id: &ResourceId,
        scroll: Scroll,
    ) -> Result<(), crate::engine::EngineError> {
        let engine = lock(&self.inner.control).engine().cloned();
        engine
            .ok_or(crate::engine::EngineError::Stopped)?
            .scroll(terminal_id, scroll)
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
