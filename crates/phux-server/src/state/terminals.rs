use std::future::Future;

use phux_core::ids::ResourceId;
use phux_protocol::ids::{BootstrapId, ResourceId as WireResourceId};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::resource_table::AttachResourcePumpReplacement;
use super::{ClientId, ServerState};
use crate::mailbox::Outbound;
use crate::resource::ResourceHandle;

impl ServerState {
    /// Subscribers (snapshot) for `pane`. Returns an empty slice if no
    /// clients are currently observing the pane.
    #[must_use]
    pub fn subscribers_for_terminal(&self, terminal: ResourceId) -> &[ClientId] {
        self.resources.subscribers_for(terminal)
    }

    /// Subscribe `client` to `terminal`'s output fanout, deduplicating so a
    /// re-attach cannot double-register.
    ///
    /// The `ATTACH_RESOURCE` / `SPAWN_RESOURCE` counterpart of
    /// [`Self::unsubscribe_terminal`]; whole-session subscription happens
    /// inside [`Self::attach`].
    ///
    /// `mailbox` is the subscriber's outbound channel. Pass `Some` from any
    /// path that can subscribe a client with no session-scoped `ATTACH`
    /// behind it (`ATTACH_RESOURCE`), so terminal-scoped fanout
    /// ([`Self::terminal_fanout_targets`]) can reach it; `None` is correct
    /// only where the caller has just verified the client is in
    /// [`Self::attached`], which already carries the same sender.
    pub fn subscribe_terminal(
        &mut self,
        client: ClientId,
        terminal: ResourceId,
        mailbox: Option<mpsc::Sender<Outbound>>,
    ) {
        if let Some(tx) = mailbox {
            self.clients.remember_terminal_mailbox(client, tx);
        }
        self.resources.subscribe(client, terminal);
    }

    /// One client's outbound mailbox, resolved the same way
    /// [`Self::terminal_fanout_targets`] resolves each of its subscribers:
    /// the session-attached mailbox first, then the `ATTACH_RESOURCE`-only
    /// one.
    ///
    /// For a caller that already knows which one client to reach — an
    /// uncorrelated `ERROR` reply to a fire-and-forget input frame that
    /// named the wrong resource kind (`docs/spec/input.md` §9) — rather
    /// than every subscriber of a pane.
    #[must_use]
    pub fn client_mailbox(&self, client: ClientId) -> Option<mpsc::Sender<Outbound>> {
        self.clients.terminal_fanout_mailbox(client).cloned()
    }

    /// Outbound mailboxes of every client subscribed to `terminal`, for the
    /// server's out-of-band terminal-scoped fanout (`RESOURCE_CLOSED`).
    ///
    /// Each subscriber is resolved to exactly one mailbox —
    /// [`Self::attached`] first, then the `ATTACH_RESOURCE` subscription
    /// mailbox — so a session-attached client receives one frame and an
    /// `ATTACH_RESOURCE`-only consumer receives one frame, per L1 §3.1's
    /// "every client subscribed to the Terminal". Resolving through
    /// [`Self::attached`] alone silently dropped the second kind
    /// (phux-w7z2.56).
    ///
    /// A subscriber whose mailbox has already gone (connection tearing
    /// down) is skipped; the fanout is best-effort by construction.
    #[must_use]
    pub fn terminal_fanout_targets(&self, terminal: ResourceId) -> Vec<mpsc::Sender<Outbound>> {
        self.subscribers_for_terminal(terminal)
            .iter()
            .filter_map(|client| self.clients.terminal_fanout_mailbox(*client).cloned())
            .collect()
    }

    /// Clone the [`ResourceHandle`] of every pane `client_id` currently
    /// subscribes to (phux-0q8). The runtime uses this at DETACH /
    /// disconnect / EOF time to send a
    /// [`ConsumerDetachRequest`](crate::terminal_actor::ConsumerDetachRequest) to each
    /// pane actor so the per-consumer `RenderState` cache (ADR-0018) is
    /// freed, mirroring the `register_consumer` calls the ATTACH path
    /// made. Gathered under-lock; the sends happen off-lock in the
    /// runtime to avoid awaiting inside `with_mut`.
    #[must_use]
    pub fn subscribed_resource_handles(&self, client_id: ClientId) -> Vec<ResourceHandle> {
        self.resources.subscribed_handles(client_id)
    }

    /// Record a freshly-spawned engine's [`ResourceHandle`] against
    /// `terminal` and allocate its wire id.
    ///
    /// Called by the runtime after `TerminalActor::new` /
    /// `build_with_token`. Subsequent attaches use
    /// [`Self::resource_handle`] to look the handle up; a Terminal-only
    /// request then goes through
    /// [`ResourceHandle::terminal`](crate::resource::ResourceHandle::terminal).
    ///
    /// `token` is stashed alongside the handle; cancelling it (e.g. via
    /// [`Self::detach_resource_actor`]) fires the actor's shutdown branch.
    ///
    /// This method does NOT spawn the actor — pair it with
    /// [`Self::spawn_resource_actor`] when you also want the actor task
    /// registered against the per-server `JoinSet`.
    ///
    /// Idempotent on the wire-id allocation (a second call for the
    /// same `pane` returns the same wire id) but overwrites the
    /// `ResourceHandle` / token. In practice the runtime calls this
    /// exactly once per pane lifetime.
    ///
    /// Stays on `ServerState` rather than moving onto the terminal table:
    /// the wire-id mint and the two table writes are one atomic step the
    /// runtime depends on, and the id space is a different concern.
    pub fn register_resource_handle(
        &mut self,
        terminal: ResourceId,
        handle: ResourceHandle,
        token: CancellationToken,
    ) -> WireResourceId {
        let wire = self.intern_terminal_wire(terminal);
        self.resources.register(terminal, handle, token);
        wire
    }

