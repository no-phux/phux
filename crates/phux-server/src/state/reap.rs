use phux_core::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::ids::SessionId as WireSessionId;

use super::{KeepEmptyOutcome, ServerState};

/// The layout-family key suffix `.layout/v1/<wire session id>` (L3.md §3.2,
/// §3.5), used to find every layout key naming a reaped session.
fn layout_key_suffix(session: WireSessionId) -> String {
    format!(".layout/v1/{}", session.get())
}

/// The wire session id a `<prefix>.layout/v1/<id>` key names, or `None` for
/// a non-layout key or a non-canonical id (which the reap would never
/// match anyway).
pub(crate) fn layout_key_session_id(key: &str) -> Option<u32> {
    let (prefix, suffix) = key.rsplit_once(".layout/v1/")?;
    if prefix.is_empty() || prefix.contains(".layout/v1/") {
        return None;
    }
    let id = suffix.parse::<u32>().ok()?;
    (suffix == id.to_string()).then_some(id)
}

impl ServerState {
    /// Whether this is a default-Group layout key naming a dead session
    /// (ADR-0129): a TUI republish can race the reap and would resurrect
    /// the key. `None` when not a layout key in that Group.
    #[must_use]
    pub fn layout_key_names_a_dead_session(
        &self,
        scope: &phux_protocol::wire::frame::Scope,
        key: &str,
    ) -> Option<bool> {
        if !matches!(scope, phux_protocol::wire::frame::Scope::Group(gid) if *gid == super::DEFAULT_GROUP_ID)
        {
            return None;
        }
        let wire_id = layout_key_session_id(key)?;
        let live = self
            .idspace
            .resolve_session(WireSessionId::new(wire_id))
            .is_some();
        Some(!live)
    }
}

impl ServerState {
    /// Reap a pane whose actor exited, cascading pane → window → session,
    /// and free its server-side bookkeeping. Returns whether the server now
    /// has no sessions (the self-exit signal). Idempotent.
    pub fn reap_terminal(&mut self, pane: ResourceId) -> bool {
        let (empty, token) = self.reap_terminal_inner(pane);
        if let Some(token) = token {
            token.cancel();
        }
        empty
    }

    /// Reap `pane` but leave its actor running until the caller cancels the
    /// returned token (so a fenced pump can publish the last screen).
    pub(crate) fn reap_terminal_deferring_actor_cancel(
        &mut self,
        pane: ResourceId,
    ) -> (bool, Option<tokio_util::sync::CancellationToken>) {
        self.reap_terminal_inner(pane)
    }

    fn reap_terminal_inner(
        &mut self,
        pane: ResourceId,
    ) -> (bool, Option<tokio_util::sync::CancellationToken>) {
        let window_id = self.sessions.registry.resource(pane).and_then(|t| t.window);
        // `remove_resource` also drops every bound descendant from the
        // registry. One still there (no close cascade ran first, e.g. a
        // failed publication) must lose its handle and engine too: its own
        // watcher will find it gone and reap nothing, so without this its
        // handle would outlive it in the resource table forever.
        let descendants = self.resource_descendants(pane);
        let token = if self.sessions.registry.remove_resource(pane).is_some() {
            for child in descendants {
                if let Some(child_token) = self.forget_terminal_bookkeeping(child) {
                    child_token.cancel();
                }
            }
            self.forget_terminal_bookkeeping(pane)
        } else {
            None
        };
        let Some(window_id) = window_id else {
            return (self.sessions.registry.session_count() == 0, token);
        };

        self.reap_window_if_empty(window_id);

        (self.sessions.registry.session_count() == 0, token)
    }

    /// Remove `window` if it has no panes, and its session if that leaves
    /// none (shared by reaping and `MOVE_RESOURCE`, ADR-0056). A keep-empty
    /// session stays with zero windows (ADR-0105). Either way the session's
    /// layout keys are forgotten.
    pub fn reap_window_if_empty(&mut self, window_id: WindowId) {
        let Some(window) = self.sessions.registry.window(window_id) else {
            return;
        };
        if !window.slots.is_empty() {
            return;
        }
        let session_id = window.session;
        if self.sessions.registry.remove_window(window_id).is_some() {
            self.forget_window_bookkeeping(window_id);
        }
        if !self.reap_session_if_empty(session_id) {
            let wire = self.idspace.session_wire(session_id);
            self.forget_layout_of_emptied_session(session_id, wire);
        }
    }

    /// A keep-empty session that lost its last window drops every layout
    /// key naming it (ADR-0105), so a later attach does not adopt panes
    /// that no longer exist.
    fn forget_layout_of_emptied_session(
        &mut self,
        session_id: SessionId,
        wire: Option<WireSessionId>,
    ) {
        let emptied = self
            .sessions
            .registry
            .session(session_id)
            .is_some_and(|s| s.keep_empty && s.windows.is_empty());
        if emptied {
            self.forget_layout_keys_of_session(wire);
        }
    }

