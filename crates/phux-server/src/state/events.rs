//! Agent-event subscriptions and the one fan-out path (ADR-0123,
//! `docs/spec/L1.md` §7.3).
//!
//! Both subscribe verbs land in one registry: `SUBSCRIBE_EVENTS` installs an
//! unfiltered scope, and `SUBSCRIBE_RESOURCE_EVENTS` installs a Terminal
//! scope with a type filter. A client has one subscription however many
//! scopes it holds, so an event reaches it once, and the latest subscription
//! to a scope sets that scope's filter.
//!
//! Every event goes through [`ServerState::record_and_fanout`] (or, for an
//! event a federation hub relays, [`ServerState::record_relayed_event`]):
//! stamped by the journal under the state lock, then offered to each
//! subscription with a non-blocking send. Nothing here ever waits on a
//! mailbox. A subscription never receives a `seq` at or below the highest it
//! was already given, so its stream is monotone across all its scopes.
//!
//! Work a subscription is owed but its mailbox cannot take right now, a
//! `journal_gap` for events it missed or the rest of a cursor replay, stays
//! on the subscription and wakes its pump: the connection task that waits
//! for mailbox room and pulls the next owed frame
//! ([`ServerState::next_owed_event_frame`]). A replay is pulled from the
//! ring a frame at a time, so a resuming consumer on a quiet server receives
//! everything it missed, in order, and an owed gap is delivered as soon as
//! the consumer reads, without waiting for another event.

use std::collections::HashMap;
use std::sync::Arc;

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{
    AgentEvent, ControlAction, EventStamp, FrameKind, ResourceEventType,
};
use tokio::sync::{Notify, mpsc};

use super::ServerState;
use super::client::ClientId;
use super::journal::{EventRecord, Journal, JournalEntry, NextEntry, now_unix_ms};
use crate::mailbox::Outbound;

/// Scope of an agent-event subscription.
///
/// A client subscribes with [`Self::Server`] (every event the server emits
/// for a local resource, including server-scoped events with no owning
/// Terminal) or [`Self::Terminal`] (that Terminal's events, and the
/// lifecycle edges of its children, ADR-0104 §2). A satellite Terminal on a
/// federation hub is only ever a [`Self::Terminal`] scope.
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

    /// Whether an event recorded as `entry` is in this scope. A relayed
    /// satellite event is not a server-wide one: it reaches only the
    /// consumers that subscribed to its satellite Terminal.
    fn covers(&self, entry: &JournalEntry) -> bool {
        match self {
            Self::Server => !entry.is_relayed(),
            Self::Terminal(id) => {
                entry.terminal.as_ref() == Some(id) || entry.parent.as_ref() == Some(id)
            }
        }
    }
}

/// A `SUBSCRIBE_RESOURCE_EVENTS` type filter. Empty admits every event.
///
/// Lifecycle edges, supervisory control, and `source_gap` bypass a
/// non-empty filter: they are not grid activity, and a loss notice that a
/// filter could hide would be a silent loss.
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
    /// A gap owed ahead of the replay's first entry: a void cursor, or a
    /// cursor below what this subscription already received.
    leading_gap: Option<(u64, u64)>,
}

/// What a connection's event pump does next
/// ([`ServerState::next_owed_event_frame`]).
#[derive(Debug)]
pub enum PumpStep {
    /// Send this frame with the mailbox slot the pump holds.
    Frame(FrameKind),
    /// Nothing is owed; wait to be woken.
    Idle,
    /// The subscription is gone (detach); the pump exits.
    Gone,
}

/// The handles a connection's event pump runs on: the wake the
/// subscription notifies when it is owed work, and the mailbox the pump
/// waits for room in.
#[derive(Debug)]
pub struct EventPump {
    /// Notified whenever the subscription is owed a gap or replay frames.
    pub wake: Arc<Notify>,
    /// The connection's outbound mailbox.
    pub tx: mpsc::Sender<Outbound>,
    /// The subscription this pump serves. A later subscription for the
    /// same client (after a detach retired this one) has another, so a
    /// pump that outlived its subscription is told it is gone rather than
    /// serving the new one.
    pub epoch: u64,
}

/// What a hub consumer's satellite scope held before a subscribe changed
/// it ([`ServerState::subscribe_satellite_events`]), so a subscribe the
/// satellite refused restores it exactly
/// ([`ServerState::restore_satellite_scope`]).
#[derive(Debug)]
pub struct SatelliteScopeChange {
    terminal: WireResourceId,
    prior: Option<EventFilter>,
}

/// One client's agent-event subscription: its mailbox, its scopes, and its
/// delivery state.
///
/// The mailbox lives here so fan-out reaches a pure `watch` client that
/// subscribed without attaching.
#[derive(Debug)]
pub struct EventSubscription {
    /// The client's outbound mailbox.
    pub(crate) tx: mpsc::Sender<Outbound>,
    /// Scopes this client watches, each with its type filter.
    pub(crate) scopes: HashMap<EventScope, EventFilter>,
    /// The client opened a scope with `after_seq`, which proves it decodes
    /// every value of this protocol draft (L1 §7.1 `EXPIRED`).
    journal_aware: bool,
    /// Sequences this subscription missed and has not yet been told about.
    gap: Option<(u64, u64)>,
    /// A cursor replay still owed from the ring. While it is pending, live
    /// events are not offered directly: the replay reaches them in the ring,
    /// which keeps the stream monotone.
    replay: Option<PendingReplay>,
    /// The highest `seq` this subscription has been given or told it
    /// missed. Nothing at or below it is ever delivered again.
    delivered: u64,
    /// Woken when the subscription is owed work its mailbox could not take.
    wake: Arc<Notify>,
    /// A pump has been started for this subscription.
    pump_claimed: bool,
    /// Tells this subscription apart from an earlier or later one for the
    /// same client ([`EventPump::epoch`]).
    epoch: u64,
}

