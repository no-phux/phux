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

/// How far apart two ids [`ResourceTable::next_bootstrap_id`] hands out are.
/// A Terminal pump counts its own resyncs up from its attach's id without
/// asking here, and the first block belongs to session ATTACH and SPAWN, whose
/// generations all start at 1. Adjacent ids handed a re-attach one a client
/// had already retired, and the client drew nothing.
const BOOTSTRAP_ID_BLOCK: u64 = 1 << 32;

/// Last sequence a pump emitted before being superseded, so a replacement
/// resumes rather than replaying from zero.
pub(super) type LastValidAttachSequence = Arc<AtomicU64>;

/// A superseded generation's tombstone identity and final sequence.
pub(super) type PriorAttachResourcePump = (BootstrapId, LastValidAttachSequence);

/// The admitted generation's lifetime, cursor, and predecessor metadata.
pub(super) type AttachResourcePumpReplacement = (
    CancellationToken,
    CancellationToken,
    LastValidAttachSequence,
    Option<PriorAttachResourcePump>,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamDisposition {
    Active,
    Draining,
}

/// One generation of an `ATTACH_RESOURCE` output pump. A re-attach replaces
/// its predecessor (clients re-bootstrap to change profile or recover): the
/// old `cancel` fires and actual emitter tasks retain the completion fences.
/// Its bootstrap id is tombstoned so late frames are dropped.
#[derive(Debug)]
pub(super) struct AttachResourceGeneration {
    /// Fires to stop this pump.
    pub(super) cancel: CancellationToken,
    /// The publication owner hands this close fence to its actual emitter,
    /// or resolves it at tick publication/drop. Replacements use task fences.
    pub(super) source_done: CancellationToken,
    disposition: StreamDisposition,
    /// Highest sequence this generation emitted.
    pub(super) last_valid_seq: LastValidAttachSequence,
    /// Bootstrap id this generation streams under.
    pub(super) bootstrap_id: BootstrapId,
    /// Cancels a still-unpublished aggregate if any member is displaced.
    publication: Option<CancellationToken>,
}

/// An output task's lifetime; dropping the owner aborts the task.
#[derive(Debug)]
struct OutputPumpTask {
    abort: Option<AbortHandle>,
    done: CancellationToken,
    drain: Option<CancellationToken>,
    generation: LastValidAttachSequence,
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
#[derive(Debug, Default)]
pub(crate) struct ResourceTable {
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
    /// Next per-client bootstrap id block; monotonic, so tombstoned ids
    /// never recur.
    next_bootstrap: HashMap<ClientId, u64>,
    /// Retired wire ids remain detachable while final publication is pending.
    draining_resources: HashMap<phux_protocol::ids::ResourceId, ResourceId>,
    /// ADR-0109: for spawned resources, the spawner and whether another
    /// connection has used it since. No entry means not client-spawned.
    spawns: HashMap<ResourceId, SpawnRecord>,
}

/// Who spawned a resource and whether another connection used it since
/// (ADR-0109, `docs/protocol.md#receipts-retries-and-backpressure`).
#[derive(Debug, Clone, Copy)]
struct SpawnRecord {
    /// The spawning connection (its own use never counts).
    spawner: ClientId,
    /// Set once any other connection subscribes to or uses it.
    used_by_other: bool,
}

impl ResourceTable {
    // -- actor handles ------------------------------------------------

    /// Look up the [`ResourceHandle`] for `terminal`, if registered.
    #[must_use]
    pub(crate) fn handle(&self, terminal: ResourceId) -> Option<&ResourceHandle> {
        self.handles.get(&terminal)
    }

    /// Every registered resource id (no handle clones).
    #[must_use]
    pub(crate) fn resource_ids(&self) -> Vec<ResourceId> {
        self.handles.keys().copied().collect()
    }

    /// Every `(pane, handle)`, cloned for use outside the lock.
    #[must_use]
    pub(crate) fn all_handles(&self) -> Vec<(ResourceId, ResourceHandle)> {
        self.handles
            .iter()
            .map(|(tid, handle)| (*tid, handle.clone()))
            .collect()
    }

    /// Record an engine's `handle` and `token` (overwrites).
    pub(in crate::state) fn register(
        &mut self,
        terminal: ResourceId,
        handle: ResourceHandle,
        token: CancellationToken,
    ) {
        self.handles.insert(terminal, handle);
        self.tokens.insert(terminal, token);
    }

    /// Spawn an engine future on the `JoinSet` (inside a `LocalSet`).
    pub(in crate::state) fn spawn_actor<F>(&mut self, actor_future: F)
    where
        F: Future<Output = ()> + 'static,
    {
        // Reap finished engines so the set does not grow for the server's life.
        while self.tasks.try_join_next().is_some() {}
        self.tasks.spawn_local(actor_future);
    }

    /// Cancel and forget `terminal`'s engine token. Idempotent.
    pub(crate) fn detach_actor(&mut self, terminal: ResourceId) {
        if let Some(token) = self.tokens.remove(&terminal) {
            token.cancel();
        }
    }

    /// Shutdown: cancel every registered engine and take all engine futures,
    /// including killed ones still inside their hangup grace, so the caller
    /// can let each finish its teardown before the `LocalSet` drops.
    pub(crate) fn retire_engines(&mut self) -> JoinSet<()> {
        for token in self.tokens.values() {
            token.cancel();
        }
        std::mem::take(&mut self.tasks)
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
    pub(crate) fn subscribers_for(&self, terminal: ResourceId) -> &[ClientId] {
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
    pub(crate) fn record_spawn(&mut self, terminal: ResourceId, spawner: ClientId) {
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

    /// The connection that spawned `terminal`, when a client did.
    #[must_use]
    pub(crate) fn spawner(&self, terminal: ResourceId) -> Option<ClientId> {
        self.spawns.get(&terminal).map(|record| record.spawner)
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
    pub(crate) fn unsubscribe(&mut self, client: ClientId, terminal: ResourceId) {
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

    /// Handles of every resource `client` subscribes to, gathered under the
    /// lock so detach requests can be sent off it.
    #[must_use]
    pub(crate) fn subscribed_handles(&self, client: ClientId) -> Vec<ResourceHandle> {
        self.subscribers
            .iter()
            .filter(|(_, subs)| subs.contains(&client))
            .filter_map(|(terminal, _)| self.handle(*terminal).cloned())
            .collect()
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
        // A natural close retains exact publication authority through its
        // source and final emitter fences. Synchronous reap has no such drain.
        self.pumps.retain(|(_, pane), generation| {
            *pane != terminal || generation.disposition == StreamDisposition::Draining
        });
        token
    }
}

mod pumps;
