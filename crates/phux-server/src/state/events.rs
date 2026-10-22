//! Agent-event subscriptions and the one fan-out path (ADR-0123,
//! `docs/protocol.md#receipts-retries-and-backpressure`).
//!
//! `SUBSCRIBE_EVENTS` (unfiltered) and `SUBSCRIBE_RESOURCE_EVENTS` (a
//! Terminal scope with a type filter) share one registry, so each client
//! has one subscription and gets an event once; the latest subscribe to a
//! scope sets its filter. Every event is stamped by the journal under the
//! state lock and offered with a non-blocking send, and a subscription's
//! `seq`s are monotone across its scopes. Work its mailbox cannot take yet
//! (a `journal_gap`, the rest of a cursor replay) stays owed and wakes the
//! connection's pump ([`ServerState::next_owed_event_frame`]).

use std::collections::HashMap;
use std::sync::Arc;

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::scope::EffectiveScopeSet;
use phux_protocol::wire::frame::{AgentEvent, FrameKind, ResourceEventType};
use tokio::sync::{Notify, mpsc};

#[cfg(doc)]
use super::ServerState;
use super::client::ClientId;
use super::journal::JournalEntry;
use crate::mailbox::Outbound;

mod fanout;
mod subscription;

/// Scope of an agent-event subscription.
///
/// [`Self::Server`] is every local event; [`Self::Terminal`] is that
/// Terminal and its children's lifecycle edges (ADR-0104 §2). A satellite
/// Terminal is only ever a Terminal scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EventScope {
    /// Every event the server emits for a local resource.
    Server,
    /// Only events concerning this Terminal.
    Terminal(WireResourceId),
}

impl EventScope {
    fn of(terminal: Option<WireResourceId>) -> Self {
        terminal.map_or(Self::Server, Self::Terminal)
    }

    /// Whether `entry` is in this scope (a relayed satellite event is never
    /// server-wide).
    fn covers(&self, entry: &JournalEntry) -> bool {
        match self {
            Self::Server => !entry.is_relayed(),
            Self::Terminal(id) => {
                entry.terminal.as_ref() == Some(id) || entry.parent.as_ref() == Some(id)
            }
        }
    }
}

/// A `SUBSCRIBE_RESOURCE_EVENTS` type filter; empty admits all. Lifecycle,
/// supervisory control, and `source_gap` always pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventFilter(Vec<ResourceEventType>);

impl EventFilter {
    /// The filter that admits every event.
    #[must_use]
    pub const fn all() -> Self {
        Self(Vec::new())
    }

    /// The filter that admits `types` (every event when empty).
    #[must_use]
    pub const fn of(types: Vec<ResourceEventType>) -> Self {
        Self(types)
    }

    fn admits(&self, event: &AgentEvent) -> bool {
        if self.0.is_empty() || bypasses_type_filter(event) {
            return true;
        }
        filter_type(event).is_some_and(|ty| self.0.contains(&ty))
    }
}

const fn bypasses_type_filter(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::TerminalControl { .. }
            | AgentEvent::ResourceSpawned { .. }
            | AgentEvent::ResourceClosed { .. }
            | AgentEvent::SourceGap { .. }
            | AgentEvent::ApprovalRequested { .. }
            | AgentEvent::ApprovalDecided { .. }
    )
}

/// The `SUBSCRIBE_RESOURCE_EVENTS` type an event is filtered as, if any.
const fn filter_type(event: &AgentEvent) -> Option<ResourceEventType> {
    match event {
        AgentEvent::CommandStarted => Some(ResourceEventType::CommandStarted),
        AgentEvent::CommandFinished { .. } => Some(ResourceEventType::CommandEnded),
        AgentEvent::CwdChanged { .. } => Some(ResourceEventType::CwdChanged),
        AgentEvent::Dirty => Some(ResourceEventType::GridChanged),
        AgentEvent::Idle => Some(ResourceEventType::OutputReceived),
        _ => None,
    }
}

/// A cursor replay still being pulled from the ring.
#[derive(Debug, Clone, Copy)]
struct PendingReplay {
    /// The last `seq` the replay has handed out (or reported missing).
    cursor: u64,
    /// A gap owed before the replay's first entry.
    leading_gap: Option<(u64, u64)>,
}

/// What a connection's event pump does next.
#[derive(Debug)]
pub enum PumpStep {
    /// Send this frame with the mailbox slot the pump holds.
    Frame(FrameKind),
    /// Nothing is owed; wait to be woken.
    Idle,
    /// The subscription is gone (detach); the pump exits.
    Gone,
}

/// The wake and mailbox a connection's event pump runs on.
#[derive(Debug)]
pub struct EventPump {
    /// Notified whenever the subscription is owed a gap or replay frames.
    pub wake: Arc<Notify>,
    /// The connection's outbound mailbox.
    pub tx: mpsc::Sender<Outbound>,
    /// The subscription served; a later one for the same client has a new
    /// epoch, so an outlived pump is told it is gone.
    pub epoch: u64,
}

/// A hub consumer's satellite scope before a subscribe, so a refused
/// subscribe can restore it ([`ServerState::restore_satellite_scope`]).
#[derive(Debug)]
pub struct SatelliteScopeChange {
    terminal: WireResourceId,
    prior: Option<EventFilter>,
}

/// One client's event subscription: mailbox (a `watch` client never
/// attaches), scopes, and delivery state.
#[derive(Debug)]
pub struct EventSubscription {
    /// The client's outbound mailbox.
    pub(crate) tx: mpsc::Sender<Outbound>,
    /// Scopes this client watches, each with its type filter.
    pub(crate) scopes: HashMap<EventScope, EventFilter>,
    /// Opened with `after_seq`, proving it decodes this draft (L1 §7.1).
    journal_aware: bool,
    /// Sequences this subscription missed and has not yet been told about.
    gap: Option<(u64, u64)>,
    /// A cursor replay still owed; live events wait for it so the stream
    /// stays monotone.
    replay: Option<PendingReplay>,
    /// Highest `seq` given or reported missing; nothing at or below recurs.
    delivered: u64,
    /// Woken when the subscription is owed work its mailbox could not take.
    wake: Arc<Notify>,
    /// A pump has been started for this subscription.
    pump_claimed: bool,
    /// Distinguishes this subscription from others for the same client.
    epoch: u64,
    /// A scoped grant's clauses: only events it may `OBSERVE` are delivered
    /// or replayed (workload-auth §6). `None` for the owner's grant.
    observe: Option<EffectiveScopeSet>,
}

/// The per-subscription `journal_gap` notice (never journaled).
#[must_use]
pub const fn journal_gap_frame(first_missing: u64, last_missing: u64) -> FrameKind {
    FrameKind::Event {
        terminal: None,
        event: AgentEvent::JournalGap {
            first_missing,
            last_missing,
        },
        stamp: None,
    }
}

/// Deliver a journaled entry to every subscription.
pub(super) fn offer_to_all<'a>(
    subscriptions: impl Iterator<Item = &'a mut EventSubscription>,
    entry: &JournalEntry,
) {
    for sub in subscriptions {
        sub.offer(entry);
    }
}

#[cfg(test)]
mod tests;
