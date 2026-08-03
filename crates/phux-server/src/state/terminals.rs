use std::future::Future;

use phux_core::ids::TerminalId;
use phux_protocol::ids::TerminalId as WireTerminalId;
use tokio_util::sync::CancellationToken;

use super::{ClientId, ServerState};
use crate::terminal_actor::TerminalHandle;

impl ServerState {
    /// Subscribers (snapshot) for `pane`. Returns an empty slice if no
    /// clients are currently observing the pane.
    #[must_use]
    pub fn subscribers_for_terminal(&self, terminal: TerminalId) -> &[ClientId] {
        self.terminal_subscribers
            .get(&terminal)
            .map_or(&[], Vec::as_slice)
    }

    /// Clone the [`TerminalHandle`] of every pane `client_id` currently
    /// subscribes to (phux-0q8). The runtime uses this at DETACH /
    /// disconnect / EOF time to send a
    /// [`ConsumerDetachRequest`](crate::terminal_actor::ConsumerDetachRequest) to each
    /// pane actor so the per-consumer `RenderState` cache (ADR-0018) is
    /// freed, mirroring the `register_consumer` calls the ATTACH path
    /// made. Gathered under-lock; the sends happen off-lock in the
    /// runtime to avoid awaiting inside `with_mut`.
    #[must_use]
    pub fn subscribed_terminal_handles(&self, client_id: ClientId) -> Vec<TerminalHandle> {
        self.terminal_subscribers
            .iter()
            .filter(|(_, subs)| subs.contains(&client_id))
            .filter_map(|(terminal, _)| self.terminal_handle(*terminal).cloned())
            .collect()
    }

    /// Record a freshly-spawned [`TerminalHandle`] against `pane` and
    /// allocate its wire id.
    ///
    /// Called by the runtime after `TerminalActor::new` /
    /// `build_with_token`. Subsequent attaches use
    /// [`Self::terminal_handle`] to look the handle up.
    ///
    /// `token` is stashed in `terminal_tokens`; cancelling it (e.g. via
    /// [`Self::detach_terminal_actor`]) fires the actor's shutdown branch.
    ///
    /// This method does NOT spawn the actor — pair it with
    /// [`Self::spawn_terminal_actor`] when you also want the actor task
    /// registered against the per-server `JoinSet`.
    ///
    /// Idempotent on the wire-id allocation (a second call for the
    /// same `pane` returns the same wire id) but overwrites the
    /// `TerminalHandle` / token. In practice the runtime calls this
    /// exactly once per pane lifetime.
    pub fn register_terminal_handle(
        &mut self,
        terminal: TerminalId,
        handle: TerminalHandle,
        token: CancellationToken,
    ) -> WireTerminalId {
        let wire = self.intern_terminal_wire(terminal);
        self.terminals.insert(terminal, handle);
        self.terminal_tokens.insert(terminal, token);
        wire
    }

    /// One-shot helper: register `handle`/`token` AND spawn
    /// `actor_future` onto the per-server pane `JoinSet`. Must be
    /// called from inside a `LocalSet` (per ADR-0014; pane actors
    /// own `!Send` `Terminal`s and are spawned via
    /// `JoinSet::spawn_local`).
    ///
    /// Returns the wire pane id, matching [`Self::register_terminal_handle`].
    pub fn spawn_terminal_actor<F>(
        &mut self,
        terminal: TerminalId,
        handle: TerminalHandle,
        token: CancellationToken,
        actor_future: F,
    ) -> WireTerminalId
    where
        F: Future<Output = ()> + 'static,
    {
        let wire = self.register_terminal_handle(terminal, handle, token);
        self.terminal_tasks.spawn_local(actor_future);
        wire
    }

    /// Cancel `pane`'s actor token, signalling the `TerminalActor` to
    /// exit, and forget the token. Idempotent. Used by future
    /// pane-close lifecycle paths; not exercised by `phux-byc.8`.
    ///
    /// The actor task itself is drained from the per-server `JoinSet`
    /// when it returns from `run`; we don't need to touch
    /// `terminal_tasks` here.
    pub fn detach_terminal_actor(&mut self, terminal: TerminalId) {
        if let Some(token) = self.terminal_tokens.remove(&terminal) {
            token.cancel();
        }
    }

    /// Look up the [`TerminalHandle`] for `pane`, if registered.
    #[must_use]
    pub fn terminal_handle(&self, terminal: TerminalId) -> Option<&TerminalHandle> {
        self.terminals.get(&terminal)
    }

    /// Register (and return the token for) an `ATTACH_TERMINAL` output
    /// pump for `(client, terminal)` (phux-v45.7). Returns `None` when a
    /// pump is already live for the pair — the idempotent re-attach must
    /// not double-stream.
    pub fn register_attach_terminal_pump(
        &mut self,
        client: ClientId,
        terminal: TerminalId,
    ) -> Option<tokio_util::sync::CancellationToken> {
        use std::collections::hash_map::Entry;
        match self.attach_terminal_pumps.entry((client, terminal)) {
            Entry::Occupied(_) => None,
            Entry::Vacant(slot) => {
                let token = tokio_util::sync::CancellationToken::new();
                slot.insert(token.clone());
                Some(token)
            }
        }
    }

    /// Cancel and forget the `ATTACH_TERMINAL` pump for `(client,
    /// terminal)`, if one is live. Idempotent.
    pub fn cancel_attach_terminal_pump(&mut self, client: ClientId, terminal: TerminalId) {
        if let Some(token) = self.attach_terminal_pumps.remove(&(client, terminal)) {
            token.cancel();
        }
    }

    /// Remove `client` from `terminal`'s subscriber list (the
    /// `DETACH_TERMINAL` counterpart of the attach-time registration).
    pub fn unsubscribe_terminal(&mut self, client: ClientId, terminal: TerminalId) {
        if let Some(subs) = self.terminal_subscribers.get_mut(&terminal) {
            subs.retain(|c| *c != client);
            if subs.is_empty() {
                self.terminal_subscribers.remove(&terminal);
            }
        }
    }
}
