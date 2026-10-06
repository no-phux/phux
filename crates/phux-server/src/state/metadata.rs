use std::collections::{HashMap, HashSet};

use phux_protocol::ids::{GroupId, ResourceId as WireResourceId};
use phux_protocol::wire::frame::Scope;
use tokio::sync::mpsc;

use super::ServerState;
use super::client::ClientId;
use crate::mailbox::Outbound;

/// Most metadata subscriptions one connection may hold. There is no reply
/// or unsubscribe verb, so this bounds a long-lived connection; the TUI
/// stays under 500 even with ~150 panes.
const MAX_SUBSCRIPTIONS_PER_CLIENT: usize = 512;

/// Per-scope L3 metadata store (SPEC §7.4) and its subscription registry.
#[derive(Debug, Default)]
pub struct MetadataStore {
    /// Per-Terminal key → value, cleared when the Terminal closes.
    terminal: HashMap<WireResourceId, HashMap<String, Vec<u8>>>,
    /// Per-Group key → value.
    group: HashMap<GroupId, HashMap<String, Vec<u8>>>,
    /// Global key → value.
    global: HashMap<String, Vec<u8>>,
    /// Active `(client, scope, key)` subscriptions, scanned linearly
    /// (subscriptions are sparse).
    subscriptions: HashSet<(ClientId, Scope, String)>,
    /// Bumped by every change to a key the workspace archive reads
    /// ([`is_archived_key`]); part of the autosave revision (ADR-0150).
    archived_revision: u64,
}

/// Whether `phux workspace save` reads `key`: a layout envelope
/// (`<prefix>.layout/v1/<session>`) or a native agent-session record
/// (ADR-0068). Hot keys such as agent state stay out of the autosave
/// revision.
fn is_archived_key(key: &str) -> bool {
    key == phux_protocol::wire::frame::RESOURCE_AGENT_SESSION_KEY || key.contains(".layout/v1")
}

/// Outcome of a `SET_METADATA`; `Unchanged` suppresses the broadcast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataSetOutcome {
    /// Key did not exist or held a different value; value was written.
    Changed,
    /// Key already held the identical value; no broadcast needed.
    Unchanged,
}

/// Outcome of [`super::ServerState::rename_session`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameOutcome {
    /// Renamed (or already so named): `COMMAND_RESULT { Ok }`.
    Renamed,
    /// No session matched the current name; reply `SESSION_NOT_FOUND`.
    NotFound,
    /// The name is taken: `INVALID_COMMAND`.
    NameTaken,
}

/// Outcome of [`super::ServerState::set_session_keep_empty`] (ADR-0105).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepEmptyOutcome {
    /// The session's mark flipped to the requested value; the session stays.
    Changed,
    /// The session already carried the requested mark; nothing changed.
    Unchanged,
    /// Cleared on an empty session, which was therefore removed.
    Removed,
    /// No session matched the name.
    NotFound,
}

/// Keys the server intercepts on write (the payload is a command, not a
/// stored value); the only keys `metadata_broadcast` accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerInterceptedKey {
    /// `phux.session.name/v1` — rename payload `current\0new`.
    SessionName,
    /// `phux.session.keep_empty/v1` — mark payload `name\0true|false`.
    SessionKeepEmpty,
}

impl ServerInterceptedKey {
    /// The conventional key string this variant broadcasts on.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionName => phux_protocol::wire::frame::SESSION_NAME_KEY,
            Self::SessionKeepEmpty => phux_protocol::wire::frame::SESSION_KEEP_EMPTY_KEY,
        }
    }
}

impl MetadataStore {
    /// Get the value at `(scope, key)`, if any.
    #[must_use]
    pub fn get(&self, scope: &Scope, key: &str) -> Option<Vec<u8>> {
        match scope {
            Scope::Resource(tid) => self.terminal.get(tid).and_then(|m| m.get(key)).cloned(),
            Scope::Group(gid) => self.group.get(gid).and_then(|m| m.get(key)).cloned(),
            Scope::Global => self.global.get(key).cloned(),
            // Unknown forward-compat scope: no value.
            _ => None,
        }
    }

