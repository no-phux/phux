//! The per-resource engine table: every map keyed on a live resource, plus
//! the `JoinSet` owning the engine futures.
//!
//! Entries appear when a spawned engine is registered and go at reap. The
//! table is kind-agnostic (it never looks inside a facet) and owns only
//! bookkeeping: wire-id minting and metadata cleanup stay on
//! `ServerState`. Everything is `pub(super)` and sync.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use phux_protocol::ids::BootstrapId;
use tokio::task::{AbortHandle, JoinSet};
use tokio_util::sync::CancellationToken;

use super::ClientId;
use crate::resource::{ResourceHandle, ResourceId};

/// Last sequence a pump emitted before being superseded, so a replacement
/// resumes rather than replaying from zero.
pub(super) type LastValidAttachSequence = Arc<AtomicU64>;

/// A superseded pump generation: bootstrap id to tombstone, a token that
/// resolves when it exits, and its last sequence.
pub(super) type PriorAttachResourcePump = (BootstrapId, CancellationToken, LastValidAttachSequence);

/// The new generation's `(cancel, done, last_valid_seq)` and what it
/// displaced.
pub(super) type AttachResourcePumpReplacement = (
    CancellationToken,
    CancellationToken,
    LastValidAttachSequence,
    Option<PriorAttachResourcePump>,
);

/// One generation of an `ATTACH_RESOURCE` output pump. A re-attach replaces
/// its predecessor (clients re-bootstrap to change profile or recover): the
/// old `cancel` fires, `done` reports when it drained, and its bootstrap id
/// is tombstoned so late frames are dropped.
#[derive(Debug)]
pub(super) struct AttachResourceGeneration {
    /// Fires to stop this pump.
    pub(super) cancel: CancellationToken,
    /// Resolves once the pump task has actually exited.
    pub(super) done: CancellationToken,
    /// Highest sequence this generation emitted.
    pub(super) last_valid_seq: LastValidAttachSequence,
    /// Bootstrap id this generation streams under.
    pub(super) bootstrap_id: BootstrapId,
}

/// An output task's lifetime; dropping the owner aborts the task.
#[derive(Debug)]
struct OutputPumpTask {
    abort: Option<AbortHandle>,
    done: CancellationToken,
    drain: Option<CancellationToken>,
}

impl Drop for OutputPumpTask {
    fn drop(&mut self) {
        if let Some(abort) = &self.abort {
            abort.abort();
        }
    }
}

impl OutputPumpTask {
    /// Drop tracking without aborting: a fenced pump may still be sending
    /// the final screen after reap.
    fn release(mut self) {
        self.abort.take();
    }
}

/// Every resource-keyed table, plus client subscriptions and output pumps.
#[derive(Debug)]
pub(super) struct ResourceTable {
    /// Engine handles by core [`ResourceId`] (the engines run on the
    /// `LocalSet`, ADR-0014).
    handles: HashMap<ResourceId, ResourceHandle>,
    /// Engine cancellation tokens, usually children of the server root.
    /// Dropping one does not cancel.
    tokens: HashMap<ResourceId, CancellationToken>,
    /// The engine futures. Their `!Send` futures were `spawn_local`ed; this
    /// table is dropped on the `LocalSet` thread at the end of `run_async`,
    /// so `Drop` never polls them cross-thread.
    tasks: JoinSet<()>,
    /// Clients observing each resource; empty lists are removed.
    subscribers: HashMap<ResourceId, Vec<ClientId>>,
    /// Per-`(client, terminal)` `ATTACH_RESOURCE` pump generations,
    /// cancelled by detach, disconnect, or reap.
    pumps: HashMap<(ClientId, ResourceId), AttachResourceGeneration>,
    /// All raw-output tasks (aggregate replacements, `SPAWN_RESOURCE`
    /// pumps); staged and published may coexist, and detach retires both.
    output_pumps: HashMap<(ClientId, ResourceId), Vec<OutputPumpTask>>,
    /// Next per-client bootstrap id; monotonic, so tombstoned ids never
    /// recur.
    next_bootstrap: HashMap<ClientId, u64>,
    /// Retired wire ids remain detachable while final publication is pending.
    draining_terminals: HashMap<phux_protocol::ids::ResourceId, ResourceId>,
    /// ADR-0109: for spawned resources, the spawner and whether another
    /// connection has used it since. No entry means not client-spawned.
    spawns: HashMap<ResourceId, SpawnRecord>,
}