impl EventSubscription {
    pub(super) fn new(tx: mpsc::Sender<Outbound>, epoch: u64) -> Self {
        Self {
            tx,
            scopes: HashMap::new(),
            journal_aware: false,
            gap: None,
            replay: None,
            delivered: 0,
            wake: Arc::new(Notify::new()),
            pump_claimed: false,
            epoch,
        }
    }

    /// Wake the pump so it notices the subscription is gone.
    pub(super) fn retire(&self) {
        self.wake.notify_one();
    }

    /// Whether any of this subscription's scopes admits `entry`.
    fn admits(&self, entry: &JournalEntry) -> bool {
        self.scopes
            .iter()
            .any(|(scope, filter)| scope.covers(entry) && filter.admits(&entry.event))
    }

    /// The newest `seq` this subscription has been sent or is owed: what it
    /// delivered, a gap it owes, and a pending replay's leading gap. A
    /// satellite scope's cursor is owed a gap its ring holds nothing for.
    fn reach(&self) -> u64 {
        let last = |gap: Option<(u64, u64)>| gap.map_or(0, |(_, last)| last);
        self.delivered
            .max(last(self.gap))
            .max(last(self.replay.and_then(|replay| replay.leading_gap)))
    }

    /// The newest evicted `seq` while this subscription's replay is still
    /// pending below it, so a `journal_gap` reaching it will be sent; `0`
    /// otherwise, since a later eviction is never reported to it.
    fn owed_eviction(&self, journal: &Journal) -> u64 {
        let evicted = journal.evicted_through();
        match self.replay {
            Some(replay) if replay.cursor.max(self.delivered) < evicted => evicted,
            _ => 0,
        }
    }

    /// The frame this subscription receives for `entry`.
    fn frame_for(&self, entry: &JournalEntry) -> FrameKind {
        entry.frame_with(render_for(&entry.event, self.journal_aware))
    }

    /// The live fan-out step: deliver `entry` if it is in scope and newer
    /// than anything this subscription was given, telling it about any
    /// earlier loss first.
    fn offer(&mut self, entry: &JournalEntry) {
        if entry.seq() <= self.delivered {
            return;
        }
        let admitted = self.admits(entry);
        if self.replay.is_some() {
            // A retained event is pulled from the ring by the replay; a
            // relayed one is not retained, so the replay cannot reach it.
            if admitted && entry.is_relayed() {
                self.owe_gap(entry.seq(), entry.seq());
            }
            return;
        }
        if !admitted {
            let _ = self.flush_gap();
            return;
        }
        if !self.flush_gap() || !self.try_send(self.frame_for(entry)) {
            self.owe_gap(entry.seq(), entry.seq());
            return;
        }
        self.delivered = entry.seq();
    }

    /// Queue what is owed while the mailbox has room, and wake the pump
    /// for the rest.
    fn fill(&mut self, journal: &Journal) {
        let tx = self.tx.clone();
        while let Ok(permit) = tx.try_reserve() {
            let Some(frame) = self.next_owed(journal) else {
                return;
            };
            permit.send(Outbound::Frame(frame));
        }
        if self.is_owed_work() {
            self.wake.notify_one();
        }
    }

    pub(super) const fn is_owed_work(&self) -> bool {
        self.gap.is_some() || self.replay.is_some()
    }

    /// The next owed frame: replay first (its gaps and entries, in `seq`
    /// order), then any gap owed for live events.
    fn next_owed(&mut self, journal: &Journal) -> Option<FrameKind> {
        if self.replay.is_some() {
            return self.next_replay_frame(journal);
        }
        self.take_gap_frame()
    }

    /// The next replay frame. Nothing at or below what was already
    /// delivered or reported missing is replayed, and a gap owed meanwhile
    /// (a relayed event, an interrupted scope) goes out before any later
    /// `seq`, so the stream stays monotone.
    fn next_replay_frame(&mut self, journal: &Journal) -> Option<FrameKind> {
        let replay = self.replay?;
        if let Some((first, last)) = replay.leading_gap {
            self.replay = Some(PendingReplay {
                cursor: replay.cursor.max(last),
                leading_gap: None,
            });
            return Some(self.report_gap(first, last));
        }
        let next = journal.next_after(replay.cursor.max(self.delivered), |entry| {
            self.admits(entry)
        });
        if self.owed_gap_precedes(&next) {
            return self.take_gap_frame();
        }
        match next {
            NextEntry::Gap(first, last) => {
                self.advance_replay(last);
                Some(self.report_gap(first, last))
            }
            NextEntry::Entry(entry) => {
                self.advance_replay(entry.seq());
                self.delivered = self.delivered.max(entry.seq());
                Some(self.frame_for(&entry))
            }
            NextEntry::CaughtUp => {
                self.replay = None;
                self.take_gap_frame()
            }
        }
    }

    /// Whether the gap owed for live events starts before the replay's
    /// next frame, and so must be sent first.
    fn owed_gap_precedes(&self, next: &NextEntry) -> bool {
        let Some((owed_first, _)) = self.gap else {
            return false;
        };
        match next {
            NextEntry::Gap(first, _) => *first > owed_first,
            NextEntry::Entry(entry) => entry.seq() > owed_first,
            NextEntry::CaughtUp => false,
        }
    }

    fn advance_replay(&mut self, cursor: u64) {
        if let Some(replay) = self.replay.as_mut() {
            replay.cursor = cursor;
        }
    }

    /// The `journal_gap` notice for `first..=last`, now reported.
    fn report_gap(&mut self, first: u64, last: u64) -> FrameKind {
        self.delivered = self.delivered.max(last);
        journal_gap_frame(first, last)
    }

    fn take_gap_frame(&mut self) -> Option<FrameKind> {
        let (first, last) = self.gap.take()?;
        Some(self.report_gap(first, last))
    }