    /// One-shot helper: register `handle`/`token` AND spawn
    /// `actor_future` onto the per-server pane `JoinSet`. Must be
    /// called from inside a `LocalSet` (per ADR-0014; pane actors
    /// own `!Send` `Terminal`s and are spawned via
    /// `JoinSet::spawn_local`).
    ///
    /// Returns the wire pane id, matching [`Self::register_resource_handle`].
    pub fn spawn_resource_actor<F>(
        &mut self,
        terminal: ResourceId,
        handle: ResourceHandle,
        token: CancellationToken,
        actor_future: F,
    ) -> WireResourceId
    where
        F: Future<Output = ()> + 'static,
    {
        let wire = self.register_resource_handle(terminal, handle, token);
        self.resources.spawn_actor(actor_future);
        wire
    }

    /// Cancel `pane`'s actor token, signalling the `TerminalActor` to
    /// exit, and forget the token. Idempotent. Used by future
    /// pane-close lifecycle paths; not exercised by `phux-byc.8`.
    ///
    /// The actor task itself is drained from the per-server `JoinSet`
    /// when it returns from `run`; we don't need to touch the pane-task
    /// set here.
    pub fn detach_resource_actor(&mut self, terminal: ResourceId) {
        self.resources.detach_actor(terminal);
    }

    /// Look up the [`ResourceHandle`] for `terminal`, if registered. The
    /// handle is kind-agnostic; see
    /// [`ResourceHandle::terminal`](crate::resource::ResourceHandle::terminal)
    /// for the Terminal facet.
    #[must_use]
    pub fn resource_handle(&self, terminal: ResourceId) -> Option<&ResourceHandle> {
        self.resources.handle(terminal)
    }

    /// Clone every registered `(pane, handle)` pair so the caller can talk
    /// to the actors outside the `ServerState` lock (the `Arc<Mutex<_>>`
    /// must not be held across an await).
    pub(crate) fn all_resource_handles(&self) -> Vec<(ResourceId, ResourceHandle)> {
        self.resources.all_handles()
    }

    /// Every registered resource id, whatever its kind. Cheap relative to
    /// `Self::all_resource_handles`: no handle clones.
    #[must_use]
    pub fn resource_ids(&self) -> Vec<ResourceId> {
        self.resources.resource_ids()
    }

    /// Install a new `ATTACH_RESOURCE` pump generation for `(client,
    /// terminal)`, displacing any live one.
    ///
    /// See `ResourceTable::replace_pump` for what the returned tuple carries
    /// and why a second attach replaces rather than being refused. (Not a
    /// rustdoc link: that method is `pub(super)`, and rustdoc does not document
    /// private items.)
    pub fn replace_attach_terminal_pump(
        &mut self,
        client: ClientId,
        terminal: ResourceId,
        bootstrap_id: BootstrapId,
    ) -> AttachResourcePumpReplacement {
        self.resources.replace_pump(client, terminal, bootstrap_id)
    }

    /// Allocate the next per-terminal bootstrap id for `client`, or `None`
    /// once the connection has exhausted its id space.
    pub fn next_attach_terminal_bootstrap_id(&mut self, client: ClientId) -> Option<BootstrapId> {
        self.resources.next_bootstrap_id(client)
    }

    /// Cancel and forget the `ATTACH_RESOURCE` pump for `(client,
    /// terminal)`, if one is live. Idempotent.
    pub fn cancel_attach_terminal_pump(&mut self, client: ClientId, terminal: ResourceId) {
        self.resources.cancel_pump(client, terminal);
    }

    /// Track a task from any of the three output subscription paths.
    pub(crate) fn track_terminal_output_pump(
        &mut self,
        client: ClientId,
        terminal: ResourceId,
        abort: tokio::task::AbortHandle,
        done: CancellationToken,
    ) {
        self.resources
            .track_output_pump(client, terminal, abort, done);
    }

    /// Abort all of this subscription's tasks. Await each returned completion
    /// token outside the state borrow before acknowledging detach/replacement.
    pub(crate) fn stop_terminal_output_pumps(
        &mut self,
        client: ClientId,
        terminal: ResourceId,
    ) -> Vec<CancellationToken> {
        self.resources.stop_output_pumps(client, terminal)
    }

    /// Remove `client` from `terminal`'s subscriber list (the
    /// `DETACH_RESOURCE` counterpart of the attach-time registration).
    pub fn unsubscribe_terminal(&mut self, client: ClientId, terminal: ResourceId) {
        self.resources.unsubscribe(client, terminal);
    }

    /// Record that `client`'s `SPAWN_RESOURCE` created `terminal`
    /// (ADR-0109). Call it before anything subscribes to the new resource:
    /// from then on a subscription by any other connection marks the
    /// resource as attached, which `KILL_RESOURCE_IF` checks.
    pub fn record_spawn(&mut self, terminal: ResourceId, client: ClientId) {
        self.resources.record_spawn(terminal, client);
    }
}
