use std::collections::{HashMap, HashSet};

use phux_protocol::ids::{GroupId, TerminalId as WireTerminalId};
use phux_protocol::wire::frame::Scope;
use tokio::sync::mpsc;

use super::ServerState;
use super::client::ClientId;
use crate::mailbox::Outbound;

/// Most metadata subscriptions one connection may hold at once (phux-w7z2.59).
///
/// `SUBSCRIBE_METADATA` has no reply frame (SPEC L3.md §1.2) and no
/// `UNSUBSCRIBE_METADATA` verb, so an unbounded remote caller can grow this
/// set for the life of the connection — on a `phux service install` server
/// that is now weeks, not a session. The cap is sized well above any
/// realistic caller, adopting the "generous multiple of shipped usage"
/// convention `agent_detect::rules` set for its own bounds
/// (phux-w7z2.14): the reference TUI subscribes a handful of Global/Group
/// keys (`phux.session.name/v1`, `phux.tui.layout/v1`,
/// `phux.tui.window_order/v1`, `phux.tui.focus/v1`) plus up to three
/// per-Terminal keys (`phux.agent/v1`, `phux.tags/v1`, `phux.link/v1`) per
/// pane it has ever attached to, so even a heavy fleet of ~150 panes stays
/// under 500 subscriptions. 512 gives headroom over that while keeping the
/// worst case trivial: each subscription is one `(ClientId, Scope, String)`
/// tuple, so a maxed-out connection costs on the order of tens of
/// kilobytes, not a resource an attacker gains anything by exhausting.
const MAX_SUBSCRIPTIONS_PER_CLIENT: usize = 512;

/// Per-scope K/V store for L3 metadata (SPEC §7.4 / §11.L3) plus the
/// matching subscription registry.
///
/// Held inside [`super::ServerState`] but lifted into its own type so the
/// subscribe / set / delete / list operations live in a focused
/// surface — easier to test, easier to reason about ordering invariants,
/// and a natural home for the per-key size cap once that lands.
#[derive(Debug, Default)]
pub struct MetadataStore {
    /// Per-Terminal key → value. Cleared when the Terminal closes (the
    /// L1 lifecycle that owns the Terminal).
    terminal: HashMap<WireTerminalId, HashMap<String, Vec<u8>>>,
    /// Per-Group key → value.
    group: HashMap<GroupId, HashMap<String, Vec<u8>>>,
    /// Global key → value.
    global: HashMap<String, Vec<u8>>,
    /// Active subscriptions: a flat set of `(client, scope, key)` tuples.
    /// Lookup on broadcast is linear in the number of subscriptions; that
    /// is acceptable while subscriptions are sparse (handful per client).
    /// A future ticket may switch this to a `HashMap<(scope, key), Vec<ClientId>>`
    /// if the dispatch path shows up in flame graphs.
    subscriptions: HashSet<(ClientId, Scope, String)>,
}

/// Outcome of a `SET_METADATA` call.
///
/// `Unchanged` means the key already held an identical value, so the
/// server SHOULD suppress the `METADATA_CHANGED` broadcast (it's a noop
/// from every subscriber's perspective).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataSetOutcome {
    /// Key did not exist or held a different value; value was written.
    Changed,
    /// Key already held the identical value; no broadcast needed.
    Unchanged,
}

/// Outcome of [`super::ServerState::rename_session`], mapping the three terminal
/// cases of a `RENAME_SESSION` to the wire replies the server issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameOutcome {
    /// The session was renamed (or already bore the requested name); reply
    /// `COMMAND_RESULT { Ok }`.
    Renamed,
    /// No session matched the current name; reply `SESSION_NOT_FOUND`.
    NotFound,
    /// Another live session already holds the requested name; reply
    /// `INVALID_COMMAND` (the code `CREATE_SESSION` uses for a taken name).
    NameTaken,
}

impl MetadataStore {
    /// Get the value at `(scope, key)`, if any.
    #[must_use]
    pub fn get(&self, scope: &Scope, key: &str) -> Option<Vec<u8>> {
        match scope {
            Scope::Terminal(tid) => self.terminal.get(tid).and_then(|m| m.get(key)).cloned(),
            Scope::Group(gid) => self.group.get(gid).and_then(|m| m.get(key)).cloned(),
            Scope::Global => self.global.get(key).cloned(),
            // `Scope` is `#[non_exhaustive]`: a forward-compat variant we
            // don't know about returns None. The cleanest default for an
            // unknown scope is "no value present" — the caller's contract
            // is preserved without trapping on unknown bytes.
            _ => None,
        }
    }

