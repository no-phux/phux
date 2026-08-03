use phux_core::ids::{SessionId, TerminalId};
use phux_core::session::Session;
use phux_protocol::ids::TerminalId as WireTerminalId;

use super::{RenameOutcome, ServerState};

impl ServerState {
    /// Most-recently-touched live session, if any. Resolves
    /// `AttachTarget::Last`.
    #[must_use]
    pub fn most_recently_touched_session(&self) -> Option<SessionId> {
        self.session_last_touched
            .iter()
            .filter(|(sid, _)| self.registry.session(**sid).is_some())
            .max_by_key(|(_, touched_at)| *touched_at)
            .map(|(sid, _)| *sid)
    }

    /// Mark `session` as touched by attach/input/focus activity.
    pub fn touch_session(&mut self, session: SessionId) {
        let touched_at = self.next_touch_timestamp;
        self.next_touch_timestamp = self.next_touch_timestamp.saturating_add(1);
        self.session_last_touched.insert(session, touched_at);
    }

    /// Look up the active pane of the active window of `session`, if any.
    #[must_use]
    pub fn active_pane_of_session(&self, session: SessionId) -> Option<TerminalId> {
        let session = self.registry.session(session)?;
        let window_id = session.active?;
        let window = self.registry.window(window_id)?;
        window.active
    }

    /// Borrow the session named `name`, if it exists.
    #[must_use]
    pub fn session_by_name(&self, name: &str) -> Option<&Session> {
        let id = self.find_session_by_name(name)?;
        self.registry.session(id)
    }

    /// Look up the [`SessionId`] for a name by scanning the registry.
    ///
    /// Uses [`Registry::sessions`] directly — no side ledger required.
    pub(super) fn find_session_by_name(&self, name: &str) -> Option<SessionId> {
        self.registry
            .sessions()
            .find(|(_, s)| s.name == name)
            .map(|(id, _)| id)
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
        let Some(id) = self.find_session_by_name(current) else {
            return RenameOutcome::NotFound;
        };
        // A no-op rename (current == new_name) resolves to the same session,
        // so the duplicate check would otherwise reject it. Treat it as
        // success: the name already is what was asked for.
        if current != new_name && self.find_session_by_name(new_name).is_some() {
            return RenameOutcome::NameTaken;
        }
        // Resolution above guarantees the id is live, so `session_mut` is
        // `Some`; the rename is a single field write.
        if let Some(session) = self.registry.session_mut(id) {
            new_name.clone_into(&mut session.name);
        }
        RenameOutcome::Renamed
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
    #[allow(clippy::expect_used, reason = "unreachable: parent just created")]
    pub fn seed_session(
        &mut self,
        name: &str,
    ) -> (SessionId, phux_core::ids::WindowId, TerminalId) {
        let sid = self.registry.new_session(name.to_owned());
        let wid = self.registry.new_window(sid).expect("session just created");
        let pid = self
            .registry
            .new_terminal(wid)
            .expect("window just created");
        (sid, wid, pid)
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
        let wid = self.registry.session(session)?.windows.first().copied()?;
        self.registry.new_terminal(wid).ok()
    }

    /// Add a pane to the exact window that owns `owner`.
    ///
    /// Used by headless spawn ownership targeting: the caller names a known
    /// Terminal so the request cannot drift into another session or window.
    /// Layout geometry remains client-owned L3 metadata.
    #[must_use]
    pub fn add_pane_to_terminal_owner(&mut self, owner: &WireTerminalId) -> Option<TerminalId> {
        let owner = self.terminal_from_wire(owner)?;
        let window = self.registry.terminal(owner)?.window;
        self.registry.new_terminal(window).ok()
    }
}