    /// Send the pending `journal_gap`, if any. `false` when one is still
    /// owed.
    fn flush_gap(&mut self) -> bool {
        let Some((first, last)) = self.gap else {
            return true;
        };
        let sent = self.try_send(journal_gap_frame(first, last));
        if sent {
            self.gap = None;
            self.delivered = self.delivered.max(last);
        }
        sent
    }

    fn try_send(&self, frame: FrameKind) -> bool {
        self.tx.try_send(Outbound::Frame(frame)).is_ok()
    }

    /// Owe a `journal_gap` covering `first..=last`, and wake the pump so it
    /// is delivered as soon as the mailbox has room.
    fn owe_gap(&mut self, first: u64, last: u64) {
        self.gap = Some(match self.gap {
            Some((prior_first, prior_last)) => (prior_first.min(first), prior_last.max(last)),
            None => (first, last),
        });
        self.wake.notify_one();
    }

    /// Start a cursor replay after `after_seq` (L1 §7.3), without replaying
    /// anything this subscription was already given.
    fn begin_replay(&mut self, after_seq: u64, head: u64) {
        let start = replay_start(after_seq, head, self.delivered);
        let Some((cursor, leading_gap)) = start else {
            return;
        };
        self.replay = Some(match self.replay {
            // One replay at a time: an already-pending one keeps its
            // position, which is past everything this one could name.
            Some(pending) => PendingReplay {
                leading_gap: merge_gap(pending.leading_gap, leading_gap),
                ..pending
            },
            None => PendingReplay {
                cursor,
                leading_gap,
            },
        });
    }
}

/// Where a replay after `after_seq` starts, given the journal's `head` and
/// what the subscription already received (`delivered`): its cursor and
/// the gap it leads with. `None` when nothing is owed (the no-replay
/// sentinel, or a cursor at the head).
fn replay_start(after_seq: u64, head: u64, delivered: u64) -> Option<(u64, Option<(u64, u64)>)> {
    if after_seq == u64::MAX || (after_seq == head && delivered <= head) {
        return None;
    }
    if after_seq > head {
        // Void: never issued by this incarnation (L1 §7.3).
        return Some((head.max(delivered), Some((1, head))));
    }
    if after_seq < delivered {
        // Already given everything up to `delivered` through another scope;
        // what this scope missed below it cannot be told apart.
        return Some((delivered, Some((after_seq + 1, delivered))));
    }
    Some((after_seq, None))
}

fn merge_gap(a: Option<(u64, u64)>, b: Option<(u64, u64)>) -> Option<(u64, u64)> {
    match (a, b) {
        (Some((a_first, a_last)), Some((b_first, b_last))) => {
            Some((a_first.min(b_first), a_last.max(b_last)))
        }
        (one, other) => one.or(other),
    }
}

/// How `event` reads to a subscriber: an `EXPIRED` lease reaches a
/// subscription that never sent a cursor as `RELEASED` with no actor,
/// because a decoder from before this draft fails the frame on the new
/// value (L1 §7.1).
fn render_for(event: &AgentEvent, journal_aware: bool) -> AgentEvent {
    match event {
        AgentEvent::TerminalControl {
            lifecycle,
            exit_status,
            input_holder,
            action: ControlAction::Expired,
            ..
        } if !journal_aware => AgentEvent::TerminalControl {
            lifecycle: *lifecycle,
            exit_status: *exit_status,
            input_holder: *input_holder,
            action: ControlAction::Released,
            actor: None,
        },
        other => other.clone(),
    }
}

/// The per-subscription `journal_gap` notice: never journaled, never
/// stamped.
#[must_use]
pub fn journal_gap_frame(first_missing: u64, last_missing: u64) -> FrameKind {
    FrameKind::Event {
        terminal: None,
        event: AgentEvent::JournalGap {
            first_missing,
            last_missing,
        },
        stamp: None,
    }
}

impl ServerState {
    /// Stamp `record` in the journal and offer it to every subscription.
    ///
    /// The one emission path: every event the server originates goes
    /// through here, under the state lock, so a snapshot cut in the same
    /// lock and the event order agree. Returns the assigned `seq`, or
    /// `None` in the unreachable case of an exhausted sequence.
    pub fn record_and_fanout(&mut self, record: EventRecord) -> Option<u64> {
        let actor = record.actor.map(|client| self.clients.actor_ref(client));
        let entry = self.journal.record(record, actor, now_unix_ms())?;
        self.clients.offer_event(&entry);
        Some(entry.seq())
    }

    /// Stamp and deliver an event a federation hub relays from a satellite
    /// (L1 §7.3): it takes this server's next `seq`, so a consumer's cursor
    /// stays hub-scoped, and reaches the subscriptions on its satellite
    /// Terminal through the same registry, gap tracking included.
    ///
    /// The satellite's time and operation cross the hub; its actor does
    /// not, because it names a connection on the satellite. The actor of a
    /// relayed event is the hub's link, which has no client id here, so the
    /// event carries none. The event is not retained: a cursor on a
    /// satellite scope is answered with a gap
    /// ([`Self::subscribe_satellite_events`]).
    pub fn record_relayed_event(
        &mut self,
        terminal: WireResourceId,
        event: AgentEvent,
        satellite: Option<&EventStamp>,
    ) -> Option<u64> {
        let seq = self.journal.allocate_seq()?;
        let ts_ms = satellite.map_or_else(now_unix_ms, |stamp| stamp.ts_ms);
        let stamp = EventStamp::new(seq, ts_ms)
            .with_operation_id(satellite.and_then(|stamp| stamp.operation_id));
        let entry = JournalEntry::relayed(terminal, event, stamp);
        self.clients.offer_event(&entry);
        Some(seq)
    }

    /// The newest journal `seq`, `0` before the first event.
    #[must_use]
    pub const fn journal_head(&self) -> u64 {
        self.journal.head()
    }