    /// Set the value at `(scope, key)`. Returns whether the value
    /// actually changed (so the caller can suppress an unnecessary
    /// broadcast).
    pub fn set(&mut self, scope: &Scope, key: &str, value: Vec<u8>) -> MetadataSetOutcome {
        let bucket: &mut HashMap<String, Vec<u8>> = match scope {
            Scope::Terminal(tid) => self.terminal.entry(tid.clone()).or_default(),
            Scope::Group(gid) => self.group.entry(*gid).or_default(),
            Scope::Global => &mut self.global,
            // Unknown forward-compat variant: silently drop the write.
            // SPEC §6 lets newer encoders ship trailing field shapes;
            // here the surface area is "unknown scope, no bucket".
            _ => return MetadataSetOutcome::Unchanged,
        };
        if let Some(prev) = bucket.get(key)
            && prev == &value
        {
            return MetadataSetOutcome::Unchanged;
        }
        bucket.insert(key.to_owned(), value);
        MetadataSetOutcome::Changed
    }

    /// Delete `(scope, key)`. Returns whether the key existed (so the
    /// caller can suppress the broadcast on a true noop).
    pub fn delete(&mut self, scope: &Scope, key: &str) -> bool {
        match scope {
            Scope::Terminal(tid) => self
                .terminal
                .get_mut(tid)
                .and_then(|m| m.remove(key))
                .is_some(),
            Scope::Group(gid) => self
                .group
                .get_mut(gid)
                .and_then(|m| m.remove(key))
                .is_some(),
            Scope::Global => self.global.remove(key).is_some(),
            // Unknown forward-compat variant: nothing to delete.
            _ => false,
        }
    }

    /// List every key in `scope` (no values, sorted for determinism).
    #[must_use]
    pub fn list(&self, scope: &Scope) -> Vec<String> {
        let mut keys: Vec<String> = match scope {
            Scope::Terminal(tid) => self
                .terminal
                .get(tid)
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default(),
            Scope::Group(gid) => self
                .group
                .get(gid)
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default(),
            Scope::Global => self.global.keys().cloned().collect(),
            // Unknown forward-compat variant: empty listing.
            _ => Vec::new(),
        };
        keys.sort();
        keys
    }

    /// Drop every key scoped to `terminal`, AND every subscription that
    /// names it. Called when the Terminal closes (the L1 lifecycle that
    /// owns the per-Terminal scope — see the `terminal` field doc).
    ///
    /// A subscription's *connection* dying is handled separately —
    /// [`Self::drop_client`] runs on detach and clears every subscription
    /// that connection holds, whatever scope it names. This method covers
    /// the other half (phux-w7z2.59): the connection stays alive but the
    /// Terminal it subscribed to does not. Without this, a long-lived
    /// watcher that has ever subscribed to a pane's `phux.agent/v1` (or any
    /// other per-Terminal key) accumulates one dead subscription per closed
    /// pane for as long as the connection is open — on a `phux service
    /// install` server, unboundedly.
    pub fn forget_terminal(&mut self, terminal: &WireTerminalId) {
        self.terminal.remove(terminal);
        self.subscriptions
            .retain(|(_, scope, _)| !matches!(scope, Scope::Terminal(tid) if tid == terminal));
    }

    /// Register `(client, scope, key)` as an active subscription, subject
    /// to `MAX_SUBSCRIPTIONS_PER_CLIENT`. Returns `true` if the
    /// subscription is now active (either newly inserted or already
    /// present — re-subscribing the same triple is idempotent and never
    /// counts against the cap twice), `false` if `client` is already at
    /// the cap and this would have been a new entry.
    ///
    /// The caller (`handle_subscribe_metadata`) is expected to drop a
    /// refusal silently but for a log line: `SUBSCRIBE_METADATA` has no
    /// reply frame to carry an error on (SPEC L3.md §1.2), matching the
    /// existing non-L3-consumer refusal one arm up the same dispatch.
    pub fn subscribe(&mut self, client: ClientId, scope: Scope, key: String) -> bool {
        let triple = (client, scope, key);
        if self.subscriptions.contains(&triple) {
            return true;
        }
        // SUBSCRIBE_METADATA is a rare, connection-setup-time event, not a
        // hot path — this scan (bounded by the cap on any one client, but
        // linear in the server's *total* subscription count across every
        // client) is the same trade the module doc already makes for
        // `subscribers_for`'s dispatch-path scan, and phux is a one-user-
        // per-server process (ADR-0003) with a correspondingly small
        // connection count.
        let held_by_client = self
            .subscriptions
            .iter()
            .filter(|(c, _, _)| *c == client)
            .count();
        if held_by_client >= MAX_SUBSCRIPTIONS_PER_CLIENT {
            return false;
        }
        self.subscriptions.insert(triple);
        true
    }

