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

impl Client {
    /// Submit exact creation without an implicit session attach.
    pub fn create_session(
        &self,
        name: String,
        lifetime: phux_protocol::wire::frame::SessionLifetime,
        launch: Option<Box<phux_protocol::wire::frame::LaunchSpec>>,
    ) -> crate::control::WorkflowSubmission {
        self.inner.with(|control| control.create_session(name, lifetime, launch))
    }

    /// Submit a terminal creation, retaining its request or operation identity.
    pub fn submit_spawn(&self, request: crate::control::SpawnRequest) -> crate::control::WorkflowSubmission {
        self.inner.with(|control| control.submit_spawn(request))
    }

    /// Submit setup once; a lost receipt is not permission to recreate it.
    pub fn submit_spawn_once(&self, request: crate::control::SpawnRequest) -> crate::control::WorkflowSubmission {
        self.inner.with(|control| control.submit_spawn_once(request))
    }

    /// Resolve existing/created at the host using a retained exact operation.
    pub fn ensure_session(
        &self,
        name: String,
        lifetime: phux_protocol::wire::frame::SessionLifetime,
        launch: Option<Box<phux_protocol::wire::frame::LaunchSpec>>,
    ) -> crate::control::WorkflowSubmission {
        self.inner.with(|control| control.ensure_session(name, lifetime, launch))
    }
}
