//! The per-client table: every map keyed on a connected client's identity,
//! plus the monotonic allocator that mints those identities.
//!
//! The client-keyed fields that were flat on [`super::ServerState`] live here because
//! they share one lifetime — a client's connection. An entry appears when
//! the client identifies itself (HELLO, ATTACH, `SUBSCRIBE_EVENTS`, a
//! session-create submission) and every one of them disappears by the time
//! `ServerState::forget_connection` returns. Keeping them together is what
//! makes "forget everything about this client" one place to look instead of
//! five map removals scattered across `state::client`, `state::events`,
//! `state::policy`, and `state::metadata`, which is where they drifted out
//! of step before.
//!
//! # Two lifetimes, not one
//!
//! The maps split on *which* edge clears them, and conflating the two is
//! how phux-w7z2.55 happened:
//!
//! * **Attachment-scoped** — [`Self::attached`], the subscription maps, and
//!   the session-create result keys. `ServerState::detach` clears these on
//!   a mid-connection `DETACH` as well as on transport close.
//! * **Connection-scoped** — [`Self::layers`], [`Self::peer_identities`], and
//!   [`Self::connection_cancellations`], established for a live connection
//!   (a second HELLO is a protocol error). Only
//!   `ServerState::forget_connection` clears these, and only when the
//!   transport is going away.
//!
//! # Ownership boundary
//!
//! This type owns the *client-keyed bookkeeping*, not the policy. Anything
//! that needs a second cluster stays on `ServerState` and reads these
//! fields directly (they are `pub(super)`, matching `state::config`):
//!
//! * `attach` walks the registry to build the pane list and arms the
//!   self-exit clock, so only the `attached` insert belongs here.
//! * `detach` also releases input leases, satellite leases, output pumps,
//!   pane subscriptions, and L3 metadata subscriptions, and re-enters
//!   `ServerState::metadata_delete` for each abandoned session-create key.
//! * `set_client_viewport` bumps `ServerState::viewport_clock` inside the
//!   same borrow as the `attached` lookup; it relies on disjoint-field
//!   borrow splitting, so it pokes `clients.attached` rather than going
//!   through an accessor that would borrow all of `ServerState`.
//! * `resolve_terminal_geometry` / `resolve_terminal_cell_px` join this
//!   table to the pane subscriber lists and the window-size policy in one
//!   expression.
//! * `build_session_snapshot` scans `attached` for per-session client
//!   counts and then interns wire ids (`&mut self`).
//!
//! Nothing here is `async` and nothing awaits, so the state lock can never
//! be held across a suspension point through this type.
//!
//! The struct and every method are `pub(super)`: the accessors the runtime
//! calls stay on `ServerState` (see `state::client`, `state::events`,
//! `state::policy`, `state::metadata`), so the crate's public surface is
//! unchanged and the five previously-private maps stay exactly as
//! unreachable from outside `state` as they were as private fields. The
//! one previously-`pub` field, `attached`, is reachable through the
//! read-only [`super::ServerState::attached`] accessor — narrower than the
//! bare field it replaces, since every write still goes through `state`.

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

