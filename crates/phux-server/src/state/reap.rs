use phux_core::ids::{ResourceId, SessionId, WindowId};

use super::{KeepEmptyOutcome, ServerState};

/// The reference TUI's per-session layout key prefix (L3.md §3.2); the full
/// key is `phux.tui.layout/v1/<wire session id>` in Group 1. Mirrors
/// `phux_client::layout_ops::LAYOUT_KEY`, which the server crate cannot
/// depend on. The server otherwise never interprets this key.
const TUI_LAYOUT_KEY: &str = "phux.tui.layout/v1";

impl ServerState {
    /// Reap a pane whose actor has exited, cascading the removal up the
    /// `pane → window → session` tree (phux-60s, the tmux server-lifecycle
    /// model). When the pane's window has no panes left the window is
    /// removed; when that window's session has no windows left the session
    /// is removed.
    ///
    /// Returns `true` iff the server now holds zero sessions — the signal
    /// the runtime uses to self-exit (nothing left to serve). Idempotent on
    /// an unknown or already-reaped pane: it touches nothing and reports the
    /// current emptiness.
    ///
    /// This is the structural counterpart to the `on_terminal_exited`
    /// path in `runtime.rs`: that path detaches clients focused on the
    /// dead pane; this one frees the domain entities and their server-side
    /// bookkeeping (actor handle, token, input log, subscribers, wire-id
    /// interning, and per-Terminal L3 metadata).
    pub fn reap_terminal(&mut self, pane: ResourceId) -> bool {
        // Resolve the parent window before the registry drops the pane.
        let window_id = self.sessions.registry.resource(pane).and_then(|t| t.window);
        if self.sessions.registry.remove_resource(pane).is_some() {
            self.forget_terminal_bookkeeping(pane);
        }
        let Some(window_id) = window_id else {
            return self.sessions.registry.session_count() == 0;
        };

        self.reap_window_if_empty(window_id);

        self.sessions.registry.session_count() == 0
    }

    /// Cascade the `window → session` half of [`Self::reap_terminal`]:
    /// remove `window` when it holds no panes, and its session when that
    /// leaves the session with no windows. A no-op on a still-populated or
    /// unknown window. Shared by pane reaping and `MOVE_RESOURCE`
    /// (ADR-0056), whose re-parent can empty the source window without any
    /// pane dying.
    ///
    /// The cascade stops at a keep-empty session (ADR-0105): the window goes
    /// and the session stays, with zero windows, until an explicit kill.
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
            self.forget_layout_of_emptied_session(session_id);
        }
    }

    /// ADR-0105: a keep-empty session that just lost its last window keeps
    /// no layout. The stored `phux.tui.layout/v1/<session>` tree now names
    /// only dead panes, and a client that attaches later would adopt it and
    /// hide the empty state behind panes that never bootstrap, so it is
    /// deleted here (broadcasting the tombstone) whether or not a client was
    /// attached to write one.
    fn forget_layout_of_emptied_session(&mut self, session_id: SessionId) {
        let emptied = self
            .sessions
            .registry
            .session(session_id)
            .is_some_and(|s| s.keep_empty && s.windows.is_empty());
        let Some(wire) = self.idspace.session_wire(session_id).filter(|_| emptied) else {
            return;
        };
        let key = format!("{TUI_LAYOUT_KEY}/{}", wire.get());
        let _ = self.metadata_delete(
            &phux_protocol::wire::frame::Scope::Group(super::DEFAULT_GROUP_ID),
            &key,
        );
    }

    /// Drain the sessions a group kill released and the cascade has since
    /// removed (ADR-0105). The reap path detaches their attached clients.
    pub fn take_killed_sessions(&mut self) -> Vec<SessionId> {
        self.sessions.take_killed()
    }

    /// Remove `session` when it holds no windows and is not keep-empty.
    /// Returns `true` iff the session was removed.
    fn reap_session_if_empty(&mut self, session_id: SessionId) -> bool {
        let reapable = self
            .sessions
            .registry
            .session(session_id)
            .is_some_and(|s| s.windows.is_empty() && !s.keep_empty);
        if !reapable || self.sessions.registry.remove_session(session_id).is_none() {
            return false;
        }
        self.sessions.note_removed(session_id);
        self.forget_session_bookkeeping(session_id);
        true
    }

    /// Apply a `phux.session.keep_empty/v1` write: set the keep-empty mark on
    /// the session named `name` (ADR-0105).
    ///
    /// Clearing the mark on a session with no windows removes it through the
    /// same bookkeeping the cascade uses, because a windowless session that
    /// reaps normally would already have been reaped. That is how an empty
    /// session is killed. Clearing it on a populated session only restores
    /// the default cascade for when its last window closes.
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

    /// Clear the keep-empty mark on every session whose Terminals are all in
    /// `targets` (ADR-0105): a `KILL_RESOURCES` naming a whole session is a
    /// group teardown, so the ordinary cascade removes the session once its
    /// panes are reaped. A session that also holds a pane outside `targets`
    /// keeps its mark. Each released session is remembered, so the reap path
    /// detaches its attached clients once the cascade removes it. Returns the
    /// released sessions' names, for the caller to broadcast.
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

    /// Whether `session` holds at least one pane and every one of them is in
    /// `targets`.
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

    /// Drop every server-side map entry keyed on a now-removed pane.
    ///
    /// Cancels the actor token defensively (the actor has usually already
    /// exited by the time we reap, but a still-live token is cleanly
    /// resolved by the cancel) and retires the wire id without reuse.
    fn forget_terminal_bookkeeping(&mut self, pane: ResourceId) {
        // Handle, actor token, subscribers, and the pane's ATTACH_RESOURCE
        // pumps (phux-v45.7) all go in one step.
        self.resources.forget_resource(pane);
        // The close reason was claimed by whoever emitted RESOURCE_CLOSED;
        // an entry still here belonged to a resource that never got that
        // far, and it must not outlive the id it is filed under.
        self.forget_close_reason(pane);
        // The asked-detector is keyed by core pane id, so it clears before
        // the wire id is retired; the arbiter half is keyed by wire id and
        // clears after.
        self.agent.clear_asked(pane);
        // The retired wire id is the key the per-Terminal metadata scope and
        // the agent-record arbiter are filed under, so `retire_terminal`
        // hands it back rather than dropping it.
        if let Some(wire) = self.idspace.retire_terminal(pane) {
            self.metadata.forget_terminal(&wire);
            // The record died with the per-Terminal metadata scope; the
            // arbiter's bookkeeping about who owned it must not outlive it,
            // or a recycled wire id would inherit a stale declaration.
            self.agent.forget_record(&wire);
        }
    }

    /// Retire a removed window's wire-id mapping (no reuse).
    fn forget_window_bookkeeping(&mut self, window: WindowId) {
        self.idspace.retire_window(window);
        // Drop the last-cwd-per-window ledger entry (phux-nyx) so a reused
        // window id can never inherit a dead window's directory.
        self.sessions.forget_window(window);
    }

    /// Forget a removed session's wire id and last-touch ordering entry.
    fn forget_session_bookkeeping(&mut self, session: SessionId) {
        self.idspace.forget_session(session);
        // Drops the last-touch stamp and the frozen session-root entry
        // (phux-nyx) alongside the rest of the session's bookkeeping.
        self.sessions.forget_session(session);
    }
}
