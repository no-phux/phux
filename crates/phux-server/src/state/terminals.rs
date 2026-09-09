use std::future::Future;

use phux_core::ids::TerminalId;
use phux_protocol::ids::{BootstrapId, TerminalId as WireTerminalId};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::terminal_table::AttachTerminalPumpReplacement;
use super::{ClientId, ServerState};
use crate::mailbox::Outbound;
use crate::terminal_actor::TerminalHandle;

impl ServerState {
    /// Subscribers (snapshot) for `pane`. Returns an empty slice if no
    /// clients are currently observing the pane.
    #[must_use]
    pub fn subscribers_for_terminal(&self, terminal: TerminalId) -> &[ClientId] {
        self.terminal_table.subscribers_for(terminal)
    }

    /// Subscribe `client` to `terminal`'s output fanout, deduplicating so a
    /// re-attach cannot double-register.
    ///
    /// The `ATTACH_TERMINAL` / `SPAWN_TERMINAL` counterpart of
    /// [`Self::unsubscribe_terminal`]; whole-session subscription happens
    /// inside [`Self::attach`].
    ///
    /// `mailbox` is the subscriber's outbound channel. Pass `Some` from any
    /// path that can subscribe a client with no session-scoped `ATTACH`
    /// behind it (`ATTACH_TERMINAL`), so terminal-scoped fanout
    /// ([`Self::terminal_fanout_targets`]) can reach it; `None` is correct
    /// only where the caller has just verified the client is in
    /// [`Self::attached`], which already carries the same sender.
    pub fn subscribe_terminal(
        &mut self,
        client: ClientId,
        terminal: TerminalId,
        mailbox: Option<mpsc::Sender<Outbound>>,
    ) {
        if let Some(tx) = mailbox {
            self.clients.remember_terminal_mailbox(client, tx);
        }
        self.terminal_table.subscribe(client, terminal);
    }

    /// Outbound mailboxes of every client subscribed to `terminal`, for the
    /// server's out-of-band terminal-scoped fanout (`TERMINAL_CLOSED`).
    ///
    /// Each subscriber is resolved to exactly one mailbox —
    /// [`Self::attached`] first, then the `ATTACH_TERMINAL` subscription
    /// mailbox — so a session-attached client receives one frame and an
    /// `ATTACH_TERMINAL`-only consumer receives one frame, per L1 §3.1's
    /// "every client subscribed to the Terminal". Resolving through
    /// [`Self::attached`] alone silently dropped the second kind
    /// (phux-w7z2.56).
    ///
    /// A subscriber whose mailbox has already gone (connection tearing
    /// down) is skipped; the fanout is best-effort by construction.
    #[must_use]
    pub fn terminal_fanout_targets(&self, terminal: TerminalId) -> Vec<mpsc::Sender<Outbound>> {
        self.subscribers_for_terminal(terminal)
            .iter()
            .filter_map(|client| self.clients.terminal_fanout_mailbox(*client).cloned())
            .collect()
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
        self.terminal_table.subscribed_handles(client_id)
    }

    /// Record a freshly-spawned [`TerminalHandle`] against `pane` and
    /// allocate its wire id.
    ///
    /// Called by the runtime after `TerminalActor::new` /
    /// `build_with_token`. Subsequent attaches use
    /// [`Self::terminal_handle`] to look the handle up.
    ///
    /// `token` is stashed alongside the handle; cancelling it (e.g. via
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
    ///
    /// Stays on `ServerState` rather than moving onto the terminal table:
    /// the wire-id mint and the two table writes are one atomic step the
    /// runtime depends on, and the id space is a different concern.
    pub fn register_terminal_handle(
        &mut self,
        terminal: TerminalId,
        handle: TerminalHandle,
        token: CancellationToken,
    ) -> WireTerminalId {
        let wire = self.intern_terminal_wire(terminal);
        self.terminal_table.register(terminal, handle, token);
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
        self.terminal_table.spawn_actor(actor_future);
        wire
    }

    /// Cancel `pane`'s actor token, signalling the `TerminalActor` to
    /// exit, and forget the token. Idempotent. Used by future
    /// pane-close lifecycle paths; not exercised by `phux-byc.8`.
    ///
    /// The actor task itself is drained from the per-server `JoinSet`
    /// when it returns from `run`; we don't need to touch the pane-task
    /// set here.
    pub fn detach_terminal_actor(&mut self, terminal: TerminalId) {
        self.terminal_table.detach_actor(terminal);
    }

    /// Look up the [`TerminalHandle`] for `pane`, if registered.
    #[must_use]
    pub fn terminal_handle(&self, terminal: TerminalId) -> Option<&TerminalHandle> {
        self.terminal_table.handle(terminal)
    }

    /// Clone every registered `(pane, handle)` pair so the caller can talk
    /// to the actors outside the `ServerState` lock (the `Arc<Mutex<_>>`
    /// must not be held across an await).
    pub(crate) fn all_terminal_handles(&self) -> Vec<(TerminalId, TerminalHandle)> {
        self.terminal_table.all_handles()
    }

    /// Install a new `ATTACH_TERMINAL` pump generation for `(client,
    /// terminal)`, displacing any live one.
    ///
    /// See `TerminalTable::replace_pump` for what the returned tuple carries
    /// and why a second attach replaces rather than being refused. (Not a
    /// rustdoc link: that method is `pub(super)`, and rustdoc does not document
    /// private items.)
    pub fn replace_attach_terminal_pump(
        &mut self,
        client: ClientId,
        terminal: TerminalId,
        bootstrap_id: BootstrapId,
    ) -> AttachTerminalPumpReplacement {
        self.terminal_table
            .replace_pump(client, terminal, bootstrap_id)
    }

    /// Allocate the next per-terminal bootstrap id for `client`, or `None`
    /// once the connection has exhausted its id space.
    pub fn next_attach_terminal_bootstrap_id(&mut self, client: ClientId) -> Option<BootstrapId> {
        self.terminal_table.next_bootstrap_id(client)
    }

    /// Cancel and forget the `ATTACH_TERMINAL` pump for `(client,
    /// terminal)`, if one is live. Idempotent.
    pub fn cancel_attach_terminal_pump(&mut self, client: ClientId, terminal: TerminalId) {
        self.terminal_table.cancel_pump(client, terminal);
    }

    /// Track a task from any of the three output subscription paths.
    pub(crate) fn track_terminal_output_pump(
        &mut self,
        client: ClientId,
        terminal: TerminalId,
        abort: tokio::task::AbortHandle,
        done: CancellationToken,
    ) {
        self.terminal_table
            .track_output_pump(client, terminal, abort, done);
    }

    /// Abort all of this subscription's tasks. Await each returned completion
    /// token outside the state borrow before acknowledging detach/replacement.
    pub(crate) fn stop_terminal_output_pumps(
        &mut self,
        client: ClientId,
        terminal: TerminalId,
    ) -> Vec<CancellationToken> {
        self.terminal_table.stop_output_pumps(client, terminal)
    }

    /// Remove `client` from `terminal`'s subscriber list (the
    /// `DETACH_TERMINAL` counterpart of the attach-time registration).
    pub fn unsubscribe_terminal(&mut self, client: ClientId, terminal: TerminalId) {
        self.terminal_table.unsubscribe(client, terminal);
    }
}