    /// Drop every subscription owned by `client`. Called on detach.
    pub fn drop_client(&mut self, client: ClientId) {
        self.subscriptions.retain(|(c, _, _)| *c != client);
    }

    /// Collect every client subscribed to `(scope, key)`. Order is
    /// unspecified — callers MUST NOT rely on subscriber iteration order.
    #[must_use]
    pub fn subscribers_for(&self, scope: &Scope, key: &str) -> Vec<ClientId> {
        self.subscriptions
            .iter()
            .filter(|(_, s, k)| s == scope && k == key)
            .map(|(c, _, _)| *c)
            .collect()
    }
}

impl ServerState {
    /// Borrow the L3 metadata store.
    #[must_use]
    pub const fn metadata(&self) -> &MetadataStore {
        &self.metadata
    }

    /// Atomic SET + broadcast: store `value` at `(scope, key)`, then
    /// enqueue a `MetadataChanged` to every L3-capable subscriber
    /// whose subscription matches `(scope, key)`. Silently skips
    /// subscribers that have been detached or whose mailbox is full
    /// (`try_send` semantics — backpressure is a flow-control concern
    /// SPEC §12 doesn't yet cover for L3).
    ///
    /// Returns the set of clients the broadcast was attempted against
    /// (after L3-capability filtering) so callers can assert fanout
    /// shape in tests.
    pub fn metadata_set(&mut self, scope: &Scope, key: &str, value: Vec<u8>) -> Vec<ClientId> {
        // Broadcast first so the borrow of `value` is finished by the time
        // the K/V store consumes it on `set`. The "set before broadcast"
        // ordering is preserved by checking the prior value: if the new
        // bytes equal what's already stored we return early *before*
        // mutating, so subscribers never observe a fake notification.
        let unchanged = self
            .metadata
            .get(scope, key)
            .is_some_and(|prev| prev == value);
        if unchanged {
            return Vec::new();
        }
        let delivered = self.metadata_broadcast(scope, key, &value);
        // Commit the write last; `MetadataSetOutcome` is now redundant
        // here but kept on the lower-level API for direct callers.
        let _ = self.metadata.set(scope, key, value);
        delivered
    }

    /// Atomic DELETE + tombstone broadcast. Idempotent: deleting a
    /// missing key returns an empty broadcast set.
    pub fn metadata_delete(&mut self, scope: &Scope, key: &str) -> Vec<ClientId> {
        let existed = self.metadata.delete(scope, key);
        if !existed {
            return Vec::new();
        }
        self.broadcast_metadata_change(scope, key, None)
    }

    /// Broadcast-only counterpart of [`Self::metadata_set`]: enqueue a
    /// `MetadataChanged` carrying `value` to every L3-capable subscriber of
    /// `(scope, key)` WITHOUT touching the store.
    ///
    /// Exists for the server-intercepted conventional keys whose written
    /// value is a *command* the server applies, not state it retains — the
    /// session rename (`phux.session.name/v1`, value `current\0new`) being
    /// the first caller. Routing such a payload through
    /// [`Self::metadata_set`] would (a) leak a stale transition blob into
    /// `GET_METADATA` / `LIST_METADATA`, and (b) let the equal-bytes dedup
    /// swallow a legitimate repeat of the same rename pair (rename `A -> B`,
    /// `B` dies, a new `A` appears, rename `A -> B` again). The caller is
    /// responsible for broadcasting only when the underlying mutation
    /// actually happened.
    ///
    /// Returns the set of clients the broadcast was attempted against, as
    /// [`Self::metadata_set`] does, so callers can assert fanout shape.
    #[must_use]
    pub fn metadata_broadcast(&self, scope: &Scope, key: &str, value: &[u8]) -> Vec<ClientId> {
        self.broadcast_metadata_change(scope, key, Some(value))
    }

