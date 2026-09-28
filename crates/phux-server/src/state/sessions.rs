use phux_core::ids::{ResourceId, SessionId};
use phux_core::registry::Registry;
use phux_core::session::Session;
use phux_protocol::ids::ResourceId as WireResourceId;

use super::{RenameOutcome, ServerState};

impl ServerState {
    /// The session/window/pane registry.
    #[must_use]
    pub const fn registry(&self) -> &Registry {
        &self.sessions.registry
    }

    /// The registry, mutably (borrows all of `ServerState`).
    pub const fn registry_mut(&mut self) -> &mut Registry {
        &mut self.sessions.registry
    }

    /// Most-recently-touched live session, if any. Resolves
    /// `AttachTarget::Last`.
    #[must_use]
    pub fn most_recently_touched_session(&self) -> Option<SessionId> {
        self.sessions.most_recently_touched()
    }

    /// Whether this server has any session-touch history, even if every
    /// touched session has since gone away.
    #[must_use]
    pub(crate) fn has_session_touch_history(&self) -> bool {
        self.sessions.has_touch_history()
    }

    /// Mark `session` as touched by attach/input/focus activity.
    pub fn touch_session(&mut self, session: SessionId) {
        self.sessions.touch(session);
    }

    /// Look up the active pane of the active window of `session`, if any.
    #[must_use]
    pub fn active_pane_of_session(&self, session: SessionId) -> Option<ResourceId> {
        self.sessions.active_pane_of(session)
    }

    /// Borrow the session named `name`, if it exists.
    #[must_use]
    pub fn session_by_name(&self, name: &str) -> Option<&Session> {
        self.sessions.by_name(name)
    }

    /// The [`SessionId`] named `name`.
    pub(crate) fn find_session_by_name(&self, name: &str) -> Option<SessionId> {
        self.sessions.find_by_name(name)
    }

    /// Rename `current` to `new_name` (names are unique); the outcome maps
    /// to `SESSION_NOT_FOUND`, `INVALID_COMMAND`, or success.
    pub fn rename_session(&mut self, current: &str, new_name: &str) -> RenameOutcome {
        self.sessions.rename(current, new_name)
    }

    /// Seed a session, window, and pane (the pre-seed entry point).
    ///
    /// # Panics
    ///
    /// Only on a `Registry` regression.
    pub fn seed_session(
        &mut self,
        name: &str,
    ) -> (SessionId, phux_core::ids::WindowId, ResourceId) {
        self.sessions.seed(name)
    }

    /// Create a keep-empty session named `name` with zero windows and return
    /// its id (ADR-0105). The caller checks the name is free first.
    pub fn seed_empty_session(&mut self, name: &str) -> SessionId {
        self.sessions.seed_empty(name)
    }

    /// Add a pane to `session`'s first window (a TUI split), without a new
    /// session. `None` if `session` is unknown.
    #[must_use]
    pub fn add_pane_to_session(&mut self, session: SessionId) -> Option<ResourceId> {
        self.sessions.add_pane(session)
    }

    /// Add a pane to the window that owns `owner` (headless spawn
    /// targeting).
    #[must_use]
    pub fn add_pane_to_terminal_owner(&mut self, owner: &WireResourceId) -> Option<ResourceId> {
        let owner = self.terminal_from_wire(owner)?;
        self.sessions.add_pane_beside(owner)
    }

    /// Whether this Terminal's natural exit should respawn a shell in place
    /// (it is the session's only Terminal and nobody killed it).
    #[must_use]
    pub(crate) fn should_replace_last_shell(&self, pane: ResourceId) -> bool {
        if matches!(
            self.pending_close_reason(pane),
            Some(
                phux_protocol::wire::frame::CloseReason::Killed
                    | phux_protocol::wire::frame::CloseReason::ParentClosed
                    | phux_protocol::wire::frame::CloseReason::ServerShutdown
            )
        ) {
            return false;
        }
        self.is_sole_session_terminal(pane)
    }

    fn is_sole_session_terminal(&self, pane: ResourceId) -> bool {
        let Some(desc) = self.sessions.registry.resource(pane) else {
            return false;
        };
        if desc.kind != phux_core::resource::ResourceKind::Terminal {
            return false;
        }
        let Some(window_id) = desc.window else {
            return false;
        };
        let Some(session_id) = self.sessions.registry.window(window_id).map(|w| w.session) else {
            return false;
        };
        let terminals = self.session_terminal_ids(session_id);
        terminals.len() == 1 && terminals[0] == pane
    }

    fn session_terminal_ids(&self, session: SessionId) -> Vec<ResourceId> {
        self.sessions
            .registry
            .session(session)
            .into_iter()
            .flat_map(|session| session.windows.iter().copied())
            .filter_map(|window| self.sessions.registry.window(window))
            .flat_map(|window| window.slots.iter().copied())
            .filter(|&id| {
                self.sessions.registry.resource(id).is_some_and(|resource| {
                    resource.kind == phux_core::resource::ResourceKind::Terminal
                })
            })
            .collect()
    }
}
