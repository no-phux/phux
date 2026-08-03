use std::path::PathBuf;

use phux_core::ids::{SessionId, TerminalId, WindowId};

use super::ServerState;

impl ServerState {
    /// Read the frozen session-creation directory recorded for `session`
    /// under the `session-root` cwd-inheritance policy (phux-nyx), if one
    /// has been captured.
    #[must_use]
    pub fn session_root(&self, session: SessionId) -> Option<&PathBuf> {
        self.session_root.get(&session)
    }

    /// Freeze `root` as `session`'s creation directory the first time it is
    /// observed; later calls are no-ops so a `cd` in the seed pane cannot
    /// move an already-recorded root (phux-nyx, `session-root`). Returns the
    /// effective recorded root.
    pub fn record_session_root(&mut self, session: SessionId, root: PathBuf) -> &PathBuf {
        self.session_root.entry(session).or_insert(root)
    }

    /// Read the most-recent working directory recorded for `window` under
    /// the `last-cwd-per-window` cwd-inheritance policy (phux-nyx), if any.
    #[must_use]
    pub fn window_last_cwd(&self, window: WindowId) -> Option<&PathBuf> {
        self.window_last_cwd.get(&window)
    }

    /// Record `cwd` as `window`'s most-recent working directory, overwriting
    /// any prior value (phux-nyx, `last-cwd-per-window`).
    pub fn record_window_last_cwd(&mut self, window: WindowId, cwd: PathBuf) {
        self.window_last_cwd.insert(window, cwd);
    }

    /// Resolve the window that owns `session`'s active pane, if any. The
    /// `last-cwd-per-window` policy keys its ledger on this window.
    #[must_use]
    pub fn active_window_of_session(&self, session: SessionId) -> Option<WindowId> {
        self.registry.session(session)?.active
    }

    /// Resolve the seed (oldest) pane of `session` — the first pane of its
    /// first window. The `session-root` policy reads this pane's CWD to
    /// establish the session's creation directory.
    #[must_use]
    pub fn seed_pane_of_session(&self, session: SessionId) -> Option<TerminalId> {
        let session = self.registry.session(session)?;
        let window_id = *session.windows.first()?;
        let window = self.registry.window(window_id)?;
        window.panes.first().copied()
    }
}
