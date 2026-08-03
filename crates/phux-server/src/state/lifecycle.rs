use super::ServerState;

impl ServerState {
    /// Record that a client connection was accepted (any transport).
    ///
    /// Disarms the idle clock: while a connection is open the server is by
    /// definition not idle, however long that connection sits quiet. "Idle"
    /// here means *unattended*, not *silent* — a human parked in an attached
    /// pane reading a log for an hour must never be reaped.
    pub fn note_connection_opened(&mut self) {
        self.live_connections = self.live_connections.saturating_add(1);
        self.idle_since = None;
    }

    /// Record that a client connection ended (clean EOF, error, or drop).
    ///
    /// Re-arms the idle clock when this was the last one. `saturating_sub`
    /// rather than `-= 1`: a decrement without a matching increment is a
    /// bookkeeping bug, and underflowing to `u32::MAX` would silently make
    /// the server immortal — the exact failure this whole feature exists to
    /// prevent. Saturating instead pins the count at zero, which fails
    /// *toward* exiting and is therefore the safe direction.
    pub fn note_connection_closed(&mut self) {
        self.live_connections = self.live_connections.saturating_sub(1);
        if self.live_connections == 0 {
            self.idle_since = Some(std::time::Instant::now());
        }
    }

    /// Instant the server became unattended, or `None` while a client
    /// connection is open. See [`Self::note_connection_opened`].
    #[must_use]
    pub const fn idle_since(&self) -> Option<std::time::Instant> {
        self.idle_since
    }

    /// Arm tmux-model last-session self-exit.
    pub(crate) const fn arm_self_exit(&mut self) {
        self.has_served_client = true;
    }

    /// Whether last-session self-exit has been armed.
    #[must_use]
    pub const fn has_served_client(&self) -> bool {
        self.has_served_client
    }
}