    /// The one fanout: resolve the subscribers of `(scope, key)` and enqueue
    /// `MetadataChanged` to each. `None` is the delete tombstone.
    ///
    /// Every caller above goes through here — SET, DELETE, and the
    /// broadcast-only path — so "who hears about a change" has a single
    /// definition and the returned set means the same thing for all three.
    fn broadcast_metadata_change(
        &self,
        scope: &Scope,
        key: &str,
        value: Option<&[u8]>,
    ) -> Vec<ClientId> {
        let subscribers = self.metadata.subscribers_for(scope, key);
        self.clients
            .broadcast_metadata_changed(&subscribers, scope, key, value)
    }

    /// Publish ownership of a one-shot session-create result.
    ///
    /// The metadata value must already have been written. At most 256 unread
    /// results are retained per connection; the oldest is evicted first.
    pub fn track_session_create_result(&mut self, client_id: ClientId, key: String) {
        const MAX_PENDING_PER_CLIENT: usize = 256;
        // The block deliberately scopes the map borrow so the following
        // `self.metadata_delete` can take `&mut self`.
        let evicted = {
            let keys = self
                .clients
                .session_create_results
                .entry(client_id)
                .or_default();
            let evicted = (keys.len() >= MAX_PENDING_PER_CLIENT)
                .then(|| keys.pop_front())
                .flatten();
            keys.push_back(key);
            evicted
        };
        if let Some(key) = evicted {
            let _ = self.metadata_delete(&phux_protocol::wire::frame::Scope::Global, &key);
        }
    }

    /// Whether any live connection owns an unread result at `key`.
    #[must_use]
    pub fn session_create_result_is_pending(&self, key: &str) -> bool {
        self.clients.session_create_result_is_pending(key)
    }

    /// Whether `client_id` owns the unread nonce-bearing result at `key`.
    #[must_use]
    pub fn owns_session_create_result(&self, client_id: ClientId, key: &str) -> bool {
        self.clients.owns_session_create_result(client_id, key)
    }

    /// Consume a one-shot session-create result and forget its owner.
    pub fn consume_session_create_result(&mut self, key: &str) {
        let _ = self.metadata_delete(&phux_protocol::wire::frame::Scope::Global, key);
        for keys in self.clients.session_create_results.values_mut() {
            keys.retain(|candidate| candidate != key);
        }
        self.clients
            .session_create_results
            .retain(|_, keys| !keys.is_empty());
    }

    /// Register a subscription for `client_id`. The client MUST be
    /// L3-capable (call sites in the runtime gate on
    /// [`Self::client_speaks_l3`] before invoking this).
    ///
    /// `tx` is the client's outbound mailbox, captured here for the same
    /// reason [`Self::subscribe_events`] captures one: a headless consumer
    /// subscribes WITHOUT attaching, so `METADATA_CHANGED` fanout cannot be
    /// resolved through `attached` alone. Both maps are cleared together on
    /// detach.
    ///
    /// Returns `false` when `client_id` is already at
    /// [`MetadataStore`]'s per-connection subscription cap and this would
    /// have added a new entry — see [`MetadataStore::subscribe`]. The
    /// mailbox is remembered only when the subscription is actually
    /// accepted, so a client that is refused every subscription from a
    /// fresh connection never gains an entry in the mailbox map either.
    pub fn metadata_subscribe(
        &mut self,
        client_id: ClientId,
        scope: Scope,
        key: String,
        tx: mpsc::Sender<Outbound>,
    ) -> bool {
        let accepted = self.metadata.subscribe(client_id, scope, key);
        if accepted {
            self.clients.remember_metadata_mailbox(client_id, tx);
        }
        accepted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: usize) -> String {
        format!("phux.test.key/{n}/v1")
    }

    /// Filling a client to the cap succeeds on every distinct key; the
    /// `MAX_SUBSCRIPTIONS_PER_CLIENT + 1`th distinct key is refused, and the
    /// refusal does not mutate the store — the count stays pinned at the
    /// cap rather than creeping past it.
    #[test]
    fn subscribe_enforces_the_per_client_cap() {
        let mut store = MetadataStore::default();
        let client = ClientId(1);

        for n in 0..MAX_SUBSCRIPTIONS_PER_CLIENT {
            assert!(
                store.subscribe(client, Scope::Global, key(n)),
                "subscription {n} is under the cap and must be accepted",
            );
        }
        assert_eq!(
            store
                .subscriptions
                .iter()
                .filter(|(c, _, _)| *c == client)
                .count(),
            MAX_SUBSCRIPTIONS_PER_CLIENT,
        );

        let refused = store.subscribe(client, Scope::Global, key(MAX_SUBSCRIPTIONS_PER_CLIENT));
        assert!(!refused, "the cap+1'th distinct key must be refused");
        assert_eq!(
            store
                .subscriptions
                .iter()
                .filter(|(c, _, _)| *c == client)
                .count(),
            MAX_SUBSCRIPTIONS_PER_CLIENT,
            "a refused subscribe must not grow the client's held count past the cap",
        );
        // Refuse, not evict (phux-w7z2.59 design choice): the refusal must
        // not have made room for itself by dropping an earlier subscription.
        // An eviction policy here would silently break a subscription that
        // was working fine to admit one that was never established — worse
        // than just declining the new one.
        assert_eq!(
            store.subscribers_for(&Scope::Global, &key(0)),
            vec![client],
            "a refused subscribe must not evict any existing subscription",
        );
    }

