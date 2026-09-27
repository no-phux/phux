//! The per-client table: every map keyed on a connected client, plus the
//! [`ClientId`] allocator.
//!
//! Two lifetimes, cleared by different edges:
//!
//! * **Attachment-scoped**: [`Self::attached`], the subscription maps, and
//!   session-create result keys, cleared by `ServerState::detach` (a
//!   mid-connection `DETACH` or transport close).
//! * **Connection-scoped**: layers, peer identity, grants, names, viewers,
//!   cancellation and revocation signals, cleared only by
//!   `ServerState::forget_connection` (a second HELLO is a protocol error,
//!   so nothing can restore them).
//!
//! This type owns the bookkeeping, not the policy; logic spanning other
//! tables stays on `ServerState`. Everything is `pub(super)` and sync.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use phux_core::ids::SessionId;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, Layer, LayerSet};
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{ActorRef, FrameKind, Scope};
use tokio::sync::{Notify, mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::client::{AttachedClient, ClientId};
use super::events::{EventScope, EventSubscription};
use super::journal::JournalEntry;
use crate::mailbox::Outbound;

/// Every client-keyed table plus the [`ClientId`] allocator; synchronized
/// by the surrounding `Mutex<ServerState>`.
#[derive(Debug)]
pub(super) struct ClientTable {
    /// Currently attached clients. Read outside `state` only through
    /// [`super::ServerState::attached`].
    pub(super) attached: HashMap<ClientId, AttachedClient>,
    /// Next [`ClientId`]: monotonic from 1 (0 reads as a placeholder),
    /// never reused, saturating rather than wrapping.
    next_id: u64,
    /// Negotiated [`LayerSet`] from HELLO, gating L3 frames (SPEC §16.4).
    /// Defaults to [`LayerSet::all`] without a HELLO. Connection-scoped.
    pub(super) layers: HashMap<ClientId, LayerSet>,
    /// Agent-event subscriptions (SPEC §7.5) with their mailboxes, since a
    /// `watch` client subscribes without attaching.
    pub(super) event_subscriptions: HashMap<ClientId, EventSubscription>,
    /// Mailboxes of L3 metadata subscribers (headless consumers subscribe
    /// without attaching); the triples live in the metadata store. Fanout
    /// prefers `attached`, so each client gets one delivery.
    pub(super) metadata_mailboxes: HashMap<ClientId, mpsc::Sender<Outbound>>,
    /// Mailboxes of `ATTACH_RESOURCE` subscribers, which need no session
    /// `ATTACH` (L1 §5.1), so terminal-scoped fanout such as
    /// `RESOURCE_CLOSED` (L1 §3.1) can reach them. Fanout prefers
    /// `attached`, so each client resolves to one mailbox.
    pub(super) terminal_mailboxes: HashMap<ClientId, mpsc::Sender<Outbound>>,
    /// Transport-stamped peer identities. Connection-scoped.
    pub(super) peer_identities: HashMap<ClientId, crate::auth::ConnectionIdentity>,
    /// Cancellation root per live transport (dropping one sender clone
    /// cannot close writers held by others). Connection-scoped.
    pub(super) connection_cancellations: HashMap<ClientId, CancellationToken>,
    /// The authority minted at HELLO (`docs/spec/workload-auth.md` §5, §7),
    /// enforced at every dispatch; no entry means everything is refused.
    pub(super) grants: HashMap<ClientId, crate::policy::ConnectionGrant>,
    /// Revocation signal per connection: once a goodbye is set, the writer
    /// drops its queue, says goodbye, and closes (§7).
    pub(super) revocation_signals: HashMap<ClientId, watch::Sender<Option<crate::policy::Goodbye>>>,
    /// Wakes the revocation watcher when a connection gets its grant.
    pub(super) revocation_wake: Arc<Notify>,
    /// One-shot session-create result keys per connection, removed on
    /// disconnect and capped per client.
    pub(super) session_create_results: HashMap<ClientId, VecDeque<String>>,
    /// `HELLO.client_name` per connection, the label on its `ActorRef`
    /// (ADR-0123). Connection-scoped.
    pub(super) client_names: HashMap<ClientId, String>,
    /// Terminals each connection subscribed as `VIEWER` (ADR-0127), by wire
    /// id. Cleared by `forget_connection`, a `PRIMARY` attach, or reaping.
    pub(super) viewers: HashMap<ClientId, HashSet<WireResourceId>>,
    /// Epoch for the next event subscription, distinguishing a pump's
    /// subscription from a later one.
    next_subscription_epoch: u64,
}

impl Default for ClientTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientTable {
    /// Build an empty table with the id allocator at `1`.
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            attached: HashMap::new(),
            next_id: 1,
            layers: HashMap::new(),
            event_subscriptions: HashMap::new(),
            metadata_mailboxes: HashMap::new(),
            terminal_mailboxes: HashMap::new(),
            peer_identities: HashMap::new(),
            connection_cancellations: HashMap::new(),
            grants: HashMap::new(),
            revocation_signals: HashMap::new(),
            revocation_wake: Arc::new(Notify::new()),
            session_create_results: HashMap::new(),
            client_names: HashMap::new(),
            viewers: HashMap::new(),
            next_subscription_epoch: 0,
        }
    }

    // -- attach roles (ADR-0127) ---------------------------------------

    /// Whether `client` subscribed `terminal` as a `VIEWER`.
    #[must_use]
    pub(super) fn is_viewer(&self, client: ClientId, terminal: &WireResourceId) -> bool {
        self.viewers
            .get(&client)
            .is_some_and(|terminals| terminals.contains(terminal))
    }

    /// Mark or unmark `client` as a viewer of `terminal`; returns whether it
    /// changed. Empty sets are dropped.
    pub(super) fn mark_viewer(
        &mut self,
        client: ClientId,
        terminal: &WireResourceId,
        viewer: bool,
    ) -> bool {
        if viewer {
            return self
                .viewers
                .entry(client)
                .or_default()
                .insert(terminal.clone());
        }
        let Some(terminals) = self.viewers.get_mut(&client) else {
            return false;
        };
        let removed = terminals.remove(terminal);
        if terminals.is_empty() {
            self.viewers.remove(&client);
        }
        removed
    }

    /// Drop every viewer mark on `terminal`, which is gone.
    pub(super) fn forget_viewed_terminal(&mut self, terminal: &WireResourceId) {
        self.viewers.retain(|_, terminals| {
            terminals.remove(terminal);
            !terminals.is_empty()
        });
    }

    /// Every connection that subscribed `terminal` as a `VIEWER`, ascending.
    #[must_use]
    pub(super) fn viewers_of(&self, terminal: &WireResourceId) -> Vec<ClientId> {
        let mut viewers: Vec<ClientId> = self
            .viewers
            .iter()
            .filter(|(_, terminals)| terminals.contains(terminal))
            .map(|(client, _)| *client)
            .collect();
        viewers.sort_unstable_by_key(|client| client.0);
        viewers
    }

    // -- identity ------------------------------------------------------

    /// Allocate the next monotonic [`ClientId`].
    pub(super) const fn new_client_id(&mut self) -> ClientId {
        let id = ClientId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    // -- negotiated layers ---------------------------------------------

    /// Record the HELLO layer set (latest wins).
    pub(super) fn set_layers(&mut self, client: ClientId, layers: LayerSet) {
        self.layers.insert(client, layers);
    }

    /// The HELLO layer set, [`LayerSet::all`] if none was seen.
    #[must_use]
    pub(super) fn layers(&self, client: ClientId) -> LayerSet {
        self.layers
            .get(&client)
            .copied()
            .unwrap_or_else(LayerSet::all)
    }

    /// Whether `client` negotiated L3 (gates `METADATA_CHANGED`).
    #[must_use]
    pub(super) fn speaks_l3(&self, client: ClientId) -> bool {
        self.layers(client).contains(Layer::L3)
    }

    // -- attached clients ----------------------------------------------

    /// Update an attached client's capabilities; `false` if not attached.
    pub(super) fn set_capabilities(&mut self, client: ClientId, caps: ClientCapabilities) -> bool {
        self.attached
            .get_mut(&client)
            .map(|c| {
                c.client_caps = caps;
            })
            .is_some()
    }

    /// Patch an attached client's color tier; `false` if not attached.
    pub(super) fn set_color_support(&mut self, client: ClientId, color: ColorSupport) -> bool {
        self.attached
            .get_mut(&client)
            .map(|c| {
                c.client_caps = c.client_caps.with_color_support(color);
            })
            .is_some()
    }

    /// `(client, mailbox)` for every client attached to `session`
    /// (per-terminal consumers excluded).
    #[must_use]
    pub(super) fn attached_in_session(
        &self,
        session: SessionId,
    ) -> Vec<(ClientId, mpsc::Sender<Outbound>)> {
        self.attached
            .values()
            .filter(|client| client.session == session)
            .map(|client| (client.id, client.tx.clone()))
            .collect()
    }

    // -- per-Terminal subscription mailboxes ---------------------------

    /// Remember `client`'s mailbox for terminal-scoped fanout, in the same
    /// critical section that subscribes it.
    pub(super) fn remember_terminal_mailbox(
        &mut self,
        client: ClientId,
        tx: mpsc::Sender<Outbound>,
    ) {
        self.terminal_mailboxes.insert(client, tx);
    }

    /// `client`'s mailbox for terminal-scoped fanout (`attached` first);
    /// `None` means the connection is already going away.
    #[must_use]
    pub(super) fn terminal_fanout_mailbox(
        &self,
        client: ClientId,
    ) -> Option<&mpsc::Sender<Outbound>> {
        self.attached
            .get(&client)
            .map(|attached| &attached.tx)
            .or_else(|| self.terminal_mailboxes.get(&client))
    }

    // -- agent-event subscriptions -------------------------------------

    /// `client`'s event subscription, created on first use; one per client
    /// whichever verb installed its scopes (ADR-0123).
    pub(super) fn event_subscription(
        &mut self,
        client: ClientId,
        tx: mpsc::Sender<Outbound>,
    ) -> &mut EventSubscription {
        let epoch = &mut self.next_subscription_epoch;
        self.event_subscriptions.entry(client).or_insert_with(|| {
            *epoch += 1;
            EventSubscription::new(tx, *epoch)
        })
    }

    /// Offer one journaled event to every subscription.
    pub(super) fn offer_event(&mut self, entry: &JournalEntry) {
        super::events::offer_to_all(self.event_subscriptions.values_mut(), entry);
    }

    /// The `ActorRef` naming `client`: wire id, credential, and HELLO name.
    #[must_use]
    pub(super) fn actor_ref(&self, client: ClientId) -> ActorRef {
        let wire = phux_protocol::ids::ClientId::new(u32::try_from(client.0).unwrap_or(u32::MAX));
        let credential_id = self
            .peer_identities
            .get(&client)
            .and_then(|identity| identity.credential.as_ref())
            .map(|credential| credential.id.clone());
        ActorRef::new(wire)
            .with_credential_id(credential_id)
            .with_client_name(self.client_names.get(&client).cloned())
    }

    /// Drop `client`'s per-terminal event scope for `wire`; an empty
    /// subscription is dropped.
    pub(super) fn unsubscribe_terminal_events(&mut self, client: ClientId, wire: &WireResourceId) {
        if let Some(sub) = self.event_subscriptions.get_mut(&client) {
            sub.scopes.remove(&EventScope::Terminal(wire.clone()));
            // A subscription still owed a gap stays until its pump delivers it.
            if sub.scopes.is_empty()
                && !sub.is_owed_work()
                && let Some(sub) = self.event_subscriptions.remove(&client)
            {
                sub.retire();
            }
        }
    }

    // -- peer identities -----------------------------------------------

    /// Store a peer identity for a client.
    pub(super) fn set_peer_identity(
        &mut self,
        client: ClientId,
        identity: phux_protocol::policy::PeerIdentity,
    ) {
        self.peer_identities.insert(client, identity.into());
    }

    /// Store transport identity and credential attestation for a client.
    pub(super) fn set_connection_identity(
        &mut self,
        client: ClientId,
        identity: crate::auth::ConnectionIdentity,
    ) {
        self.peer_identities.insert(client, identity);
    }

    /// Look up a peer identity by client id.
    #[must_use]
    pub(super) fn peer_identity(
        &self,
        client: ClientId,
    ) -> Option<&phux_protocol::policy::PeerIdentity> {
        self.peer_identities
            .get(&client)
            .map(|identity| &identity.peer)
    }

    /// Look up the credential captured for this connection.
    #[must_use]
    pub(super) fn authenticated_credential(
        &self,
        client: ClientId,
    ) -> Option<&crate::auth::AuthenticatedCredential> {
        self.peer_identities
            .get(&client)
            .and_then(|identity| identity.credential.as_ref())
    }

    /// Record the ssh origin a same-uid bridge announced (no-op without an
    /// identity).
    pub(super) fn set_ssh_origin(
        &mut self,
        client: ClientId,
        origin: phux_protocol::wire::ssh_origin::SshOrigin,
    ) {
        if let Some(identity) = self.peer_identities.get_mut(&client) {
            identity.ssh_origin = Some(origin);
        }
    }

    /// The ssh origin recorded for this connection, if any.
    #[must_use]
    pub(super) fn ssh_origin(
        &self,
        client: ClientId,
    ) -> Option<phux_protocol::wire::ssh_origin::SshOrigin> {
        self.peer_identities
            .get(&client)
            .and_then(|identity| identity.ssh_origin)
    }

    /// Remove a peer identity when a client disconnects.
    pub(super) fn remove_peer_identity(&mut self, client: ClientId) {
        self.peer_identities.remove(&client);
    }

    // -- one-shot session-create results -------------------------------

    /// Whether any live connection owns an unread result at `key`.
    #[must_use]
    pub(super) fn session_create_result_is_pending(&self, key: &str) -> bool {
        self.session_create_results
            .values()
            .any(|keys| keys.iter().any(|candidate| candidate == key))
    }

    /// Whether `client` owns the unread nonce-bearing result at `key`.
    #[must_use]
    pub(super) fn owns_session_create_result(&self, client: ClientId, key: &str) -> bool {
        self.session_create_results
            .get(&client)
            .is_some_and(|keys| keys.iter().any(|candidate| candidate == key))
    }

    // -- L3 metadata fanout --------------------------------------------

    /// Remember `client`'s mailbox for `METADATA_CHANGED` fanout.
    pub(super) fn remember_metadata_mailbox(
        &mut self,
        client: ClientId,
        tx: mpsc::Sender<Outbound>,
    ) {
        self.metadata_mailboxes.insert(client, tx);
    }

    /// Fan `MetadataChanged` out to every L3-capable, open subscriber,
    /// resolving mailboxes from `attached` or the metadata mailboxes (so
    /// headless `phux watch` consumers are reached). Returns the targets.
    pub(super) fn broadcast_metadata_changed(
        &self,
        subscribers: &[ClientId],
        scope: &Scope,
        key: &str,
        value: Option<&[u8]>,
        actor: Option<&ActorRef>,
    ) -> Vec<ClientId> {
        let mut delivered = Vec::with_capacity(subscribers.len());
        for client_id in subscribers {
            if !self.speaks_l3(*client_id) {
                continue;
            }
            let Some(tx) = self
                .attached
                .get(client_id)
                .map(|client| &client.tx)
                .or_else(|| self.metadata_mailboxes.get(client_id))
            else {
                continue;
            };
            let frame = FrameKind::MetadataChanged {
                scope: scope.clone(),
                key: key.to_owned(),
                value: value.map(<[u8]>::to_vec),
                actor: actor.cloned(),
            };
            // `try_send`: we hold the lock; a dropped notification is
            // acceptable (SPEC §7.4).
            if tx.try_send(Outbound::Frame(frame)).is_ok() {
                delivered.push(*client_id);
            }
        }
        delivered
    }
}
