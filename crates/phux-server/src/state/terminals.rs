use std::future::Future;

use phux_core::ids::TerminalId;
use phux_protocol::ids::TerminalId as WireTerminalId;
use tokio_util::sync::CancellationToken;

use super::{ClientId, ServerState};
use crate::terminal_actor::TerminalHandle;

impl ServerState {
    /// Commit actor-applied dimensions only if no newer resize already won.
    pub fn record_applied_terminal_resize(
        &mut self,
        terminal: TerminalId,
        applied: crate::terminal_actor::ResizeApplied,
    ) -> bool {
        if !self
            .terminal_table
            .accept_resize_revision(terminal, applied.revision)
        {
            return false;
        }
        if let Some(pane) = self.registry_mut().terminal_mut(terminal) {
            pane.dims = (applied.cols, applied.rows);
            true
        } else {
            false
        }
    }

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
    pub fn subscribe_terminal(&mut self, client: ClientId, terminal: TerminalId) {
        self.terminal_table.subscribe(client, terminal);
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
    pub fn subscribed_terminal_consumer_handles(
        &self,
        client_id: ClientId,
    ) -> Vec<(TerminalHandle, u64)> {
        self.terminal_table.subscribed_consumer_handles(client_id)
    }

    /// Commit an actor registration and its emitter token only while the
    /// subscription that initiated the asynchronous registration is live.
    pub fn commit_terminal_consumer(
        &mut self,
        client: ClientId,
        terminal: TerminalId,
        generation: u64,
        sequence: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> Option<(
        CancellationToken,
        std::sync::Arc<std::sync::atomic::AtomicU64>,
    )> {
        let staged = self.stage_terminal_consumer(client, terminal, sequence)?;
        self.commit_staged_terminal_consumer(
            client,
            terminal,
            generation,
            staged.0.clone(),
            staged.1.clone(),
        )?;
        Some(staged)
    }

    /// Allocate a replacement emitter without disturbing the live one.
    pub fn stage_terminal_consumer(
        &self,
        client: ClientId,
        terminal: TerminalId,
        sequence: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> Option<(
        CancellationToken,
        std::sync::Arc<std::sync::atomic::AtomicU64>,
    )> {
        if !self
            .terminal_table
            .subscribers_for(terminal)
            .contains(&client)
        {
            return None;
        }
        Some((CancellationToken::new(), sequence))
    }

    /// Publish a staged registration after its authoritative snapshot exists.
    pub fn commit_staged_terminal_consumer(
        &mut self,
        client: ClientId,
        terminal: TerminalId,
        generation: u64,
        token: CancellationToken,
        sequence: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> Option<()> {
        if !self
            .terminal_table
            .subscribers_for(terminal)
            .contains(&client)
        {
            token.cancel();
            return None;
        }
        self.terminal_table
            .record_consumer_registration(client, terminal, generation);
        self.terminal_table
            .register_staged_pump(client, terminal, generation, token, sequence);
        Some(())
    }

    /// Roll back one failed attach without disturbing a newer replacement.
    pub fn rollback_terminal_consumer(
        &mut self,
        client: ClientId,
        terminal: TerminalId,
        generation: u64,
    ) -> bool {
        if !self
            .terminal_table
            .remove_consumer_registration_if(client, terminal, generation)
        {
            return false;
        }
        self.terminal_table
            .cancel_pump_if(client, terminal, generation);
        self.terminal_table.unsubscribe(client, terminal);
        true
    }

    /// Remove and return the actor registration for one pane subscription.
    pub fn take_consumer_registration(
        &mut self,
        client: ClientId,
        terminal: TerminalId,
    ) -> Option<u64> {
        self.terminal_table
            .take_consumer_registration(client, terminal)
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

    /// Shared sequence counter from the current emitter, or a fresh counter.
    #[must_use]
    pub fn attach_terminal_pump_sequence(
        &self,
        client: ClientId,
        terminal: TerminalId,
    ) -> std::sync::Arc<std::sync::atomic::AtomicU64> {
        self.terminal_table.pump_sequence(client, terminal)
    }

    /// Cancel and forget the `ATTACH_TERMINAL` pump for `(client,
    /// terminal)`, if one is live. Idempotent.
    pub fn cancel_attach_terminal_pump(
        &mut self,
        client: ClientId,
        terminal: TerminalId,
    ) -> Option<u64> {
        self.terminal_table.cancel_pump(client, terminal)
    }

    /// Remove `client` from `terminal`'s subscriber list (the
    /// `DETACH_TERMINAL` counterpart of the attach-time registration).
    pub fn unsubscribe_terminal(&mut self, client: ClientId, terminal: TerminalId) {
        self.terminal_table.unsubscribe(client, terminal);
    }
}