/// Every client-keyed table the server owns, plus the allocator that mints
/// fresh [`ClientId`]s.
///
/// Held as a single field on [`super::ServerState`]. Not thread-safe on
/// its own; the surrounding `Mutex<ServerState>` provides synchronization.
#[derive(Debug)]
pub(super) struct ClientTable {
    /// Currently attached clients, keyed by server-assigned id.
    ///
    /// The one field here with readers outside `state`; they reach it
    /// through the read-only [`super::ServerState::attached`] accessor.
    /// Every write is in `state` (`attach`, `detach`, the capability
    /// setters, `set_client_viewport`).
    pub(super) attached: HashMap<ClientId, AttachedClient>,
    /// Next [`ClientId`] to hand out. Monotonic from `1`; ids are never
    /// reused, and `0` is intentionally skipped so a log line reading
    /// `client=0` is obviously a placeholder. Saturates rather than wraps,
    /// because a duplicate id would silently cross-route two clients.
    next_id: u64,
    /// Per-client cache of the negotiated [`LayerSet`] from HELLO (SPEC
    /// §6.2). The dispatcher consults this before emitting any L3
    /// frame; non-L3 consumers MUST NOT see `METADATA_CHANGED` (SPEC
    /// §16.4). Default for a client that never sent HELLO (test
    /// scaffolding) is [`LayerSet::all`] — the most-permissive default
    /// keeps test setups simple; production clients always advertise.
    ///
    /// Connection-scoped: survives `DETACH` and is cleared only by
    /// `ServerState::forget_connection`. Because the default is
    /// permissive, clearing it early is a fail-open tier escalation, not
    /// a lost restriction.
    pub(super) layers: HashMap<ClientId, LayerSet>,
    /// Per-client agent-event subscriptions (SPEC §7.5, phux-y2t). Each
    /// subscribed client maps to its outbound mailbox plus the set of
    /// scopes it watches: `EventScope::Server` (every event) or one or
    /// more `EventScope::Terminal(id)` (per-pane). The push half of the
    /// agent surface; an additive accelerator of the CLI poll-floor
    /// `wait`. Cleared on detach, matching the L3 metadata subscription
    /// lifecycle.
    ///
    /// The mailbox is stored here (rather than resolved through
    /// [`Self::attached`]) because a `watch` client subscribes WITHOUT
    /// attaching — it connects, sends `SUBSCRIBE_EVENTS`, and streams. So
    /// event fanout must not depend on an `attached` entry that a pure
    /// watcher never creates.
    pub(super) event_subscriptions: HashMap<ClientId, EventSubscription>,
    /// Outbound mailbox of every client that holds an L3 metadata
    /// subscription, for exactly the same reason
    /// [`Self::event_subscriptions`] carries one: a headless consumer can
    /// `SUBSCRIBE_METADATA` **without attaching**, so `METADATA_CHANGED`
    /// fanout must not be resolved solely through [`Self::attached`].
    ///
    /// Only the mailbox lives here; the `(client, scope, key)` triples stay
    /// in [`super::metadata::MetadataStore`], which owns subscription
    /// matching. An attached subscriber has the same sender in both maps —
    /// [`Self::broadcast_metadata_changed`] prefers `attached` and falls
    /// back here, so there is exactly one delivery per client either way.
    /// Cleared on detach alongside the subscription triples.
    pub(super) metadata_mailboxes: HashMap<ClientId, mpsc::Sender<Outbound>>,
    /// Outbound mailbox of every client holding an `ATTACH_RESOURCE`
    /// subscription, for exactly the same reason
    /// [`Self::event_subscriptions`] and [`Self::metadata_mailboxes`] carry
    /// one: `ATTACH_RESOURCE` is a per-Terminal subscription that does
    /// **not** require a session-scoped `ATTACH` (L1 §5.1, "a session-scoped
    /// `ATTACH` is not required"), so such a consumer has no
    /// [`Self::attached`] entry and cannot be reached through it.
    ///
    /// Terminal *content* still reaches it — the per-`(client, terminal)`
    /// output pump owns its own clone of the mailbox — but the server's
    /// out-of-band terminal-scoped fanout (`RESOURCE_CLOSED`, L1 §3.1: "the
    /// server MUST emit it to every client subscribed to the Terminal")
    /// resolves mailboxes from the subscriber list, and every subscriber
    /// that only ever sent `ATTACH_RESOURCE` was silently filtered out.
    /// An agent watching one pane then never learned the pane died; it just
    /// stopped receiving output, which is indistinguishable from an idle
    /// pane (phux-w7z2.56).
    ///
    /// An attached subscriber has the same sender in both maps.
    /// [`Self::terminal_fanout_mailbox`] prefers `attached` and falls back
    /// here, so a client is resolved to exactly one mailbox either way.
    /// Cleared on detach alongside the subscriptions themselves.
    pub(super) terminal_mailboxes: HashMap<ClientId, mpsc::Sender<Outbound>>,
    /// Per-client peer identities, keyed by server-assigned client id.
    ///
    /// Connection-scoped, and stamped by the accepting transport before the
    /// client task is spawned — nothing on a live connection can restore it,
    /// so it survives `DETACH` and is cleared only by
    /// `ServerState::forget_connection`.
    pub(super) peer_identities: HashMap<ClientId, crate::auth::ConnectionIdentity>,
    /// Cancellation root for each live client transport. Relay delivery
    /// retirement uses this connection-scoped signal because dropping one
    /// outbound sender cannot close writers held alive by other sender clones.
    /// Cleared only by `ServerState::forget_connection`.
    pub(super) connection_cancellations: HashMap<ClientId, CancellationToken>,
    /// The authority minted for each connection at HELLO
    /// (`docs/spec/workload-auth.md` §5, §7), enforced at every dispatch.
    ///
    /// Connection-scoped: a second HELLO is a protocol error, so nothing on
    /// a live connection re-mints it, and only
    /// `ServerState::forget_connection` clears it. A connection with no
    /// entry is refused everything by the dispatch guard.
    pub(super) grants: HashMap<ClientId, crate::policy::ConnectionGrant>,
    /// Each live connection's revocation signal. Its writer watches the
    /// receiving half and, once a goodbye is set, drops everything queued,
    /// says the goodbye, and closes (`docs/spec/workload-auth.md` §7).
    /// Connection-scoped: cleared only by `ServerState::forget_connection`.
    pub(super) revocation_signals: HashMap<ClientId, watch::Sender<Option<crate::policy::Goodbye>>>,
    /// Wakes the revocation watcher when a connection it must watch gets
    /// its grant.
    pub(super) revocation_wake: Arc<Notify>,
    /// Nonce-bearing session-create result keys owned by each connection.
    ///
    /// Results are one-shot and connection-scoped even though their transport
    /// uses Global L3 metadata. Tracking ownership lets disconnect cleanup
    /// remove abandoned results, while the per-client cap bounds a connected
    /// client that submits creates without reading replies.
    pub(super) session_create_results: HashMap<ClientId, VecDeque<String>>,
    /// The `HELLO.client_name` each connection announced (ADR-0123): the
    /// label on the `ActorRef` of every event and metadata change it
    /// causes. Connection-scoped, like the HELLO layer set, so it survives
    /// `DETACH` and is cleared only by `ServerState::forget_connection`.
    pub(super) client_names: HashMap<ClientId, String>,
    /// The Terminals each connection subscribed as a `VIEWER` (ADR-0127),
    /// by the wire id it named, so a satellite Terminal on a hub is covered
    /// exactly like a local one. Connection-scoped tombstones: detach and a
    /// stream's end leave them, only `ServerState::forget_connection` or a
    /// fresh `PRIMARY` attach clears one, and a reaped Terminal's are pruned.
    pub(super) viewers: HashMap<ClientId, HashSet<WireResourceId>>,
    /// The epoch the next event subscription is created with, so a pump
    /// can tell its subscription from a later one for the same client.
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