    /// The journal head `GET_STATE` reports to `client` (L1 §7.3): on a
    /// connection that holds event subscriptions, every `seq` their replay
    /// delivers, gaps included (the newest retained entry they admit, what
    /// they were sent or are owed, and the newest evicted `seq` only while
    /// a replay is pending below it), so the consumer's catch-up always
    /// completes and an event journaled on another scope after the
    /// subscribe, or the eviction it causes, never holds it open; else the
    /// global head.
    #[must_use]
    pub fn journal_head_for(&self, client: Option<ClientId>) -> u64 {
        match client.and_then(|id| self.clients.event_subscriptions.get(&id)) {
            Some(sub) if !sub.scopes.is_empty() => self
                .journal
                .newest_admitted(|entry| sub.admits(entry))
                .max(sub.reach())
                .max(sub.owed_eviction(&self.journal)),
            _ => self.journal.head(),
        }
    }

    /// Re-bound the journal (`defaults.event-journal-entries` /
    /// `defaults.event-journal-bytes`).
    pub fn set_event_journal_bounds(&mut self, entries: usize, bytes: usize) {
        self.journal.set_bounds(entries, bytes);
    }

    /// Install a live `SUBSCRIBE_EVENTS` scope for `client_id` (`None` is
    /// server-wide). The latest subscription to a scope sets its filter, so
    /// this clears one a `SUBSCRIBE_RESOURCE_EVENTS` put there;
    /// re-subscribing an unfiltered scope is a no-op.
    ///
    /// `tx` is the client's outbound mailbox, captured so fan-out reaches a
    /// pure `watch` client that never attached.
    pub fn subscribe_events(
        &mut self,
        client_id: ClientId,
        terminal: Option<WireResourceId>,
        tx: mpsc::Sender<Outbound>,
    ) {
        self.clients
            .event_subscription(client_id, tx)
            .scopes
            .insert(EventScope::of(terminal), EventFilter::all());
    }

    /// Install `SUBSCRIBE_RESOURCE_EVENTS`: the Terminal scope for
    /// `terminal` with `filter`, replacing any filter that scope had.
    pub fn subscribe_resource_events(
        &mut self,
        client_id: ClientId,
        terminal: WireResourceId,
        filter: EventFilter,
        tx: mpsc::Sender<Outbound>,
    ) {
        self.clients
            .event_subscription(client_id, tx)
            .scopes
            .insert(EventScope::Terminal(terminal), filter);
    }

    /// Install a cursor subscription and start its replay in one step (L1
    /// §7.3): the scope is live in the registry, and every retained event
    /// after `after_seq` is owed to it in `seq` order before anything
    /// later. What the mailbox can take is queued now; the connection's
    /// event pump pulls the rest from the ring as the consumer reads, so
    /// nothing here waits and a slow reader delays only itself.
    pub fn subscribe_events_after(
        &mut self,
        client_id: ClientId,
        terminal: Option<WireResourceId>,
        after_seq: u64,
        tx: mpsc::Sender<Outbound>,
    ) {
        let head = self.journal.head();
        let sub = self.clients.event_subscription(client_id, tx);
        sub.scopes
            .insert(EventScope::of(terminal), EventFilter::all());
        sub.journal_aware = true;
        sub.begin_replay(after_seq, head);
        sub.fill(&self.journal);
    }

    /// Install a hub consumer's subscription to a satellite Terminal: the
    /// scope relayed events for it are delivered through (L1 §7.3). A
    /// cursor on it cannot be replayed, because relayed events are not
    /// retained, so a cursor that misses anything is owed a gap.
    ///
    /// Returns what the scope held before, for
    /// [`Self::restore_satellite_scope`] when the satellite refuses.
    pub fn subscribe_satellite_events(
        &mut self,
        client_id: ClientId,
        terminal: WireResourceId,
        filter: EventFilter,
        after_seq: Option<u64>,
        tx: mpsc::Sender<Outbound>,
    ) -> SatelliteScopeChange {
        let gap = after_seq.and_then(|after| self.journal.gap_without_replay(after));
        let sub = self.clients.event_subscription(client_id, tx);
        let prior = sub
            .scopes
            .insert(EventScope::Terminal(terminal.clone()), filter);
        sub.journal_aware |= after_seq.is_some();
        if let Some((first, last)) = gap {
            sub.owe_gap(first, last);
            sub.fill(&self.journal);
        }
        SatelliteScopeChange { terminal, prior }
    }

    /// Undo a satellite subscribe the satellite refused: put back the
    /// filter the scope had, or remove the scope if it had none. An
    /// established scope stays established, and a subscription still owed
    /// a gap is kept until it is delivered.
    pub fn restore_satellite_scope(&mut self, client_id: ClientId, change: SatelliteScopeChange) {
        let SatelliteScopeChange { terminal, prior } = change;
        let Some(prior) = prior else {
            self.clients
                .unsubscribe_terminal_events(client_id, &terminal);
            return;
        };
        if let Some(sub) = self.clients.event_subscriptions.get_mut(&client_id) {
            sub.scopes.insert(EventScope::Terminal(terminal), prior);
        }
    }

    /// End a hub consumer's satellite scope because its link went away:
    /// the stream it was on is interrupted, so the consumer is owed a gap
    /// (L1 §7.3) and re-reads level state. The gap names a `seq` taken for
    /// the purpose, so it never collides with an event anyone received.
    ///
    /// The subscription itself stays, even with no scope left, so the gap
    /// is still owed and delivered when the consumer reads; detach drops it.
    pub fn interrupt_satellite_scope(&mut self, client_id: ClientId, terminal: &WireResourceId) {
        let scope = EventScope::Terminal(terminal.clone());
        let Some(sub) = self.clients.event_subscriptions.get_mut(&client_id) else {
            return;
        };
        if sub.scopes.remove(&scope).is_none() {
            return;
        }
        let Some(seq) = self.journal.allocate_seq() else {
            return;
        };
        sub.owe_gap(seq, seq);
        sub.fill(&self.journal);
    }

