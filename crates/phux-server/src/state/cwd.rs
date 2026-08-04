use std::path::PathBuf;

use phux_core::ids::{SessionId, TerminalId, WindowId};

use super::ServerState;

impl ServerState {
    /// Read the frozen session-creation directory recorded for `session`
    /// under the `session-root` cwd-inheritance policy (phux-nyx), if one
    /// has been captured.
    #[must_use]
    pub fn session_root(&self, session: SessionId) -> Option<&PathBuf> {
        self.sessions.root(session)
    }

    /// Freeze `root` as `session`'s creation directory the first time it is
    /// observed; later calls are no-ops so a `cd` in the seed pane cannot
    /// move an already-recorded root (phux-nyx, `session-root`). Returns the
    /// effective recorded root.
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

    /// Resolve the seed (oldest) pane of `session` — the first pane of its
    /// first window. The `session-root` policy reads this pane's CWD to
    /// establish the session's creation directory.
    #[must_use]
    pub fn seed_pane_of_session(&self, session: SessionId) -> Option<TerminalId> {
        self.sessions.seed_pane_of(session)
    }
}
