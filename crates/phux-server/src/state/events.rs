use std::collections::HashSet;

use phux_protocol::ids::TerminalId as WireTerminalId;
use tokio::sync::mpsc;

use super::ServerState;
use super::client::ClientId;
use super::input_log::Outbound;

/// Scope of an agent-event subscription (SPEC §7.5, phux-y2t).
///
/// A client subscribes with [`Self::Server`] (every event the server
/// emits, including server-scoped events with no owning Terminal) or
/// [`Self::Terminal`] (only that Terminal's events). The two are stored
/// in a per-client `HashSet`, so a client MAY watch the whole server and
/// a specific pane simultaneously — fan-out de-duplicates by client.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EventScope {
    /// Every event the server emits for any Terminal the client may
    /// observe, plus server-scoped events (`terminal: None`).
    Server,
    /// Only events concerning this specific Terminal.
    Terminal(WireTerminalId),
}

/// One client's agent-event subscription (SPEC §7.5, phux-y2t): its
/// outbound mailbox plus the set of scopes it watches.
///
/// The mailbox lives here so event fanout works for a pure `watch` client
/// that subscribed without attaching (no [`super::AttachedClient`] entry exists
/// for it). An attached client that also subscribes stores its same
/// mailbox here — fanout de-duplicates by [`super::ClientId`].
#[derive(Debug)]
pub struct EventSubscription {
    /// The client's outbound mailbox (the per-connection writer task
    /// drains it). Best-effort `try_send` target for `EVENT` frames.
    pub(crate) tx: mpsc::Sender<Outbound>,
    /// Scopes this client watches.
    pub(crate) scopes: HashSet<EventScope>,
}

impl ServerState {
    /// Record an agent-event subscription for `client_id` at `scope`
    /// (SPEC §7.5, phux-y2t). Idempotent: re-subscribing the same scope
    /// is a no-op (the per-client scope set absorbs the duplicate). A
    /// `terminal: None` `SUBSCRIBE_EVENTS` maps to [`EventScope::Server`];
    /// a `Some(id)` maps to [`EventScope::Terminal`].
    ///
    /// `tx` is the client's outbound mailbox, captured here so event
    /// fanout reaches a pure `watch` client that never attached. A
    /// re-subscribe leaves the stored mailbox in place (the connection's
    /// tx is stable, so this is a no-op in practice).
    pub fn subscribe_events(
        &mut self,
        client_id: ClientId,
        terminal: Option<WireTerminalId>,
        tx: mpsc::Sender<Outbound>,
    ) {
        self.clients.subscribe_events(client_id, terminal, tx);
    }

    /// Collect the outbound mailbox of every client subscribed to an agent
    /// event scoped to `terminal` (SPEC §7.5, phux-y2t).
    ///
    /// A client receives the event when it subscribed [`EventScope::Server`]
    /// (server-wide) OR, when `terminal` is `Some(id)`, it subscribed
    /// [`EventScope::Terminal`] for that same id. A server-scoped event
    /// (`terminal == None`) reaches only the server-wide subscribers — it
    /// has no single owning Terminal to match a per-pane subscription.
    /// Order is unspecified; callers MUST NOT rely on it. Resolves the
    /// mailbox from the subscription registry, NOT from
    /// [`Self::attached`], so a pure `watch` client (subscribed without an
    /// attach) is still reached.
    #[must_use]
    pub fn event_targets(&self, terminal: Option<&WireTerminalId>) -> Vec<mpsc::Sender<Outbound>> {
        self.clients.event_targets(terminal)
    }

    /// Drop `client`'s per-terminal agent-event subscription for `wire`
    /// (`DETACH_TERMINAL`, phux-v45.7). Server-wide subscriptions and
    /// other terminals' scopes are untouched; an empty scope set drops
    /// the whole entry so the map stays bounded.
    pub fn unsubscribe_terminal_events(&mut self, client: ClientId, wire: &WireTerminalId) {
        self.clients.unsubscribe_terminal_events(client, wire);
    }
}