    /// Claim the event pump for `client_id`'s subscription, once: the
    /// runtime spawns one task per subscription that pulls owed frames as
    /// the mailbox frees ([`Self::next_owed_event_frame`]). `None` when
    /// there is no subscription or its pump already runs.
    pub fn claim_event_pump(&mut self, client_id: ClientId) -> Option<EventPump> {
        let sub = self.clients.event_subscriptions.get_mut(&client_id)?;
        if sub.pump_claimed {
            return None;
        }
        sub.pump_claimed = true;
        let pump = EventPump {
            wake: Arc::clone(&sub.wake),
            tx: sub.tx.clone(),
            epoch: sub.epoch,
        };
        if sub.is_owed_work() {
            pump.wake.notify_one();
        }
        Some(pump)
    }

    /// The next frame the pump for `client_id`'s subscription `epoch`
    /// should send with the mailbox slot it holds: an owed gap or replay
    /// frame, [`PumpStep::Idle`] when nothing is owed, or
    /// [`PumpStep::Gone`] once that subscription ended, including when a
    /// later one for the same client replaced it (that one has its own
    /// pump).
    pub fn next_owed_event_frame(&mut self, client_id: ClientId, epoch: u64) -> PumpStep {
        let Some(sub) = self.clients.event_subscriptions.get_mut(&client_id) else {
            return PumpStep::Gone;
        };
        if sub.epoch != epoch {
            return PumpStep::Gone;
        }
        sub.next_owed(&self.journal)
            .map_or(PumpStep::Idle, PumpStep::Frame)
    }

    /// Drop `client`'s per-terminal agent-event subscription for `wire`
    /// (`DETACH_RESOURCE`, phux-v45.7). Server-wide subscriptions and
    /// other terminals' scopes are untouched; an empty scope set drops
    /// the whole entry so the map stays bounded.
    pub fn unsubscribe_terminal_events(&mut self, client: ClientId, wire: &WireResourceId) {
        self.clients.unsubscribe_terminal_events(client, wire);
    }

    /// Remember the `HELLO.client_name` `client_id` announced, the label
    /// its [`phux_protocol::wire::frame::ActorRef`] carries.
    pub fn set_client_name(&mut self, client_id: ClientId, name: String) {
        self.clients.client_names.insert(client_id, name);
    }
}