/// Who spawned a resource and whether another connection used it since
/// (ADR-0109, `docs/spec/L1.md` §5.2.1).
#[derive(Debug, Clone, Copy)]
struct SpawnRecord {
    /// The spawning connection (its own use never counts).
    spawner: ClientId,
    /// Set once any other connection subscribes to or uses it.
    used_by_other: bool,
}

impl Default for ResourceTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceTable {
    /// Build an empty table.
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            handles: HashMap::new(),
            tokens: HashMap::new(),
            tasks: JoinSet::new(),
            subscribers: HashMap::new(),
            pumps: HashMap::new(),
            output_pumps: HashMap::new(),
            next_bootstrap: HashMap::new(),
            draining_terminals: HashMap::new(),
            spawns: HashMap::new(),
        }
    }

    // -- actor handles ------------------------------------------------

    /// Look up the [`ResourceHandle`] for `terminal`, if registered.
    #[must_use]
    pub(super) fn handle(&self, terminal: ResourceId) -> Option<&ResourceHandle> {
        self.handles.get(&terminal)
    }

    /// Every registered resource id (no handle clones).
    #[must_use]
    pub(super) fn resource_ids(&self) -> Vec<ResourceId> {
        self.handles.keys().copied().collect()
    }

    /// Every `(pane, handle)`, cloned for use outside the lock.
    #[must_use]
    pub(super) fn all_handles(&self) -> Vec<(ResourceId, ResourceHandle)> {
        self.handles
            .iter()
            .map(|(tid, handle)| (*tid, handle.clone()))
            .collect()
    }

    /// Record an engine's `handle` and `token` (overwrites).
    pub(super) fn register(
        &mut self,
        terminal: ResourceId,
        handle: ResourceHandle,
        token: CancellationToken,
    ) {
        self.handles.insert(terminal, handle);
        self.tokens.insert(terminal, token);
    }

    /// Spawn an engine future on the `JoinSet` (inside a `LocalSet`).
    pub(super) fn spawn_actor<F>(&mut self, actor_future: F)
    where
        F: Future<Output = ()> + 'static,
    {
        self.tasks.spawn_local(actor_future);
    }

    /// Cancel and forget `terminal`'s engine token. Idempotent.
    pub(super) fn detach_actor(&mut self, terminal: ResourceId) {
        if let Some(token) = self.tokens.remove(&terminal) {
            token.cancel();
        }
    }

    /// `terminal`'s engine token while registered (a retained pane's exit
    /// watcher waits on it).
    pub(super) fn token(&self, terminal: ResourceId) -> Option<CancellationToken> {
        self.tokens.get(&terminal).cloned()
    }

    /// Register a bare engine token for `terminal`, with no handle.
    #[cfg(test)]
    pub(super) fn register_token_for_test(
        &mut self,
        terminal: ResourceId,
        token: CancellationToken,
    ) {
        self.tokens.insert(terminal, token);
    }

    // -- subscriptions ------------------------------------------------

    /// Current subscribers of `terminal` (empty if none).
    #[must_use]
    pub(super) fn subscribers_for(&self, terminal: ResourceId) -> &[ClientId] {
        self.subscribers.get(&terminal).map_or(&[], Vec::as_slice)
    }

    /// Subscribe `client` once (no double fanout). Every subscription path
    /// comes through here, so this is where a spawned resource learns a
    /// non-spawner attached it (ADR-0109); the mark survives a rollback.
    pub(super) fn subscribe(&mut self, client: ClientId, terminal: ResourceId) {
        self.note_use(client, terminal);
        let subs = self.subscribers.entry(terminal).or_default();
        if !subs.contains(&client) {
            subs.push(client);
        }
    }

    // -- spawn provenance (ADR-0109) -----------------------------------

    /// Record that `spawner` created `terminal`, before any subscription.
    pub(super) fn record_spawn(&mut self, terminal: ResourceId, spawner: ClientId) {
        self.spawns.insert(
            terminal,
            SpawnRecord {
                spawner,
                used_by_other: false,
            },
        );
    }

    /// Note that `client` used `terminal` (ADR-0109); only non-spawners
    /// count.
    pub(super) fn note_use(&mut self, client: ClientId, terminal: ResourceId) {
        if let Some(record) = self.spawns.get_mut(&terminal)
            && record.spawner != client
        {
            record.used_by_other = true;
        }
    }

    /// Whether `terminal` was client-spawned and only its spawner has used
    /// it.
    #[must_use]
    pub(super) fn unattached_since_spawn(&self, terminal: ResourceId) -> bool {
        self.spawns
            .get(&terminal)
            .is_some_and(|record| !record.used_by_other)
    }

    /// Remove `client` from `terminal`'s subscribers (dropping empty lists).
    pub(super) fn unsubscribe(&mut self, client: ClientId, terminal: ResourceId) {
        if let Some(subs) = self.subscribers.get_mut(&terminal) {
            subs.retain(|c| *c != client);
            if subs.is_empty() {
                self.subscribers.remove(&terminal);
            }
        }
    }

    /// Remove `client` from every subscriber list (dropping empty lists).
    pub(super) fn drop_client_subscriptions(&mut self, client: ClientId) {
        for subs in self.subscribers.values_mut() {
            subs.retain(|c| *c != client);
        }
        self.subscribers.retain(|_, subs| !subs.is_empty());
    }

    /// Handles of every resource `client` subscribes to.
    #[must_use]
    pub(super) fn subscribed_handles(&self, client: ClientId) -> Vec<ResourceHandle> {
        self.subscribers
            .iter()
            .filter(|(_, subs)| subs.contains(&client))
            .filter_map(|(terminal, _)| self.handle(*terminal).cloned())
            .collect()
    }

    /// Whether no pane has a subscriber (tests the empty-list GC).
    #[cfg(test)]
    #[must_use]
    pub(super) fn subscriber_map_is_empty(&self) -> bool {
        self.subscribers.is_empty()
    }

    // -- output task lifetimes and ATTACH_RESOURCE generations --------

    pub(super) fn track_output_pump(
        &mut self,
        client: ClientId,
        terminal: ResourceId,
        abort: AbortHandle,
        done: CancellationToken,
        drain: Option<CancellationToken>,
    ) {
        let tasks = self.output_pumps.entry((client, terminal)).or_default();
        tasks.retain(|task| !task.done.is_cancelled());
        tasks.push(OutputPumpTask {
            abort: Some(abort),
            done,
            drain,
        });
    }

    /// Ask terminal pumps for their final capture, retaining completion fences
    /// before the resource and subscription tables are removed.
    pub(super) fn begin_output_drain(
        &self,
        terminal: ResourceId,
    ) -> Vec<(ClientId, Vec<CancellationToken>)> {
        self.output_pumps
            .iter()
            .filter_map(|((client, pane), tasks)| {
                if *pane != terminal {
                    return None;
                }
                let done: Vec<_> = tasks
                    .iter()
                    .filter_map(|task| {
                        let drain = task.drain.as_ref()?;
                        if task.done.is_cancelled() {
                            return None;
                        }
                        drain.cancel();
                        Some(task.done.clone())
                    })
                    .collect();
                (!done.is_empty()).then_some((*client, done))
            })
            .collect()
    }

    pub(super) fn remember_draining_terminal(
        &mut self,
        wire: phux_protocol::ids::ResourceId,
        core: ResourceId,
    ) {
        self.draining_terminals.insert(wire, core);
    }

    pub(super) fn draining_terminal(
        &self,
        wire: &phux_protocol::ids::ResourceId,
    ) -> Option<ResourceId> {
        self.draining_terminals.get(wire).copied()
    }

    pub(super) fn finish_output_drain(&mut self, terminal: ResourceId) {
        self.cancel_pumps_for_terminal(terminal);
        self.draining_terminals.retain(|_, core| *core != terminal);
    }

    /// Let this pane's output pumps finish without aborting them.
    fn release_output_pumps(&mut self, terminal: ResourceId) {
        let keys: Vec<_> = self
            .output_pumps
            .keys()
            .filter(|(_, pane)| *pane == terminal)
            .copied()
            .collect();
        for key in keys {
            let mut draining = Vec::new();
            for task in self.output_pumps.remove(&key).unwrap_or_default() {
                if task
                    .drain
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled)
                    && !task.done.is_cancelled()
                {
                    draining.push(task);
                } else {
                    task.release();
                }
            }
            if !draining.is_empty() {
                self.output_pumps.insert(key, draining);
            }
        }
    }

    /// Abort every output task for this subscription, returning their exit
    /// fences.
    pub(super) fn stop_output_pumps(
        &mut self,
        client: ClientId,
        terminal: ResourceId,
    ) -> Vec<CancellationToken> {
        self.output_pumps
            .remove(&(client, terminal))
            .unwrap_or_default()
            .into_iter()
            .map(|task| task.done.clone())
            .collect()
    }

    /// Install a new `ATTACH_RESOURCE` pump generation, displacing any live
    /// one. The caller awaits the prior `done` before tombstoning its
    /// bootstrap id.
    pub(super) fn replace_pump(
        &mut self,
        client: ClientId,
        terminal: ResourceId,
        bootstrap_id: BootstrapId,
    ) -> AttachResourcePumpReplacement {
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        let last_valid_seq: LastValidAttachSequence = Arc::new(AtomicU64::new(0));
        let prior = self
            .pumps
            .insert(
                (client, terminal),
                AttachResourceGeneration {
                    cancel: cancel.clone(),
                    done: done.clone(),
                    last_valid_seq: Arc::clone(&last_valid_seq),
                    bootstrap_id,
                },
            )
            .map(|prior| {
                prior.cancel.cancel();
                (prior.bootstrap_id, prior.done, prior.last_valid_seq)
            });
        (cancel, done, last_valid_seq, prior)
    }

    /// Next per-terminal bootstrap id for `client`; `None` on overflow (the
    /// connection must reconnect).
    pub(super) fn next_bootstrap_id(&mut self, client: ClientId) -> Option<BootstrapId> {
        let next = self.next_bootstrap.entry(client).or_insert(1);
        let raw = *next;
        *next = raw.checked_add(1)?;
        BootstrapId::new(raw)
    }

    /// Cancel and forget the pump for `(client, terminal)`. Idempotent.
    pub(super) fn cancel_pump(&mut self, client: ClientId, terminal: ResourceId) {
        self.stop_output_pumps(client, terminal);
        if let Some(generation) = self.pumps.remove(&(client, terminal)) {
            generation.cancel.cancel();
        }
    }

    /// Cancel every client's pumps for `terminal`, so nothing it outputs
    /// reaches a subscriber after its close.
    pub(super) fn cancel_pumps_for_terminal(&mut self, terminal: ResourceId) {
        let clients: Vec<ClientId> = self
            .pumps
            .keys()
            .chain(self.output_pumps.keys())
            .filter(|(_, pane)| *pane == terminal)
            .map(|(client, _)| *client)
            .collect();
        for client in clients {
            self.cancel_pump(client, terminal);
        }
    }

    /// Cancel every pump `client` owns.
    pub(super) fn cancel_pumps_for_client(&mut self, client: ClientId) {
        self.output_pumps.retain(|(owner, _), _| *owner != client);
        self.pumps.retain(|(owner, _), generation| {
            if *owner == client {
                generation.cancel.cancel();
                false
            } else {
                true
            }
        });
        self.next_bootstrap.remove(&client);
    }

    // -- teardown -----------------------------------------------------

    /// Drop every entry keyed on a removed resource and return its actor
    /// token. Pumps are released, not cancelled, so a fenced pump can still
    /// publish the final screen; the caller cancels the actor afterwards.
    pub(super) fn forget_resource(
        &mut self,
        terminal: ResourceId,
    ) -> Option<tokio_util::sync::CancellationToken> {
        self.release_output_pumps(terminal);
        self.handles.remove(&terminal);
        let token = self.tokens.remove(&terminal);
        self.subscribers.remove(&terminal);
        self.spawns.remove(&terminal);
        // Release, don't cancel: see above.
        self.pumps.retain(|(_, pane), _| *pane != terminal);
        token
    }
}
