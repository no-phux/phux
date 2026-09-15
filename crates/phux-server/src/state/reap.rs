use phux_core::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::ids::SessionId as WireSessionId;

use super::{KeepEmptyOutcome, ServerState};

/// The layout-family metadata suffix (L3.md §3.2, §3.5): every consumer's
/// per-session layout key, default or named projection alike, is
/// `<prefix>.layout/v1/<wire session id>` in Group 1. The server never
/// interprets the prefix; it only ever needs to find "every layout key that
/// named this now-gone session" so a reap does not orphan one.
fn layout_key_suffix(session: WireSessionId) -> String {
    format!(".layout/v1/{}", session.get())
}

/// Parse the wire session id a `<prefix>.layout/v1/<id>` key names (default
/// key or named projection alike), for the resurrection guard in
/// `runtime::client::reject_set_metadata`. `None` when `key` is not in the
/// layout family at all — including a non-canonical id (a leading zero, a
/// `+` sign, or anything but ASCII digits), which can never be the literal
/// suffix [`Self::forget_layout_keys_of_session`] matches, so treating it as
/// "not a layout key" here is the same "never found, never orphaned by this
/// guard" answer the client-side `phux_client::layout_ops::projection_key_session`
/// grammar gives.
pub(crate) fn layout_key_session_id(key: &str) -> Option<u32> {
    let (prefix, suffix) = key.rsplit_once(".layout/v1/")?;
    if prefix.is_empty() || prefix.contains(".layout/v1/") {
        return None;
    }
    let id = suffix.parse::<u32>().ok()?;
    (suffix == id.to_string()).then_some(id)
}

impl ServerState {
    /// Whether `scope`/`key` is a `*.layout/v1/<id>` key in the default
    /// layout Group naming a session that is no longer live (ADR-0129
    /// resurrection guard). `None` when `key` is not in the layout family,
    /// or `scope` is not the default layout Group, so the caller's normal
    /// rejection path continues unaffected.
    ///
    /// The TUI republishes its layout on `RESOURCE_CLOSED` for whichever
    /// panes survive a close (`phux-tui`'s `loop_state.rs`); that broadcast
    /// can lose a race with the pane's own reap and land afterward, holding
    /// the dead session's real (canonical) wire id. Without this guard the
    /// late write recreates the very key the reap cascade just deleted —
    /// this session is gone, but its `*.layout/v1/<id>` key would live on
    /// forever with no reap left to ever clean it up again.
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
    ///
    /// Either way the session collapses to zero windows here — kept alive or
    /// fully removed — so its layout keys (the default TUI key and any named
    /// `--projection`, ADR-0129) are forgotten in both branches. The
    /// internal `reap_session_if_empty` is the actual-removal path's own
    /// choke point for that (every caller of it gets the deletion for
    /// free, including [`Self::set_session_keep_empty`]'s "clear the mark
    /// on an already-windowless session" path — see that method's doc);
    /// this function only has to handle its own "kept alive" branch.
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

    /// ADR-0105: a keep-empty session that just lost its last window keeps
    /// no layout. The stored layout tree(s) now name only dead panes, and a
    /// client that attaches later would adopt one and hide the empty state
    /// behind panes that never bootstrap, so every layout key naming this
    /// session is deleted here (broadcasting the tombstone) whether or not a
    /// client was attached to write one.
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

    /// Delete every `<prefix>.layout/v1/<wire session id>` metadata key in
    /// the default layout Group (L3.md §3.2, §3.5): the reference TUI's own
    /// key plus any consumer's named projection over the same session
    /// envelope shape. A no-op when `wire` is `None` (the session was never
    /// interned, so it never had a layout key).
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

    /// Drain the sessions a group kill released and the cascade has since
    /// removed (ADR-0105). The reap path detaches their attached clients.
    pub fn take_killed_sessions(&mut self) -> Vec<SessionId> {
        self.sessions.take_killed()
    }

    /// Remove `session` when it holds no windows and is not keep-empty.
    /// Returns `true` iff the session was removed.
    ///
    /// The single choke point every session-removal path goes through
    /// ([`Self::reap_window_if_empty`]'s cascade and
    /// [`Self::set_session_keep_empty`]'s "clear the mark on an
    /// already-windowless session" path both call this, not the registry
    /// directly), so it is where the session's layout keys (ADR-0129) are
    /// deleted — no removal path can add itself later and forget to. The
    /// wire id is captured before the removal, whose own bookkeeping
    /// retires it.
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