/// Deliver a journaled entry to every subscription (the registry half of
/// [`ServerState::record_and_fanout`]).
pub(super) fn offer_to_all<'a>(
    subscriptions: impl Iterator<Item = &'a mut EventSubscription>,
    entry: &JournalEntry,
) {
    for sub in subscriptions {
        sub.offer(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::journal::EventRecord;

    fn pane(id: u32) -> WireResourceId {
        WireResourceId::local(id)
    }

    fn drain(rx: &mut mpsc::Receiver<Outbound>) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        while let Ok(Outbound::Frame(FrameKind::Event { event, .. })) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    /// Everything the pump would send, as if the consumer read as fast as
    /// it was given frames.
    fn pump_all(state: &mut ServerState, client: ClientId) -> Vec<AgentEvent> {
        pump_stamped(state, client)
            .into_iter()
            .map(|(_, event)| event)
            .collect()
    }

    /// What the pump of `client`'s current subscription would send, with
    /// each frame's `seq`.
    fn pump_stamped(state: &mut ServerState, client: ClientId) -> Vec<(Option<u64>, AgentEvent)> {
        let epoch = epoch(state, client);
        let mut events = Vec::new();
        while let PumpStep::Frame(FrameKind::Event { event, stamp, .. }) =
            state.next_owed_event_frame(client, epoch)
        {
            events.push((stamp.map(|stamp| stamp.seq), event));
        }
        events
    }

    fn drain_stamped(rx: &mut mpsc::Receiver<Outbound>) -> Vec<(Option<u64>, AgentEvent)> {
        let mut events = Vec::new();
        while let Ok(Outbound::Frame(FrameKind::Event { event, stamp, .. })) = rx.try_recv() {
            events.push((stamp.map(|stamp| stamp.seq), event));
        }
        events
    }

    fn epoch(state: &ServerState, client: ClientId) -> u64 {
        state
            .clients
            .event_subscriptions
            .get(&client)
            .map_or(0, |sub| sub.epoch)
    }

    fn record(state: &mut ServerState, terminal: u32, event: AgentEvent) -> u64 {
        state
            .record_and_fanout(EventRecord::new(Some(pane(terminal)), event))
            .expect("stamped")
    }

    fn gap(first_missing: u64, last_missing: u64) -> AgentEvent {
        AgentEvent::JournalGap {
            first_missing,
            last_missing,
        }
    }

    #[test]
    fn a_client_subscribed_both_ways_receives_each_event_once() {
        let mut state = ServerState::new();
        let client = state.new_client_id();
        let (tx, mut rx) = mpsc::channel(16);
        state.subscribe_events(client, Some(pane(1)), tx.clone());
        state.subscribe_resource_events(client, pane(1), EventFilter::all(), tx.clone());
        state.subscribe_events(client, None, tx);
        let _ = record(&mut state, 1, AgentEvent::Bell);
        assert_eq!(drain(&mut rx), vec![AgentEvent::Bell]);
    }

    #[test]
    fn resubscribing_a_scope_replaces_its_filter() {
        let mut state = ServerState::new();
        let client = state.new_client_id();
        let (tx, mut rx) = mpsc::channel(16);
        let cwd_only = EventFilter::of(vec![ResourceEventType::CwdChanged]);
        state.subscribe_resource_events(client, pane(1), cwd_only, tx.clone());
        let _ = record(&mut state, 1, AgentEvent::Bell);
        assert!(drain(&mut rx).is_empty(), "a bell is not a cwd change");
        state.subscribe_resource_events(client, pane(1), EventFilter::all(), tx);
        let _ = record(&mut state, 1, AgentEvent::Bell);
        assert_eq!(drain(&mut rx), vec![AgentEvent::Bell]);
    }

    #[test]
    fn subscribe_events_resets_a_resource_events_filter() {
        let mut state = ServerState::new();
        let client = state.new_client_id();
        let (tx, mut rx) = mpsc::channel(16);
        let cwd_only = EventFilter::of(vec![ResourceEventType::CwdChanged]);
        state.subscribe_resource_events(client, pane(1), cwd_only, tx.clone());
        state.subscribe_events(client, Some(pane(1)), tx);
        let _ = record(&mut state, 1, AgentEvent::Bell);
        assert_eq!(drain(&mut rx), vec![AgentEvent::Bell], "latest wins");
    }

    #[test]
    fn a_filter_never_hides_lifecycle_control_or_a_source_gap() {
        let filter = EventFilter::of(vec![ResourceEventType::CwdChanged]);
        assert!(filter.admits(&AgentEvent::ResourceClosed { exit_status: None }));
        assert!(filter.admits(&AgentEvent::SourceGap { dropped: 1 }));
        assert!(!filter.admits(&AgentEvent::Idle));
    }

    #[test]
    fn a_full_mailbox_owes_a_gap_the_pump_delivers_without_a_later_event() {
        let mut state = ServerState::new();
        let client = state.new_client_id();
        let (tx, mut rx) = mpsc::channel(1);
        state.subscribe_events(client, None, tx);
        let _ = record(&mut state, 1, AgentEvent::Dirty);
        let lost_a = record(&mut state, 1, AgentEvent::Idle);
        let lost_b = record(&mut state, 1, AgentEvent::Bell);
        assert_eq!(
            drain(&mut rx),
            vec![AgentEvent::Dirty],
            "only the first fit"
        );
        // The server goes quiet; the reader drained its mailbox, and the pump
        // hands it the owed gap with no further event journaled.
        assert_eq!(pump_all(&mut state, client), vec![gap(lost_a, lost_b)]);
        let epoch = epoch(&state, client);
        assert!(matches!(
            state.next_owed_event_frame(client, epoch),
            PumpStep::Idle
        ));
    }

    #[test]
    fn a_cursor_replay_is_queued_in_the_installing_lock_and_goes_live_at_once() {
        let mut state = ServerState::new();
        let _ = record(&mut state, 1, AgentEvent::Bell);
        let _ = record(&mut state, 2, AgentEvent::Bell);
        let client = state.new_client_id();
        let (tx, mut rx) = mpsc::channel(16);
        state.subscribe_events_after(client, Some(pane(1)), 0, tx);
        assert_eq!(
            drain(&mut rx),
            vec![AgentEvent::Bell],
            "only pane 1's event"
        );
        let _ = record(&mut state, 1, AgentEvent::Dirty);
        assert_eq!(drain(&mut rx), vec![AgentEvent::Dirty], "live at once");
    }

    #[test]
    fn a_replay_larger_than_the_mailbox_is_pulled_in_order_as_room_frees() {
        let mut state = ServerState::new();
        for _ in 0..5 {
            let _ = record(&mut state, 1, AgentEvent::Bell);
        }
        let client = state.new_client_id();
        let (tx, mut rx) = mpsc::channel(2);
        state.subscribe_events_after(client, None, 0, tx);
        assert_eq!(drain(&mut rx).len(), 2, "what fits is queued at once");
        // Live events during the replay wait in the ring, behind it.
        let _ = record(&mut state, 1, AgentEvent::Idle);
        assert!(
            drain(&mut rx).is_empty(),
            "no live event overtakes the replay"
        );
        assert_eq!(
            pump_all(&mut state, client),
            vec![
                AgentEvent::Bell,
                AgentEvent::Bell,
                AgentEvent::Bell,
                AgentEvent::Idle
            ],
            "the rest of the replay, then what arrived during it; no gap"
        );
        let _ = record(&mut state, 1, AgentEvent::Dirty);
        assert_eq!(
            drain(&mut rx),
            vec![AgentEvent::Dirty],
            "live after catch-up"
        );
    }

    #[test]
    fn a_second_cursor_scope_never_redelivers_what_the_first_gave() {
        let mut state = ServerState::new();
        for terminal in [1, 2, 1] {
            let _ = record(&mut state, terminal, AgentEvent::Bell);
        }
        let client = state.new_client_id();
        let (tx, mut rx) = mpsc::channel(16);
        state.subscribe_events_after(client, None, 0, tx.clone());
        assert_eq!(drain(&mut rx).len(), 3);
        state.subscribe_events_after(client, Some(pane(1)), 0, tx);
        let second = drain(&mut rx);
        assert!(
            second
                .iter()
                .all(|event| matches!(event, AgentEvent::JournalGap { .. })),
            "at most a gap below what was already given, never a duplicate: {second:?}"
        );
    }

    #[test]
    fn expired_reads_as_released_to_a_subscription_without_a_cursor() {
        let expired = AgentEvent::TerminalControl {
            lifecycle: phux_protocol::wire::frame::ResourceLifecycle::Running,
            exit_status: None,
            input_holder: None,
            action: ControlAction::Expired,
            actor: None,
        };
        let mut state = ServerState::new();
        let legacy = state.new_client_id();
        let aware = state.new_client_id();
        let (legacy_tx, mut legacy_rx) = mpsc::channel(4);
        let (aware_tx, mut aware_rx) = mpsc::channel(4);
        state.subscribe_events(legacy, None, legacy_tx);
        state.subscribe_events_after(aware, None, u64::MAX, aware_tx);
        let _ = record(&mut state, 1, expired.clone());
        assert!(matches!(
            drain(&mut legacy_rx).as_slice(),
            [AgentEvent::TerminalControl {
                action: ControlAction::Released,
                ..
            }]
        ));
        assert_eq!(drain(&mut aware_rx), vec![expired]);
    }

    #[test]
    fn events_carry_the_actors_hello_name() {
        let mut state = ServerState::new();
        let client = state.new_client_id();
        state.set_client_name(client, "orchestrator".to_owned());
        let (tx, mut rx) = mpsc::channel(4);
        state.subscribe_events(client, None, tx);
        let _ = state.record_and_fanout(
            EventRecord::new(Some(pane(1)), AgentEvent::Bell).with_actor(Some(client)),
        );
        let Ok(Outbound::Frame(FrameKind::Event {
            stamp: Some(stamp), ..
        })) = rx.try_recv()
        else {
            panic!("a stamped event");
        };
        let actor = stamp.actor.expect("attributed");
        assert_eq!(actor.client_name.as_deref(), Some("orchestrator"));
        assert!(stamp.ts_ms > 0);
    }

    #[test]
    fn relayed_events_reach_only_their_satellite_scope_and_are_not_retained() {
        let satellite = WireResourceId::satellite("sat", 9);
        let mut state = ServerState::new();
        let wide = state.new_client_id();
        let scoped = state.new_client_id();
        let (wide_tx, mut wide_rx) = mpsc::channel(4);
        let (scoped_tx, mut scoped_rx) = mpsc::channel(4);
        state.subscribe_events(wide, None, wide_tx);
        state.subscribe_satellite_events(
            scoped,
            satellite.clone(),
            EventFilter::all(),
            None,
            scoped_tx,
        );
        let actor = phux_protocol::wire::frame::ActorRef::new(phux_protocol::ids::ClientId::new(3));
        let theirs = EventStamp::new(40, 1_234).with_actor(Some(actor));
        let seq = state
            .record_relayed_event(satellite.clone(), AgentEvent::Bell, Some(&theirs))
            .expect("stamped");
        assert!(drain(&mut wide_rx).is_empty(), "not a server-wide event");
        let Ok(Outbound::Frame(FrameKind::Event {
            terminal,
            stamp: Some(stamp),
            ..
        })) = scoped_rx.try_recv()
        else {
            panic!("the satellite scope receives it");
        };
        assert_eq!(terminal, Some(satellite));
        assert_eq!((stamp.seq, stamp.ts_ms), (seq, 1_234));
        assert_eq!(stamp.actor, None, "a satellite's actor does not cross");
        assert!(
            state.journal.replay_after(0, |_| true).entries.is_empty(),
            "relayed events are not retained"
        );
    }

    #[test]
    fn a_cursor_on_a_satellite_scope_is_owed_a_gap() {
        let mut state = ServerState::new();
        let _ = record(&mut state, 1, AgentEvent::Bell);
        let client = state.new_client_id();
        let (tx, mut rx) = mpsc::channel(4);
        state.subscribe_satellite_events(
            client,
            WireResourceId::satellite("sat", 9),
            EventFilter::all(),
            Some(0),
            tx,
        );
        assert_eq!(drain(&mut rx), vec![gap(1, 1)]);
    }

    /// A hub keeps no satellite events, so a satellite scope's cursor is
    /// owed a gap the ring holds nothing for. The head covers that gap,
    /// whether already sent or still owed behind a full mailbox, and a hub
    /// event on another scope after the subscribe does not raise it.
    #[test]
    fn the_head_covers_a_satellite_cursors_gap_and_no_later_hub_event() {
        let mut state = ServerState::new();
        let _ = record(&mut state, 1, AgentEvent::Bell);
        let past_cursor = record(&mut state, 1, AgentEvent::Bell);
        let subscribe = |state: &mut ServerState, tx| {
            let client = state.new_client_id();
            let satellite = WireResourceId::satellite("sat", 9);
            state.subscribe_satellite_events(client, satellite, EventFilter::all(), Some(1), tx);
            client
        };
        let (tx, mut rx) = mpsc::channel(4);
        let sent = subscribe(&mut state, tx);
        let (full_tx, _full_rx) = mpsc::channel(1);
        full_tx
            .try_send(Outbound::Frame(journal_gap_frame(0, 0)))
            .expect("room for the filler");
        let owed = subscribe(&mut state, full_tx);
        let later = record(&mut state, 2, AgentEvent::Bell);

        assert_eq!(drain(&mut rx), vec![gap(2, past_cursor)]);
        for client in [sent, owed] {
            let head = state.journal_head_for(Some(client));
            assert_eq!(head, past_cursor, "covers the gap's last_missing");
            assert!(head < later, "a later hub event does not raise it");
        }
    }

    /// A closed, unretained pane, a stale cursor, and a full ring: the
    /// replay reports the eviction and ends, and another terminal's event
    /// before the cut advances the eviction. The head stays at the gap the
    /// subscription was sent, not the eviction it will never hear of; a
    /// replay still pending below the eviction does count it.
    #[test]
    fn a_later_eviction_does_not_raise_the_head_past_the_gap_sent() {
        let mut state = ServerState::new();
        state.set_event_journal_bounds(2, 1 << 20);
        for _ in 0..3 {
            let _ = record(&mut state, 2, AgentEvent::Bell);
        }
        let closed = pane(7);
        let sent = state.new_client_id();
        let (tx, mut rx) = mpsc::channel(4);
        state.subscribe_events_after(sent, Some(closed.clone()), 0, tx);
        assert_eq!(drain(&mut rx), vec![gap(1, 1)]);
        let owed = state.new_client_id();
        let (full_tx, _full_rx) = mpsc::channel(1);
        full_tx
            .try_send(Outbound::Frame(journal_gap_frame(0, 0)))
            .expect("room for the filler");
        state.subscribe_events_after(owed, Some(closed), 0, full_tx);
        let _ = record(&mut state, 2, AgentEvent::Bell);

        assert_eq!(
            state.journal_head_for(Some(sent)),
            1,
            "the gap it was sent, not the eviction since"
        );
        assert_eq!(
            state.journal_head_for(Some(owed)),
            2,
            "a replay pending below the eviction will report it"
        );
    }

    #[test]
    fn an_interrupted_satellite_scope_is_owed_a_gap_and_stops_receiving() {
        let satellite = WireResourceId::satellite("sat", 9);
        let mut state = ServerState::new();
        let client = state.new_client_id();
        let (tx, mut rx) = mpsc::channel(4);
        state.subscribe_satellite_events(client, satellite.clone(), EventFilter::all(), None, tx);
        state.interrupt_satellite_scope(client, &satellite);
        assert!(
            matches!(drain(&mut rx).as_slice(), [AgentEvent::JournalGap { .. }]),
            "the interrupted stream is reported"
        );
        let _ = state.record_relayed_event(satellite, AgentEvent::Bell, None);
        assert!(drain(&mut rx).is_empty(), "the scope is gone");
    }

    /// Review probe P1: a void cursor merged into a pending replay reports
    /// `journal_gap{1, head}`, and nothing at or below that gap follows it.
    #[test]
    fn a_void_cursor_merged_into_a_pending_replay_never_redelivers_below_its_gap() {
        let mut state = ServerState::new();
        for _ in 0..10 {
            let _ = record(&mut state, 1, AgentEvent::Bell);
        }
        let client = state.new_client_id();
        let (tx, mut rx) = mpsc::channel(2);
        state.subscribe_events_after(client, None, 0, tx.clone());
        let _first = drain_stamped(&mut rx);
        state.subscribe_events_after(client, Some(pane(1)), 999, tx);
        let mut rest = drain_stamped(&mut rx);
        rest.extend(pump_stamped(&mut state, client));
        let gap_last = rest
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::JournalGap { last_missing, .. } => Some(*last_missing),
                _ => None,
            })
            .expect("the void cursor is reported");
        let after_gap: Vec<u64> = rest
            .iter()
            .skip_while(|(_, event)| !matches!(event, AgentEvent::JournalGap { .. }))
            .filter_map(|(seq, _)| *seq)
            .collect();
        assert!(
            after_gap.iter().all(|seq| *seq > gap_last),
            "gap last={gap_last}, then {after_gap:?}"
        );
    }

    /// Review probe P2: a re-subscribe the satellite refused puts the
    /// established scope back, filter included, rather than removing it.
    #[test]
    fn a_refused_resubscribe_restores_the_established_satellite_scope() {
        let satellite = WireResourceId::satellite("sat", 9);
        let mut state = ServerState::new();
        let client = state.new_client_id();
        let (tx, mut rx) = mpsc::channel(8);
        let _established = state.subscribe_satellite_events(
            client,
            satellite.clone(),
            EventFilter::all(),
            None,
            tx.clone(),
        );
        let refused = state.subscribe_satellite_events(
            client,
            satellite.clone(),
            EventFilter::of(vec![ResourceEventType::CwdChanged]),
            None,
            tx,
        );
        state.restore_satellite_scope(client, refused);
        let _ = state.record_relayed_event(satellite, AgentEvent::Bell, None);
        assert_eq!(
            drain(&mut rx),
            vec![AgentEvent::Bell],
            "the established, unfiltered scope still delivers"
        );
    }

    #[test]
    fn a_refused_first_subscribe_leaves_no_satellite_scope() {
        let satellite = WireResourceId::satellite("sat", 9);
        let mut state = ServerState::new();
        let client = state.new_client_id();
        let (tx, mut rx) = mpsc::channel(8);
        let refused = state.subscribe_satellite_events(
            client,
            satellite.clone(),
            EventFilter::all(),
            None,
            tx,
        );
        state.restore_satellite_scope(client, refused);
        let _ = state.record_relayed_event(satellite, AgentEvent::Bell, None);
        assert!(drain(&mut rx).is_empty());
        assert!(!state.clients.event_subscriptions.contains_key(&client));
    }

    /// Review probe P3: a pump that outlived its subscription (detach, then
    /// re-subscribe) is told it is gone instead of serving the new one,
    /// whose own pump takes over.
    #[test]
    fn a_pump_that_outlived_its_subscription_is_told_it_is_gone() {
        let mut state = ServerState::new();
        let client = state.new_client_id();
        let (tx, _rx) = mpsc::channel(8);
        state.subscribe_events(client, Some(pane(1)), tx.clone());
        let old = state.claim_event_pump(client).expect("first pump");
        state.unsubscribe_terminal_events(client, &pane(1));
        state.subscribe_events(client, Some(pane(1)), tx);
        let new = state
            .claim_event_pump(client)
            .expect("the new subscription has its own pump");
        assert!(!Arc::ptr_eq(&old.wake, &new.wake));
        assert!(matches!(
            state.next_owed_event_frame(client, old.epoch),
            PumpStep::Gone
        ));
        assert!(matches!(
            state.next_owed_event_frame(client, new.epoch),
            PumpStep::Idle
        ));
    }

    /// Review probe P4: a gap owed while a replay is pending (here a
    /// relayed event the replay cannot reach) goes out before any later
    /// `seq`.
    #[test]
    fn a_gap_owed_during_a_replay_arrives_before_later_seqs() {
        let satellite = WireResourceId::satellite("sat", 9);
        let mut state = ServerState::new();
        for _ in 0..4 {
            let _ = record(&mut state, 1, AgentEvent::Bell);
        }
        let client = state.new_client_id();
        let (tx, mut rx) = mpsc::channel(1);
        let _ = state.subscribe_satellite_events(
            client,
            satellite.clone(),
            EventFilter::all(),
            None,
            tx.clone(),
        );
        state.subscribe_events_after(client, None, 0, tx);
        let relayed = state
            .record_relayed_event(satellite, AgentEvent::Bell, None)
            .expect("stamped");
        let later = record(&mut state, 1, AgentEvent::Idle);
        let mut stream = drain_stamped(&mut rx);
        stream.extend(pump_stamped(&mut state, client));
        let seqs: Vec<Option<u64>> = stream.iter().map(|(seq, _)| *seq).collect();
        assert_eq!(
            seqs,
            vec![Some(1), Some(2), Some(3), Some(4), None, Some(later)],
            "{stream:?}"
        );
        assert_eq!(stream[4].1, gap(relayed, relayed));
    }
}
