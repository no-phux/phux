use phux_core::ids::{SessionId, TerminalId};
use phux_core::registry::Registry;
use phux_core::session::Session;
use phux_protocol::ids::TerminalId as WireTerminalId;

use super::{RenameOutcome, ServerState};

impl ServerState {
    /// Borrow the canonical session/window/pane registry.
    ///
    /// The read half of what used to be a bare `pub registry` field; the
    /// map itself now lives in `state::session_table` next to the ledgers
    /// keyed on it.
    #[must_use]
    pub const fn registry(&self) -> &Registry {
        &self.sessions.registry
    }

    /// Mutably borrow the canonical session/window/pane registry.
    ///
    /// The write half of what used to be a bare `pub registry` field.
    /// Note that this borrows all of `ServerState`, so a `&mut` registry
    /// borrow and a `&self` accessor call cannot overlap the way two
    /// disjoint fields could — keep them in separate statements.
    pub const fn registry_mut(&mut self) -> &mut Registry {
        &mut self.sessions.registry
    }

    /// Most-recently-touched live session, if any. Resolves
    /// `AttachTarget::Last`.
    #[must_use]
    pub fn most_recently_touched_session(&self) -> Option<SessionId> {
        self.sessions.most_recently_touched()
    }

    /// Mark `session` as touched by attach/input/focus activity.
    pub fn touch_session(&mut self, session: SessionId) {
        self.sessions.touch(session);
    }

    /// Look up the active pane of the active window of `session`, if any.
    #[must_use]
    pub fn active_pane_of_session(&self, session: SessionId) -> Option<TerminalId> {
        self.sessions.active_pane_of(session)
    }

    /// Borrow the session named `name`, if it exists.
    #[must_use]
    pub fn session_by_name(&self, name: &str) -> Option<&Session> {
        self.sessions.by_name(name)
    }

    /// Look up the [`SessionId`] for a name by scanning the registry.
    ///
    /// Uses `Registry::sessions` directly — no side ledger required.
    pub(crate) fn find_session_by_name(&self, name: &str) -> Option<SessionId> {
        self.sessions.find_by_name(name)
    }

    /// Rename the session named `current` to `new_name`, in place.
    ///
    /// Mirrors `CREATE_SESSION`'s uniqueness rule: names are unique within
    /// the registry, so a `new_name` already in use is rejected. Resolution
    /// uses the same registry scan as every other name lookup (no side
    /// ledger, per [`Self::session_by_name`]), so there is nothing else to keep
    /// in sync. The server is authoritative once this returns
    /// [`RenameOutcome::Renamed`]; the next `ATTACHED` snapshot each client
    /// builds carries the new name.
    ///
    /// Returns a [`RenameOutcome`] distinguishing the two refusal cases the
    /// wire surfaces (`SESSION_NOT_FOUND` vs `INVALID_COMMAND`) from success.
    pub fn rename_session(&mut self, current: &str, new_name: &str) -> RenameOutcome {
        self.sessions.rename(current, new_name)
    }

    /// Seed a session+window+pane. Returns the new
    /// `(SessionId, WindowId, TerminalId)`.
    ///
    /// This is the entry point `ServerConfig::pre_seeded_session` uses to
    /// pre-populate the registry before clients connect.
    ///
    /// # Panics
    ///
    /// Panics if the registry rejects the freshly-allocated session or
    /// window ids — both branches are unreachable because the parent
    /// entity was created on the line above. A panic here indicates a
    /// `phux-core::Registry` regression.
    pub fn seed_session(
        &mut self,
        name: &str,
    ) -> (SessionId, phux_core::ids::WindowId, TerminalId) {
        self.sessions.seed(name)
    }

    /// Add a new pane (Terminal) to `session`'s first window — the spawn
    /// counterpart to [`Self::seed_session`] that does NOT create a new
    /// session.
    ///
    /// A TUI split lands here (phux-i9zl): the new L1 Terminal joins the
    /// current session's window so `phux ls` keeps showing one session, and
    /// a reattach to that session resolves every split pane. Targets the
    /// session's first window — v0.1 sessions are single-window, so that is
    /// the window the client is viewing; multi-window targeting (the client's
    /// active window) is future work.
    ///
    /// Returns `None` if `session` is unknown or has no window — unreachable
    /// for a seeded session, which always has at least one window.
    #[must_use]
    pub fn add_pane_to_session(&mut self, session: SessionId) -> Option<TerminalId> {
        self.sessions.add_pane(session)
    }

    /// Add a pane to the exact window that owns `owner`.
    ///
    /// Used by headless spawn ownership targeting: the caller names a known
    /// Terminal so the request cannot drift into another session or window.
    /// Layout geometry remains client-owned L3 metadata.
    ///
    /// Stays on this type: resolving the caller's wire id runs through
    /// `state::id_space` before the session table can name `owner`.
    #[must_use]
    pub fn add_pane_to_terminal_owner(&mut self, owner: &WireTerminalId) -> Option<TerminalId> {
        let owner = self.terminal_from_wire(owner)?;
        self.sessions.add_pane_beside(owner)
    }
}
