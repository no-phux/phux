use phux_core::ids::{SessionId, TerminalId, WindowId};

use super::ServerState;

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
    pub fn reap_terminal(&mut self, pane: TerminalId) -> bool {
        // Resolve the parent window before the registry drops the pane.
        let window_id = self.sessions.registry.terminal(pane).map(|t| t.window);
        if self.sessions.registry.remove_terminal(pane).is_some() {
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
    /// unknown window. Shared by pane reaping and `MOVE_TERMINAL`
    /// (ADR-0056), whose re-parent can empty the source window without any
    /// pane dying.
    pub fn reap_window_if_empty(&mut self, window_id: WindowId) {
        let window_empty = self
            .sessions
            .registry
            .window(window_id)
            .is_some_and(|w| w.panes.is_empty());
        if window_empty {
            let session_id = self.sessions.registry.window(window_id).map(|w| w.session);
            if self.sessions.registry.remove_window(window_id).is_some() {
                self.forget_window_bookkeeping(window_id);
            }
            if let Some(session_id) = session_id {
                let session_empty = self
                    .sessions
                    .registry
                    .session(session_id)
                    .is_some_and(|s| s.windows.is_empty());
                if session_empty && self.sessions.registry.remove_session(session_id).is_some() {
                    self.forget_session_bookkeeping(session_id);
                }
            }
        }
    }

    /// Drop every server-side map entry keyed on a now-removed pane.
    ///
    /// Cancels the actor token defensively (the actor has usually already
    /// exited by the time we reap, but a still-live token is cleanly
    /// resolved by the cancel) and retires the wire id without reuse.
    fn forget_terminal_bookkeeping(&mut self, pane: TerminalId) {
        // Handle, actor token, subscribers, and the pane's ATTACH_TERMINAL
        // pumps (phux-v45.7) all go in one step.
        self.terminal_table.forget_terminal(pane);
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