    /// Delete every default-Group `<prefix>.layout/v1/<id>` key for the
    /// session (no-op if it never had a wire id).
    fn forget_layout_keys_of_session(&mut self, wire: Option<WireSessionId>) {
        let Some(wire) = wire else {
            return;
        };
        let suffix = layout_key_suffix(wire);
        let scope = phux_protocol::wire::frame::Scope::Group(super::DEFAULT_GROUP_ID);
        let keys: Vec<String> = self
            .metadata()
            .list(&scope)
            .into_iter()
            .filter(|key| key.ends_with(&suffix))
            .collect();
        for key in keys {
            let _ = self.metadata_delete(&scope, &key);
        }
    }

    /// Drain released sessions the cascade removed (ADR-0105).
    pub fn take_killed_sessions(&mut self) -> Vec<SessionId> {
        self.sessions.take_killed()
    }

    /// Remove `session` if windowless and not keep-empty, deleting its
    /// layout keys (ADR-0129). The single session-removal choke point.
    fn reap_session_if_empty(&mut self, session_id: SessionId) -> bool {
        let reapable = self
            .sessions
            .registry
            .session(session_id)
            .is_some_and(|s| s.windows.is_empty() && !s.keep_empty);
        if !reapable {
            return false;
        }
        let wire = self.idspace.session_wire(session_id);
        if self.sessions.registry.remove_session(session_id).is_none() {
            return false;
        }
        self.sessions.note_removed(session_id);
        self.forget_session_bookkeeping(session_id);
        self.forget_layout_keys_of_session(wire);
        true
    }

    /// Set or clear `phux.session.keep_empty/v1` on `name` (ADR-0105).
    /// Clearing it on a windowless session removes the session.
    pub fn set_session_keep_empty(&mut self, name: &str, keep: bool) -> KeepEmptyOutcome {
        let Some(session_id) = self.sessions.find_by_name(name) else {
            return KeepEmptyOutcome::NotFound;
        };
        let changed = self
            .sessions
            .registry
            .session_mut(session_id)
            .is_some_and(|session| std::mem::replace(&mut session.keep_empty, keep) != keep);
        if self.reap_session_if_empty(session_id) {
            return KeepEmptyOutcome::Removed;
        }
        if changed {
            KeepEmptyOutcome::Changed
        } else {
            KeepEmptyOutcome::Unchanged
        }
    }

    /// Clear keep-empty on every session whose Terminals are all in
    /// `targets` (a group teardown), remembering them for detach. Returns
    /// their names.
    pub fn release_keep_empty_covered_by(&mut self, targets: &[ResourceId]) -> Vec<String> {
        let covered: Vec<SessionId> = self
            .sessions
            .registry
            .sessions()
            .filter(|(_, session)| session.keep_empty)
            .filter(|(_, session)| self.session_terminals_within(session, targets))
            .map(|(id, _)| id)
            .collect();
        let mut names = Vec::with_capacity(covered.len());
        for session_id in covered {
            if let Some(session) = self.sessions.registry.session_mut(session_id) {
                session.keep_empty = false;
                names.push(session.name.clone());
            }
            self.sessions.mark_released(session_id);
        }
        names
    }

    /// Whether `session` has a pane and all are in `targets`.
    fn session_terminals_within(
        &self,
        session: &phux_core::session::Session,
        targets: &[ResourceId],
    ) -> bool {
        let mut panes = session
            .windows
            .iter()
            .filter_map(|wid| self.sessions.registry.window(*wid))
            .flat_map(|window| window.slots.iter())
            .peekable();
        panes.peek().is_some() && panes.all(|pane| targets.contains(pane))
    }

    /// Drop every map entry keyed on a removed pane and retire its wire id
    /// (never reused).
    fn forget_terminal_bookkeeping(
        &mut self,
        pane: ResourceId,
    ) -> Option<tokio_util::sync::CancellationToken> {
        // Pumps and subscriptions go now; the actor token comes back
        // uncancelled so the last screen can publish.
        let token = self.resources.forget_resource(pane);
        // An unclaimed close reason must not outlive its id.
        self.forget_close_reason(pane);
        self.retained.forget(pane);
        // Asked state is keyed by core id; the arbiter by wire id (below).
        self.agent.clear_asked(pane);
        // The retired wire id keys the metadata scope and the arbiter.
        if let Some(wire) = self.idspace.retire_terminal(pane) {
            self.metadata.forget_terminal(&wire);
            // A viewer mark (ADR-0127) must not outlive the Terminal it names.
            self.clients.forget_viewed_terminal(&wire);
            // A recycled wire id must not inherit a stale declaration.
            self.agent.forget_record(&wire);
        }
        token
    }

    /// Retire a removed window's wire-id mapping (no reuse).
    fn forget_window_bookkeeping(&mut self, window: WindowId) {
        self.idspace.retire_window(window);
        self.sessions.forget_window(window);
    }

    /// Forget a removed session's wire id and last-touch ordering entry.
    fn forget_session_bookkeeping(&mut self, session: SessionId) {
        self.idspace.forget_session(session);
        self.sessions.forget_session(session);
    }
}