    /// Mark or unmark `client` as a viewer of `terminal`. Returns whether
    /// the mark changed. An empty set drops the entry, so the map stays
    /// bounded across attach churn.
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
    ///
    /// Ids are never reused. `0` is intentionally skipped so log entries
    /// printing `client=0` are obviously a placeholder, not a real client.
    pub(super) const fn new_client_id(&mut self) -> ClientId {
        let id = ClientId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    // -- negotiated layers ---------------------------------------------

    /// Record the layer set advertised by `client` in HELLO. Re-set is
    /// idempotent (the most recent HELLO wins, matching `ColorSupport`).
    pub(super) fn set_layers(&mut self, client: ClientId, layers: LayerSet) {
        self.layers.insert(client, layers);
    }

    /// Look up the layer set advertised by `client`. Defaults to
    /// [`LayerSet::all`] for clients we never saw a HELLO from — the
    /// permissive default matches test scaffolding that skips HELLO.
    #[must_use]
    pub(super) fn layers(&self, client: ClientId) -> LayerSet {
        self.layers
            .get(&client)
            .copied()
            .unwrap_or_else(LayerSet::all)
    }

    /// `true` iff `client` has L3 in its negotiated `HELLO.layers`.
    /// Gates emission of `METADATA_CHANGED` per SPEC §16.4.
    #[must_use]
    pub(super) fn speaks_l3(&self, client: ClientId) -> bool {
        self.layers(client).contains(Layer::L3)
    }

    // -- attached clients ----------------------------------------------

    /// Update the recorded [`ClientCapabilities`] for an already-attached
    /// client. Returns `false` if the client is not in [`Self::attached`].
    pub(super) fn set_capabilities(&mut self, client: ClientId, caps: ClientCapabilities) -> bool {
        self.attached
            .get_mut(&client)
            .map(|c| {
                c.client_caps = caps;
            })
            .is_some()
    }

    /// Patch only the color tier of an already-attached client's
    /// capabilities. Returns `false` if the client is not in
    /// [`Self::attached`].
    pub(super) fn set_color_support(&mut self, client: ClientId, color: ColorSupport) -> bool {
        self.attached
            .get_mut(&client)
            .map(|c| {
                c.client_caps = c.client_caps.with_color_support(color);
            })
            .is_some()
    }

    /// Collect the `(client, outbound mailbox)` pairs of every client
    /// attached to `session`, by its stable id.
    ///
    /// Per-terminal `ATTACH_RESOURCE` consumers are not in
    /// [`Self::attached`] and are deliberately excluded.
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

    /// Remember `client`'s outbound mailbox for terminal-scoped fanout.
    ///
    /// Called from the `ATTACH_RESOURCE` handler, in the same critical
    /// section that appends `client` to the Terminal's subscriber list, so
    /// the mailbox is never missing for a client the list already names.
    /// Re-attaching overwrites with the same sender (a connection's tx is
    /// stable), so this is idempotent in practice.
    pub(super) fn remember_terminal_mailbox(
        &mut self,
        client: ClientId,
        tx: mpsc::Sender<Outbound>,
    ) {
        self.terminal_mailboxes.insert(client, tx);
    }

    /// Resolve `client`'s outbound mailbox for terminal-scoped fanout:
    /// [`Self::attached`] first, then [`Self::terminal_mailboxes`].
    ///
    /// One mailbox per client, whichever way it subscribed — the
    /// "exactly once" half of the [`Self::terminal_mailboxes`] contract.
    /// `None` for a client that is neither attached nor per-Terminal
    /// subscribed, which for a name taken from a live subscriber list means
    /// the connection is already tearing down.
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

    /// `client`'s agent-event subscription, created empty on first use.
    ///
    /// One entry per client whichever verb installed its scopes, which is
    /// what makes a client subscribed both ways receive each event once
    /// (ADR-0123). `tx` is the client's outbound mailbox, captured so event
    /// fanout reaches a pure `watch` client that never attached; an
    /// existing entry keeps its sender (a connection's tx is stable).
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

    /// Offer one journaled event to every subscription (the fan-out half
    /// of `ServerState::record_and_fanout`).
    pub(super) fn offer_event(&mut self, entry: &JournalEntry) {
        super::events::offer_to_all(self.event_subscriptions.values_mut(), entry);
    }

    /// The `ActorRef` naming `client`: its wire id, the credential it
    /// authenticated with, and the name it announced in `HELLO`.
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

    /// Drop `client`'s per-terminal agent-event subscription for `wire`
    /// (`DETACH_RESOURCE`, phux-v45.7). Server-wide subscriptions and
    /// other terminals' scopes are untouched; an empty scope set drops
    /// the whole entry so the map stays bounded.
    pub(super) fn unsubscribe_terminal_events(&mut self, client: ClientId, wire: &WireResourceId) {
        if let Some(sub) = self.event_subscriptions.get_mut(&client) {
            sub.scopes.remove(&EventScope::Terminal(wire.clone()));
            // A subscription still owed a gap stays until its pump delivers
            // it; detach drops it with the connection.
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

    /// Record the ssh origin a same-uid bridge announced in HELLO. A client
    /// with no stored identity is left alone: there is nothing to annotate.
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

    /// Remember `client`'s outbound mailbox for `METADATA_CHANGED` fanout.
    ///
    /// Called from the `SUBSCRIBE_METADATA` handler. Re-subscribing
    /// overwrites with the same sender (a connection's tx is stable), so
    /// this is idempotent in practice.
    pub(super) fn remember_metadata_mailbox(
        &mut self,
        client: ClientId,
        tx: mpsc::Sender<Outbound>,
    ) {
        self.metadata_mailboxes.insert(client, tx);
    }

    /// Fan a `MetadataChanged` frame out to every subscriber in
    /// `subscribers` that is (a) L3-capable and (b) drainable (mailbox not
    /// closed). Returns the actually-targeted client list.
    ///
    /// The mailbox is resolved from [`Self::attached`] when the subscriber
    /// attached, and otherwise from [`Self::metadata_mailboxes`], so a
    /// headless consumer that only ever sent `SUBSCRIBE_METADATA` (the
    /// `phux watch` shape — it never attaches, by design) is still reached.
    /// Before that fallback existed, every `phux.agent/v1` record the
    /// ADR-0046 detector published was computed, broadcast, and dropped for
    /// want of an `attached` entry.
    ///
    /// The subscriber list itself comes from the L3 metadata store, which
    /// stays on [`super::ServerState`]; this half is pure client-keyed
    /// fanout, which is why it lives here.
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
            // `try_send`: the mailbox is bounded (DEFAULT_CLIENT_MAILBOX)
            // and we hold the state mutex synchronously; awaiting on a
            // full mailbox would deadlock the per-client read loop. A
            // dropped notification is acceptable per SPEC §7.4 — the
            // subscriber can re-`GET_METADATA` on next attach.
            if tx.try_send(Outbound::Frame(frame)).is_ok() {
                delivered.push(*client_id);
            }
        }
        delivered
    }
}