    /// Set `(scope, key)`; reports whether it changed.
    pub fn set(&mut self, scope: &Scope, key: &str, value: Vec<u8>) -> MetadataSetOutcome {
        let bucket: &mut HashMap<String, Vec<u8>> = match scope {
            Scope::Resource(tid) => self.terminal.entry(tid.clone()).or_default(),
            Scope::Group(gid) => self.group.entry(*gid).or_default(),
            Scope::Global => &mut self.global,
            // Unknown forward-compat scope: drop the write.
            _ => return MetadataSetOutcome::Unchanged,
        };
        if let Some(prev) = bucket.get(key)
            && prev == &value
        {
            return MetadataSetOutcome::Unchanged;
        }
        bucket.insert(key.to_owned(), value);
        self.note_archived_change(key);
        MetadataSetOutcome::Changed
    }

    /// Delete `(scope, key)`; reports whether it existed.
    pub fn delete(&mut self, scope: &Scope, key: &str) -> bool {
        let existed = self.remove(scope, key);
        if existed {
            self.note_archived_change(key);
        }
        existed
    }

    /// How many times a key the workspace archive reads (a layout envelope
    /// or an agent-session record) has changed.
    #[must_use]
    pub const fn archived_revision(&self) -> u64 {
        self.archived_revision
    }

    fn note_archived_change(&mut self, key: &str) {
        if is_archived_key(key) {
            self.archived_revision = self.archived_revision.wrapping_add(1);
        }
    }