    /// Re-subscribing an already-held triple is a no-op on the count, so it
    /// never itself trips the cap — a client cannot be starved of its own
    /// re-subscribes by having previously reached the limit.
    #[test]
    fn resubscribing_an_existing_triple_at_the_cap_stays_accepted() {
        let mut store = MetadataStore::default();
        let client = ClientId(1);
        for n in 0..MAX_SUBSCRIPTIONS_PER_CLIENT {
            assert!(store.subscribe(client, Scope::Global, key(n)));
        }

        assert!(
            store.subscribe(client, Scope::Global, key(0)),
            "re-subscribing an existing triple must succeed even while at the cap",
        );
    }

    /// The cap is per-client: one client hitting its limit must not affect
    /// another client's ability to subscribe.
    #[test]
    fn the_cap_is_per_client_not_global() {
        let mut store = MetadataStore::default();
        let hog = ClientId(1);
        let other = ClientId(2);
        for n in 0..MAX_SUBSCRIPTIONS_PER_CLIENT {
            assert!(store.subscribe(hog, Scope::Global, key(n)));
        }
        assert!(!store.subscribe(hog, Scope::Global, key(MAX_SUBSCRIPTIONS_PER_CLIENT)));

        assert!(
            store.subscribe(other, Scope::Global, key(0)),
            "a different client must still be able to subscribe to the same key",
        );
    }

    /// `forget_terminal` clears the Terminal's K/V bucket (existing
    /// behavior) AND now reaps any subscription naming that Terminal,
    /// while leaving subscriptions for other scopes/terminals untouched.
    /// This is the "reap" half of phux-w7z2.59: a subscription's
    /// connection can outlive the Terminal it named.
    #[test]
    fn forget_terminal_reaps_only_subscriptions_naming_that_terminal() {
        let mut store = MetadataStore::default();
        let client = ClientId(1);
        let dead = WireTerminalId::local(1);
        let alive = WireTerminalId::local(2);

        assert!(store.subscribe(client, Scope::Terminal(dead.clone()), "k".to_owned()));
        assert!(store.subscribe(client, Scope::Terminal(alive.clone()), "k".to_owned()));
        assert!(store.subscribe(client, Scope::Global, "k".to_owned()));

        store.forget_terminal(&dead);

        assert!(
            store
                .subscribers_for(&Scope::Terminal(dead.clone()), "k")
                .is_empty(),
            "the dead Terminal's subscription must be reaped",
        );
        assert_eq!(
            store.subscribers_for(&Scope::Terminal(alive), "k"),
            vec![client],
            "a different Terminal's subscription must survive",
        );
        assert_eq!(
            store.subscribers_for(&Scope::Global, "k"),
            vec![client],
            "a Global subscription must survive",
        );
    }

    /// Reaping a dead Terminal's subscription frees a cap slot: a
    /// connection that churns through many short-lived Terminals must not
    /// have every one of them permanently occupy its subscription budget.
    #[test]
    fn forget_terminal_frees_a_cap_slot_for_reuse() {
        let mut store = MetadataStore::default();
        let client = ClientId(1);
        let terminal = WireTerminalId::local(1);

        for n in 0..MAX_SUBSCRIPTIONS_PER_CLIENT {
            assert!(store.subscribe(client, Scope::Terminal(terminal.clone()), key(n)));
        }
        assert!(!store.subscribe(client, Scope::Global, "overflow".to_owned()));

        store.forget_terminal(&terminal);

        assert!(
            store.subscribe(client, Scope::Global, "overflow".to_owned()),
            "reaping the dead Terminal's subscriptions must free room under the cap",
        );
    }
}
