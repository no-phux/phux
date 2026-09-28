use std::path::PathBuf;

use phux_core::ids::{ResourceId, SessionId, WindowId};

use super::ServerState;

impl ServerState {
    /// `session`'s frozen creation directory (`session-root`), if captured.
    #[must_use]
    pub fn session_root(&self, session: SessionId) -> Option<&PathBuf> {
        self.sessions.root(session)
    }

    /// Record `session`'s root the first time only; returns the recorded
    /// root.
    pub fn record_session_root(&mut self, session: SessionId, root: PathBuf) -> &PathBuf {
        self.sessions.record_root(session, root)
    }

    /// Read the most-recent working directory recorded for `window` under
    /// the `last-cwd-per-window` cwd-inheritance policy (phux-nyx), if any.
    #[must_use]
    pub fn window_last_cwd(&self, window: WindowId) -> Option<&PathBuf> {
        self.sessions.last_cwd(window)
    }

    /// Record `cwd` as `window`'s most-recent working directory, overwriting
    /// any prior value (phux-nyx, `last-cwd-per-window`).
    pub fn record_window_last_cwd(&mut self, window: WindowId, cwd: PathBuf) {
        self.sessions.record_last_cwd(window, cwd);
    }

    /// Resolve the window that owns `session`'s active pane, if any. The
    /// `last-cwd-per-window` policy keys its ledger on this window.
    #[must_use]
    pub fn active_window_of_session(&self, session: SessionId) -> Option<WindowId> {
        self.sessions.active_window_of(session)
    }

    /// `session`'s seed (oldest) pane.
    #[must_use]
    pub fn seed_pane_of_session(&self, session: SessionId) -> Option<ResourceId> {
        self.sessions.seed_pane_of(session)
    }
}
