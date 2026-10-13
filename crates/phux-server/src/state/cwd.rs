use std::path::PathBuf;

use phux_core::ids::{ResourceId, SessionId};

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

    /// The most-recent working directory recorded for `session` under the
    /// `last-cwd-per-window` cwd-inheritance policy (phux-nyx), if any.
    #[must_use]
    pub fn session_last_cwd(&self, session: SessionId) -> Option<&PathBuf> {
        self.sessions.last_cwd(session)
    }

    /// Record `cwd` as `session`'s most-recent working directory,
    /// overwriting any prior value (phux-nyx, `last-cwd-per-window`).
    pub fn record_session_last_cwd(&mut self, session: SessionId, cwd: PathBuf) {
        self.sessions.record_last_cwd(session, cwd);
    }

    /// `session`'s oldest Terminal member.
    #[must_use]
    pub fn seed_pane_of_session(&self, session: SessionId) -> Option<ResourceId> {
        self.sessions.seed_pane_of(session)
    }
}
