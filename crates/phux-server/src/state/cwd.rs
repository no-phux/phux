use std::path::PathBuf;

use phux_core::ids::SessionId;

use super::ServerState;

impl ServerState {
    /// Record `session`'s root the first time only; returns the recorded
    /// root.
    pub fn record_session_root(&mut self, session: SessionId, root: PathBuf) -> &PathBuf {
        self.sessions.record_root(session, root)
    }

    /// Record `cwd` as `session`'s most-recent working directory,
    /// overwriting any prior value (phux-nyx, `last-cwd-per-window`).
    pub fn record_session_last_cwd(&mut self, session: SessionId, cwd: PathBuf) {
        self.sessions.record_last_cwd(session, cwd);
    }
}
