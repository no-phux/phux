//! Resource-specific geometry control, serialized by the control owner.

use phux_protocol::ids::ResourceId;

use super::Client;
use crate::control::TerminalResizeOutcome;

impl Client {
    /// Queue an exact resize for one ready, writable terminal, without changing
    /// the client's global viewport. The result is a local disposition, not an
    /// acknowledgement; read published frames for authoritative geometry.
    ///
    /// See [`crate::control::ControlPlane::resize_terminal`] for server policy
    /// precedence and size/readiness refusal semantics.
    #[must_use]
    pub fn resize_terminal(
        &self,
        terminal_id: &ResourceId,
        cols: u32,
        rows: u32,
    ) -> TerminalResizeOutcome {
        self.inner
            .with(|control| control.resize_terminal(terminal_id, cols, rows))
    }

    /// Subscribe without a resize, including automatic reconnect and recovery.
    /// Existing subscriptions keep their original policy; zero means no attach
    /// was queued. See [`crate::control::ControlPlane::attach_terminal_preserving_geometry`].
    #[must_use]
    pub fn attach_terminal_preserving_geometry(&self, terminal_id: &ResourceId) -> u32 {
        self.inner
            .with(|control| control.attach_terminal_preserving_geometry(terminal_id))
    }
}
