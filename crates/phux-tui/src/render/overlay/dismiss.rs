//! Questions: one at a time, and overlays that go away on their own.

use super::OverlayState;

impl OverlayState {
    /// Whether a stacked overlay is a question
    /// ([`super::RenderOverlay::asks`]), which another must not cover.
    #[must_use]
    pub fn asking(&self) -> bool {
        self.stack.iter().any(|overlay| overlay.asks())
    }

    /// Drop every overlay a resize invalidated
    /// ([`super::RenderOverlay::survives_resize`]) anywhere in the stack, like native
    /// menus close on resize. `true` when the stack changed.
    pub fn dismiss_stale_on_resize(&mut self) -> bool {
        let before = self.stack.len();
        self.stack.retain(|overlay| overlay.survives_resize());
        self.stack.len() != before
    }

    /// Drop every overlay whose question has expired
    /// ([`super::RenderOverlay::expired`]). `true` when the stack changed.
    pub fn dismiss_expired(&mut self) -> bool {
        let before = self.stack.len();
        self.stack.retain(|overlay| !overlay.expired());
        self.stack.len() != before
    }
}
