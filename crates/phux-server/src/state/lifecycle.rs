use super::ServerState;

impl ServerState {
    /// Record an accepted connection (disarms the idle clock).
    pub fn note_connection_opened(&mut self) {
        self.lifecycle.note_connection_opened();
    }

    /// Record a closed connection (re-arms the idle clock at zero).
    pub fn note_connection_closed(&mut self) {
        self.lifecycle.note_connection_closed();
    }

    /// Instant the server became unattended, or `None` while a client
    /// connection is open. See [`Self::note_connection_opened`].
    #[must_use]
    pub const fn idle_since(&self) -> Option<std::time::Instant> {
        self.lifecycle.idle_since()
    }

    /// Arm tmux-model last-session self-exit.
    pub(crate) const fn arm_self_exit(&mut self) {
        self.lifecycle.arm_self_exit();
    }

    /// Whether last-session self-exit has been armed.
    #[must_use]
    pub const fn has_served_client(&self) -> bool {
        self.lifecycle.has_served_client()
    }
}
