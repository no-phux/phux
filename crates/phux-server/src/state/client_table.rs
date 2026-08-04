//! The per-client table: every map keyed on a connected client's identity,
//! plus the monotonic allocator that mints those identities.
//!
//! Six fields that were flat on [`super::ServerState`] live here because
//! they share one lifetime — a client's connection. An entry appears when
//! the client identifies itself (HELLO, ATTACH, `SUBSCRIBE_EVENTS`, a
//! session-create submission) and every one of them disappears in
//! `ServerState::detach`. Keeping them together is what makes "forget
//! everything about this client" one place to look instead of five map
//! removals scattered across `state::client`, `state::events`,
//! `state::policy`, and `state::metadata`, which is where they drifted out
//! of step before.
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

use phux_core::ids::SessionId;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, Layer, LayerSet};
use phux_protocol::ids::TerminalId as WireTerminalId;
use phux_protocol::wire::frame::{FrameKind, Scope};
use tokio::sync::mpsc;

use super::client::{AttachedClient, ClientId};
use super::events::{EventScope, EventSubscription};
use super::input_log::Outbound;

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
    /// Per-client peer identities, keyed by server-assigned client id.
    pub(super) peer_identities: HashMap<ClientId, phux_protocol::policy::PeerIdentity>,
    /// Nonce-bearing session-create result keys owned by each connection.
    ///
    /// Results are one-shot and connection-scoped even though their transport
    /// uses Global L3 metadata. Tracking ownership lets disconnect cleanup
    /// remove abandoned results, while the per-client cap bounds a connected
    /// client that submits creates without reading replies.
    pub(super) session_create_results: HashMap<ClientId, VecDeque<String>>,
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
            peer_identities: HashMap::new(),
            session_create_results: HashMap::new(),
        }
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
    /// Per-terminal `ATTACH_TERMINAL` consumers are not in
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

    // -- agent-event subscriptions -------------------------------------

    /// Record an agent-event subscription for `client` at the scope named
    /// by `terminal` (SPEC §7.5, phux-y2t). Idempotent: re-subscribing the
    /// same scope is a no-op (the per-client scope set absorbs the
    /// duplicate). `None` maps to [`EventScope::Server`]; `Some(id)` maps
    /// to [`EventScope::Terminal`].
    ///
    /// `tx` is the client's outbound mailbox, captured here so event
    /// fanout reaches a pure `watch` client that never attached. A
    /// re-subscribe leaves the stored mailbox in place (the connection's
    /// tx is stable, so this is a no-op in practice).
    pub(super) fn subscribe_events(
        &mut self,
        client: ClientId,
        terminal: Option<WireTerminalId>,
        tx: mpsc::Sender<Outbound>,
    ) {
        let scope = terminal.map_or(EventScope::Server, EventScope::Terminal);
        let entry = self
            .event_subscriptions
            .entry(client)
            .or_insert_with(|| EventSubscription {
                tx,
                scopes: HashSet::new(),
            });
        entry.scopes.insert(scope);
    }

    /// Collect the outbound mailbox of every client subscribed to an agent
    /// event scoped to `terminal` (SPEC §7.5, phux-y2t).
    ///
    /// Resolves the mailbox from the subscription registry, NOT from
    /// [`Self::attached`], so a pure `watch` client (subscribed without an
    /// attach) is still reached.
    #[must_use]
    pub(super) fn event_targets(
        &self,
        terminal: Option<&WireTerminalId>,
    ) -> Vec<mpsc::Sender<Outbound>> {
        self.event_subscriptions
            .values()
            .filter(|sub| {
                sub.scopes.contains(&EventScope::Server)
                    || terminal
                        .is_some_and(|tid| sub.scopes.contains(&EventScope::Terminal(tid.clone())))
            })
            .map(|sub| sub.tx.clone())
            .collect()
    }

    /// Drop `client`'s per-terminal agent-event subscription for `wire`
    /// (`DETACH_TERMINAL`, phux-v45.7). Server-wide subscriptions and
    /// other terminals' scopes are untouched; an empty scope set drops
    /// the whole entry so the map stays bounded.
    pub(super) fn unsubscribe_terminal_events(&mut self, client: ClientId, wire: &WireTerminalId) {
        if let Some(sub) = self.event_subscriptions.get_mut(&client) {
            sub.scopes.remove(&EventScope::Terminal(wire.clone()));
            if sub.scopes.is_empty() {
                self.event_subscriptions.remove(&client);
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
        self.peer_identities.insert(client, identity);
    }

    /// Look up a peer identity by client id.
    #[must_use]
    pub(super) fn peer_identity(
        &self,
        client: ClientId,
    ) -> Option<&phux_protocol::policy::PeerIdentity> {
        self.peer_identities.get(&client)
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

    /// Fan a `MetadataChanged` frame out to every subscriber in
    /// `subscribers` that is (a) still attached, (b) L3-capable, and
    /// (c) drainable (mailbox not closed). Returns the actually-targeted
    /// client list.
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
    ) -> Vec<ClientId> {
        let mut delivered = Vec::with_capacity(subscribers.len());
        for client_id in subscribers {
            if !self.speaks_l3(*client_id) {
                continue;
            }
            let Some(client) = self.attached.get(client_id) else {
                continue;
            };
            let frame = FrameKind::MetadataChanged {
                scope: scope.clone(),
                key: key.to_owned(),
                value: value.map(<[u8]>::to_vec),
            };
            // `try_send`: the mailbox is bounded (DEFAULT_CLIENT_MAILBOX)
            // and we hold the state mutex synchronously; awaiting on a
            // full mailbox would deadlock the per-client read loop. A
            // dropped notification is acceptable per SPEC §7.4 — the
            // subscriber can re-`GET_METADATA` on next attach.
            if client.tx.try_send(Outbound::Frame(frame)).is_ok() {
                delivered.push(*client_id);
            }
        }
        delivered
    }
}
