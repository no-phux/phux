//! Presentation operations for the foreign client.

use super::*;

#[uniffi::export]
impl RemoteClient {
    pub fn has_projection(&self, terminal_id: String) -> bool {
        self.with_terminal(&terminal_id, Client::has_projection)
            .unwrap_or(false)
    }

    pub fn retain_projection_on_close(&self, terminal_id: String) {
        let _ = self.with_terminal(&terminal_id, |client, id| {
            client.set_retain_on_close(id, true);
        });
    }

    pub fn release_projection(&self, terminal_id: String) {
        let Some(id) = id::parse(&terminal_id) else {
            return;
        };
        self.generations.lock().unwrap().remove(&id);
        if let Some(client) = self.runtime_client() {
            client.release(&id);
        }
    }

    pub fn projection_changed(&self, terminal_id: String) -> bool {
        let Some(id) = id::parse(&terminal_id) else {
            return false;
        };
        let Some(client) = self.runtime_client() else {
            return false;
        };
        let Some(generation) = client.generation(&id) else {
            return false;
        };
        self.generations
            .lock()
            .unwrap()
            .get(&id)
            .copied()
            .unwrap_or(0)
            != generation
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "preserves the established UniFFI throws signature for generated clients"
    )]
    pub fn render_projection(
        &self,
        terminal_id: String,
    ) -> Result<Option<engine::GridProjection>, engine::EngineError> {
        let Some(id) = id::parse(&terminal_id) else {
            return Ok(None);
        };
        let Some(client) = self.runtime_client() else {
            return Ok(None);
        };
        let Some(frame) = client.acquire(&id) else {
            return Ok(None);
        };
        self.generations
            .lock()
            .unwrap()
            .insert(id, frame.generation);
        let receipt = client
            .presentation_evidence(None, &frame)
            .map(|evidence| std::sync::Arc::new(engine::PresentationReceipt { evidence }));
        let mut projection = engine::grid_projection(&grid::view(&frame), grid::dirty_rows(&frame));
        projection.presentation_receipt = receipt;
        Ok(Some(projection))
    }

    /// Report actual visible, unclipped display of the frame carrying receipt.
    /// Acquiring or building a projection is never presentation. A stale frame,
    /// connection, stream/bootstrap or newer delivery fence is rejected.
    pub fn acknowledge_presentation(
        &self,
        receipt: std::sync::Arc<engine::PresentationReceipt>,
        visible: bool,
        unclipped: bool,
    ) -> bool {
        self.runtime_client().is_some_and(|client| {
            client.acknowledge_presentation(&receipt.evidence, visible, unclipped)
        })
    }

    pub fn predict_projection_text(&self, terminal_id: String, text: String) {
        let _ = self.with_terminal(&terminal_id, |client, id| client.predict_text(id, text));
    }

    pub fn clear_projection_predictions(&self, terminal_id: String) {
        let _ = self.with_terminal(&terminal_id, Client::clear_predictions);
    }

    pub fn scroll_projection(&self, terminal_id: String, delta: i64) {
        let _ = self.with_terminal(&terminal_id, |client, id| {
            client.scroll(id, phux_client_runtime::engine::Scroll::Delta(delta))
        });
    }

    pub fn scroll_projection_to_bottom(&self, terminal_id: String) {
        let _ = self.with_terminal(&terminal_id, |client, id| {
            client.scroll(id, phux_client_runtime::engine::Scroll::Bottom)
        });
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "preserves the established UniFFI throws signature for generated clients"
    )]
    pub fn projection_scrollbar(
        &self,
        terminal_id: String,
    ) -> Result<Option<engine::ScrollbarState>, engine::EngineError> {
        let Some(id) = id::parse(&terminal_id) else {
            return Ok(None);
        };
        Ok(self
            .runtime_client()
            .and_then(|client| client.acquire(&id))
            .map(|frame| engine::scrollbar_state(grid::view(&frame).scrollbar)))
    }

    pub fn projection_is_alt_screen(&self, terminal_id: String) -> bool {
        self.with_terminal(&terminal_id, Client::is_alt_screen)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests;