    fn remove(&mut self, scope: &Scope, key: &str) -> bool {
        match scope {
            Scope::Resource(tid) => self
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
            Scope::Resource(tid) => self
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

    /// Drop `terminal`'s keys and every subscription naming it, so a
    /// long-lived watcher does not accumulate dead subscriptions.
    pub fn forget_terminal(&mut self, terminal: &WireResourceId) {
        self.terminal.remove(terminal);
        self.subscriptions
            .retain(|(_, scope, _)| !matches!(scope, Scope::Resource(tid) if tid == terminal));
    }

    /// Register a subscription under `MAX_SUBSCRIPTIONS_PER_CLIENT`. `true`
    /// if active (re-subscribing is idempotent); `false` if the client is at
    /// the cap. The caller can only log a refusal (no reply frame).
    pub fn subscribe(&mut self, client: ClientId, scope: Scope, key: String) -> bool {
        let triple = (client, scope, key);
        if self.subscriptions.contains(&triple) {
            return true;
        }
        // A linear scan on a rare setup-time call.
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

    /// Every client subscribed to `(scope, key)`, in no particular order.
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

    /// Store `value` and broadcast `MetadataChanged` to L3-capable
    /// subscribers (`try_send`). Returns the targeted clients.
    pub fn metadata_set(&mut self, scope: &Scope, key: &str, value: Vec<u8>) -> Vec<ClientId> {
        self.metadata_set_by(scope, key, value, None)
    }

    /// [`Self::metadata_set`] attributed to `writer` (ADR-0123); `None` is
    /// the server.
    pub fn metadata_set_by(
        &mut self,
        scope: &Scope,
        key: &str,
        value: Vec<u8>,
        writer: Option<ClientId>,
    ) -> Vec<ClientId> {
        // Equal bytes return early before any broadcast or write.
        let unchanged = self
            .metadata
            .get(scope, key)
            .is_some_and(|prev| prev == value);
        if unchanged {
            return Vec::new();
        }
        let delivered = self.broadcast_metadata_change(scope, key, Some(&value), writer);
        let _ = self.metadata.set(scope, key, value);
        delivered
    }

    /// Delete and broadcast a tombstone; idempotent.
    pub fn metadata_delete(&mut self, scope: &Scope, key: &str) -> Vec<ClientId> {
        self.metadata_delete_by(scope, key, None)
    }

    /// Drop a Terminal's keys and subscriptions (the satellite mirror, after
    /// tombstoning, ADR-0136).
    pub(crate) fn drop_terminal_metadata(&mut self, terminal: &WireResourceId) {
        self.metadata.forget_terminal(terminal);
    }

    /// [`Self::metadata_delete`] attributed to `writer` (ADR-0123).
    pub fn metadata_delete_by(
        &mut self,
        scope: &Scope,
        key: &str,
        writer: Option<ClientId>,
    ) -> Vec<ClientId> {
        let existed = self.metadata.delete(scope, key);
        if !existed {
            return Vec::new();
        }
        self.broadcast_metadata_change(scope, key, None, writer)
    }

    /// Broadcast `value` for an intercepted key without storing it: the
    /// payload is a command, and storing it would leak into reads and let
    /// dedup swallow a legitimate repeat. Callers broadcast only when the
    /// mutation happened. Returns the targeted clients.
    #[must_use]
    pub fn metadata_broadcast(
        &self,
        scope: &Scope,
        key: ServerInterceptedKey,
        value: &[u8],
    ) -> Vec<ClientId> {
        self.metadata_broadcast_by(scope, key, value, None)
    }

    /// [`Self::metadata_broadcast`] attributed to `writer` (ADR-0123).
    #[must_use]
    pub fn metadata_broadcast_by(
        &self,
        scope: &Scope,
        key: ServerInterceptedKey,
        value: &[u8],
        writer: Option<ClientId>,
    ) -> Vec<ClientId> {
        self.broadcast_metadata_change(scope, key.as_str(), Some(value), writer)
    }

    /// The single fanout for set, delete, and broadcast-only: `None` is a
    /// tombstone; `writer` becomes the frame's `actor`.
    fn broadcast_metadata_change(
        &self,
        scope: &Scope,
        key: &str,
        value: Option<&[u8]>,
        writer: Option<ClientId>,
    ) -> Vec<ClientId> {
        let subscribers = self.metadata.subscribers_for(scope, key);
        let actor = writer.map(|client| self.clients.actor_ref(client));
        self.clients
            .broadcast_metadata_changed(&subscribers, scope, key, value, actor.as_ref())
    }

    /// Track a one-shot session-create result key (already written); at
    /// most 256 unread per connection, oldest evicted.
    pub fn track_session_create_result(&mut self, client_id: ClientId, key: String) {
        const MAX_PENDING_PER_CLIENT: usize = 256;
        // Scope the borrow so `metadata_delete` can take `&mut self`.
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
        self.disown_session_create_result(key);
    }

    /// Forget every owner of the result at `key`, keeping the value (a
    /// replayed create hands it back, ADR-0126).
    pub fn disown_session_create_result(&mut self, key: &str) {
        for keys in self.clients.session_create_results.values_mut() {
            keys.retain(|candidate| candidate != key);
        }
        self.clients
            .session_create_results
            .retain(|_, keys| !keys.is_empty());
    }

    /// Subscribe `client_id` (already checked L3-capable), remembering `tx`
    /// for headless consumers. `false` at the per-connection cap, in which
    /// case the mailbox is not remembered either.
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

    /// `metadata_broadcast` accepts only the intercepted keys.
    #[test]
    fn intercepted_key_variants_are_the_broadcast_only_session_writes() {
        assert_eq!(
            ServerInterceptedKey::SessionName.as_str(),
            phux_protocol::wire::frame::SESSION_NAME_KEY,
        );
        assert_eq!(
            ServerInterceptedKey::SessionKeepEmpty.as_str(),
            phux_protocol::wire::frame::SESSION_KEEP_EMPTY_KEY,
        );
    }

    /// Every intercepted key has a workload-auth catalog entry (ADR-0125)
    /// its writes classify onto, and refused keys are denied.
    #[test]
    fn every_intercepted_key_has_a_catalog_classification() {
        use phux_protocol::kinds::{self, Carrier, Classification};
        use phux_protocol::wire::frame::{
            FrameKind, RESOURCE_PANE_OCCUPANT_KEY, SESSION_CREATE_KEY,
            SESSION_CREATE_RESULT_KEY_PREFIX, Scope, WHOAMI_KEY,
        };

        let set = |key: &str| FrameKind::SetMetadata {
            request_id: 1,
            scope: Scope::Global,
            key: key.to_owned(),
            value: b"x".to_vec(),
        };
        let intercepted = [
            ServerInterceptedKey::SessionName,
            ServerInterceptedKey::SessionKeepEmpty,
        ]
        .map(|key| match key {
            ServerInterceptedKey::SessionName | ServerInterceptedKey::SessionKeepEmpty => {
                key.as_str()
            }
        });
        for key in intercepted.into_iter().chain([SESSION_CREATE_KEY]) {
            let method = kinds::method_named(key);
            assert!(method.is_some(), "{key} has no catalog entry");
            let method = method.unwrap_or_else(|| unreachable!());
            assert_eq!(method.carrier, Carrier::Metadata(key));
            let rule = kinds::frame_rule(&set(key));
            assert!(
                method.rules.iter().any(|row| std::ptr::eq(*row, rule)),
                "{key}: a Global SET lands on `{}`, which its entry does not name",
                rule.case
            );
        }
        let result = format!("{SESSION_CREATE_RESULT_KEY_PREFIX}token");
        for key in [RESOURCE_PANE_OCCUPANT_KEY, WHOAMI_KEY, result.as_str()] {
            assert_eq!(
                kinds::classify_frame(&set(key)),
                Classification::Deny,
                "{key}"
            );
        }
    }

    /// The cap refuses the next distinct key without mutating the store.
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
        // Refuse, never evict a working subscription.
        assert_eq!(
            store.subscribers_for(&Scope::Global, &key(0)),
            vec![client],
            "a refused subscribe must not evict any existing subscription",
        );
    }

    /// Re-subscribing a held triple never trips the cap.
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

    /// The cap is per client.
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

    /// `forget_terminal` removes the Terminal's keys and subscriptions only.
    #[test]
    fn forget_terminal_reaps_only_subscriptions_naming_that_terminal() {
        let mut store = MetadataStore::default();
        let client = ClientId(1);
        let dead = WireResourceId::local(1);
        let alive = WireResourceId::local(2);

        assert!(store.subscribe(client, Scope::Resource(dead.clone()), "k".to_owned()));
        assert!(store.subscribe(client, Scope::Resource(alive.clone()), "k".to_owned()));
        assert!(store.subscribe(client, Scope::Global, "k".to_owned()));

        store.forget_terminal(&dead);

        assert!(
            store
                .subscribers_for(&Scope::Resource(dead.clone()), "k")
                .is_empty(),
            "the dead Terminal's subscription must be reaped",
        );
        assert_eq!(
            store.subscribers_for(&Scope::Resource(alive), "k"),
            vec![client],
            "a different Terminal's subscription must survive",
        );
        assert_eq!(
            store.subscribers_for(&Scope::Global, "k"),
            vec![client],
            "a Global subscription must survive",
        );
    }

    /// Reaping a Terminal frees its subscriptions' cap slots.
    #[test]
    fn forget_terminal_frees_a_cap_slot_for_reuse() {
        let mut store = MetadataStore::default();
        let client = ClientId(1);
        let terminal = WireResourceId::local(1);

        for n in 0..MAX_SUBSCRIPTIONS_PER_CLIENT {
            assert!(store.subscribe(client, Scope::Resource(terminal.clone()), key(n)));
        }
        assert!(!store.subscribe(client, Scope::Global, "overflow".to_owned()));

        store.forget_terminal(&terminal);

        assert!(
            store.subscribe(client, Scope::Global, "overflow".to_owned()),
            "reaping the dead Terminal's subscriptions must free room under the cap",
        );
    }
}
