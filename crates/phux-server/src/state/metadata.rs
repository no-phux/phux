use std::collections::{HashMap, HashSet};

use phux_protocol::ids::{GroupId, TerminalId as WireTerminalId};
use phux_protocol::wire::frame::Scope;

use super::ServerState;
use super::client::ClientId;

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

    /// Drop every key scoped to `terminal`. Called when the Terminal
    /// closes (the L1 lifecycle that owns the per-Terminal scope — see
    /// the `terminal` field doc). Subscriptions targeting the dead
    /// Terminal are connection-scoped and are reaped on detach, so they
    /// are left untouched here.
    pub fn forget_terminal(&mut self, terminal: &WireTerminalId) {
        self.terminal.remove(terminal);
    }

    /// Register `(client, scope, key)` as an active subscription. The
    /// underlying set is idempotent: re-subscribing the same triple is
    /// a noop.
    pub fn subscribe(&mut self, client: ClientId, scope: Scope, key: String) {
        self.subscriptions.insert((client, scope, key));
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
        let subscribers = self.metadata.subscribers_for(scope, key);
        let delivered =
            self.clients
                .broadcast_metadata_changed(&subscribers, scope, key, Some(&value));
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
        let subscribers = self.metadata.subscribers_for(scope, key);
        self.clients
            .broadcast_metadata_changed(&subscribers, scope, key, None)
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
    pub fn metadata_subscribe(&mut self, client_id: ClientId, scope: Scope, key: String) {
        self.metadata.subscribe(client_id, scope, key);
    }
}
