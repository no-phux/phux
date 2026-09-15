//! The server-wide event journal (ADR-0123, `docs/spec/L1.md` §7.3).
//!
//! Every semantic event the server emits is stamped here exactly once, with
//! one server-wide `seq`, and kept in a bounded ring so a subscription that
//! names a cursor can be replayed from it. The ring bounds both the number
//! of events and their estimated encoded size, evicting whole events oldest
//! first. It is memory: a restart empties it, which the incarnation rule
//! (`HELLO_OK.server_id`) makes visible to a consumer.
//!
//! The journal never delivers anything itself. [`super::events`] owns the
//! subscription registry and fans each recorded entry out; this module only
//! answers "what is the next stamp" and "what can be replayed after `n`".

use std::collections::VecDeque;

use phux_protocol::ids::{IdempotencyKey, ResourceId as WireResourceId};
use phux_protocol::wire::frame::{ActorRef, AgentEvent, EventStamp, FrameKind};

use super::client::ClientId;

/// Fixed per-entry overhead in the byte estimate: the frame header, the
/// envelope's field tags, and the stamp's two `u64`s.
const ENTRY_OVERHEAD_BYTES: usize = 48;

/// One event as an emitter hands it to [`super::ServerState::record_and_fanout`].
#[derive(Debug, Clone)]
pub struct EventRecord {
    /// The resource the event concerns, or `None` for a server-scoped one.
    pub terminal: Option<WireResourceId>,
    /// The resource whose watchers also receive this event (ADR-0104 §2):
    /// the parent of a child whose `pane_spawned` / `pane_closed` this is.
    pub parent: Option<WireResourceId>,
    /// The event itself.
    pub event: AgentEvent,
    /// The connection that caused the event; `None` for a server-driven one.
    pub actor: Option<ClientId>,
    /// The idempotency key of the operation that caused the event.
    pub operation_id: Option<IdempotencyKey>,
}

impl EventRecord {
    /// A server-driven event about `terminal`.
    #[must_use]
    pub const fn new(terminal: Option<WireResourceId>, event: AgentEvent) -> Self {
        Self {
            terminal,
            parent: None,
            event,
            actor: None,
            operation_id: None,
        }
    }

    /// Widen the audience to `parent`'s watchers (ADR-0104 §2).
    #[must_use]
    pub fn with_parent(mut self, parent: Option<WireResourceId>) -> Self {
        self.parent = parent;
        self
    }

    /// Attribute the event to the connection that caused it.
    #[must_use]
    pub const fn with_actor(mut self, actor: Option<ClientId>) -> Self {
        self.actor = actor;
        self
    }

    /// Name the idempotency key of the operation that caused the event
    /// (`SPAWN_RESOURCE` field 17, ADR-0126).
    #[must_use]
    pub const fn with_operation_id(mut self, operation_id: Option<IdempotencyKey>) -> Self {
        self.operation_id = operation_id;
        self
    }
}

/// One stamped event held in the ring.
///
/// Holds the decoded event rather than an encoded frame: a subscriber that
/// predates a value sees it rendered differently (`EXPIRED`, L1 §7.1), so the
/// frame is built per delivery.
#[derive(Debug, Clone)]
pub(crate) struct JournalEntry {
    /// The resource the event concerns.
    pub(crate) terminal: Option<WireResourceId>,
    /// The widened audience, as [`EventRecord::parent`].
    pub(crate) parent: Option<WireResourceId>,
    /// The event.
    pub(crate) event: AgentEvent,
    /// The stamp every subscriber sees.
    pub(crate) stamp: EventStamp,
    /// Estimated encoded size, charged against the byte bound.
    bytes: usize,
    /// A federation hub relayed this event from a satellite: it took a
    /// `seq` here but is not retained, and only its satellite scope
    /// receives it.
    relayed: bool,
}

impl JournalEntry {
    /// A relayed satellite event, stamped by this journal and never
    /// retained in it.
    #[must_use]
    pub(crate) const fn relayed(
        terminal: WireResourceId,
        event: AgentEvent,
        stamp: EventStamp,
    ) -> Self {
        Self {
            terminal: Some(terminal),
            parent: None,
            event,
            stamp,
            bytes: 0,
            relayed: true,
        }
    }

    /// Whether a hub relayed this event from a satellite.
    #[must_use]
    pub(crate) const fn is_relayed(&self) -> bool {
        self.relayed
    }

    /// The journal sequence of this entry.
    #[must_use]
    pub(crate) const fn seq(&self) -> u64 {
        self.stamp.seq
    }

    /// The `EVENT` frame carrying `event` (this entry's, or a rendering of
    /// it) under this entry's scope and stamp.
    #[must_use]
    pub(crate) fn frame_with(&self, event: AgentEvent) -> FrameKind {
        FrameKind::Event {
            terminal: self.terminal.clone(),
            event,
            stamp: Some(Box::new(self.stamp.clone())),
        }
    }
}

/// What a cursor subscription is owed, per L1 §7.3.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct Replay {
    /// A leading `journal_gap` range, when the ring no longer covers the
    /// cursor or the cursor was never issued by this journal.
    pub(crate) gap: Option<(u64, u64)>,
    /// Retained entries after the cursor that the caller's filter admitted,
    /// in `seq` order.
    pub(crate) entries: Vec<JournalEntry>,
}

#[cfg(test)]
impl Replay {
    /// Nothing to send.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn is_empty(&self) -> bool {
        self.gap.is_none() && self.entries.is_empty()
    }
}

/// One step of a pull-based replay ([`Journal::next_after`]).
#[derive(Debug)]
pub(crate) enum NextEntry {
    /// The ring evicted `first..=last` past the replay's position.
    Gap(u64, u64),
    /// The next admitted retained entry (boxed: it dwarfs the other
    /// variants).
    Entry(Box<JournalEntry>),
    /// Nothing retained after the position is admitted: the replay is
    /// done and the subscription is live.
    CaughtUp,
}

/// The bounded ring of stamped events.
#[derive(Debug)]
pub(crate) struct Journal {
    entries: VecDeque<JournalEntry>,
    /// The last `seq` assigned; `0` before the first.
    head: u64,
    /// The newest `seq` evicted from the ring; `0` while nothing has been.
    /// A cursor below it cannot be replayed in full.
    evicted_through: u64,
    /// Sum of the retained entries' estimated sizes.
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl Journal {
    /// An empty journal bounded by `max_entries` events and `max_bytes`
    /// estimated encoded bytes.
    #[must_use]
    pub(crate) const fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            head: 0,
            evicted_through: 0,
            bytes: 0,
            max_entries,
            max_bytes,
        }
    }

    /// Re-bound the ring, evicting whatever the new bounds no longer hold.
    pub(crate) fn set_bounds(&mut self, max_entries: usize, max_bytes: usize) {
        self.max_entries = max_entries;
        self.max_bytes = max_bytes;
        self.evict_to_bounds();
    }

    /// The newest `seq` assigned, `0` before the first.
    #[must_use]
    pub(crate) const fn head(&self) -> u64 {
        self.head
    }

    /// The newest retained `seq` that `admits` accepts, `0` when none is.
    /// Scans the ring newest first, so it is bounded by it.
    #[must_use]
    pub(crate) fn newest_admitted(&self, admits: impl Fn(&JournalEntry) -> bool) -> u64 {
        self.entries
            .iter()
            .rev()
            .find(|entry| admits(entry))
            .map_or(0, JournalEntry::seq)
    }

    /// The newest `seq` evicted from the ring; `0` while nothing has been.
    #[must_use]
    pub(crate) const fn evicted_through(&self) -> u64 {
        self.evicted_through
    }

    /// Assign the next `seq`, or `None` once the sequence is exhausted:
    /// `2^64 - 1` is never assigned (it is the no-replay cursor).
    pub(crate) fn allocate_seq(&mut self) -> Option<u64> {
        let next = self.head.checked_add(1).filter(|seq| *seq < u64::MAX)?;
        self.head = next;
        Some(next)
    }

    /// Stamp `record` with the next `seq`, retain it, and return the entry
    /// for delivery. `None` when the sequence is exhausted.
    pub(crate) fn record(
        &mut self,
        record: EventRecord,
        actor: Option<ActorRef>,
        ts_ms: u64,
    ) -> Option<JournalEntry> {
        let seq = self.allocate_seq()?;
        let bytes = estimated_bytes(&record, actor.as_ref());
        let stamp = EventStamp::new(seq, ts_ms)
            .with_actor(actor)
            .with_operation_id(record.operation_id);
        let entry = JournalEntry {
            terminal: record.terminal,
            parent: record.parent,
            event: record.event,
            stamp,
            bytes,
            relayed: false,
        };
        self.entries.push_back(entry.clone());
        self.bytes = self.bytes.saturating_add(bytes);
        self.evict_to_bounds();
        Some(entry)
    }

    /// What a subscription whose cursor is `after` is owed, keeping only the
    /// entries `admits` accepts.
    ///
    /// L1 §7.3: `2^64 - 1` asks for nothing; a cursor at the head is owed
    /// nothing; a cursor below the head is owed every retained entry after
    /// it, led by a gap when the ring no longer holds `after + 1`; and a
    /// cursor ahead of the head was never issued by this journal, so it is
    /// void and owed only a gap.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn replay_after(
        &self,
        after: u64,
        admits: impl Fn(&JournalEntry) -> bool,
    ) -> Replay {
        if after >= self.head {
            return Replay {
                gap: self.void_cursor_gap(after),
                entries: Vec::new(),
            };
        }
        Replay {
            gap: (after < self.evicted_through).then(|| (after + 1, self.evicted_through)),
            entries: self
                .entries
                .iter()
                .filter(|entry| entry.seq() > after && admits(entry))
                .cloned()
                .collect(),
        }
    }

    /// The next thing a pull-based replay positioned at `after` is owed:
    /// the eviction gap when the ring no longer holds `after + 1`, else the
    /// first retained entry after it that `admits` accepts, else nothing
    /// (the replay has caught up with the head).
    pub(crate) fn next_after(
        &self,
        after: u64,
        admits: impl Fn(&JournalEntry) -> bool,
    ) -> NextEntry {
        if after < self.evicted_through {
            return NextEntry::Gap(after + 1, self.evicted_through);
        }
        let start = self.entries.partition_point(|entry| entry.seq() <= after);
        self.entries
            .range(start..)
            .find(|entry| admits(entry))
            .cloned()
            .map_or(NextEntry::CaughtUp, |entry| {
                NextEntry::Entry(Box::new(entry))
            })
    }

    /// The gap a cursor is owed on a scope this journal does not retain
    /// (a satellite scope on a hub): everything after it, or the void
    /// cursor's gap.
    #[must_use]
    pub(crate) const fn gap_without_replay(&self, after: u64) -> Option<(u64, u64)> {
        if after < self.head {
            return Some((after + 1, self.head));
        }
        self.void_cursor_gap(after)
    }

    /// The gap a cursor at or past the head is owed: none for the head
    /// itself or the no-replay sentinel, and `{ 1, head }` ("everything
    /// this incarnation issued") for a cursor it never issued (L1 §7.3).
    ///
    /// `last_missing` is the head, so a consumer that resumes from it after
    /// re-reading level state resumes correctly. Before anything was
    /// journaled that is `{ 1, 0 }`, the empty range the spec names.
    const fn void_cursor_gap(&self, after: u64) -> Option<(u64, u64)> {
        if after == self.head || after == u64::MAX {
            return None;
        }
        Some((1, self.head))
    }

    /// Evict whole entries, oldest first, until both bounds hold.
    fn evict_to_bounds(&mut self) {
        while self.over_bounds() {
            let Some(evicted) = self.entries.pop_front() else {
                return;
            };
            self.bytes = self.bytes.saturating_sub(evicted.bytes);
            self.evicted_through = evicted.seq();
        }
    }

    fn over_bounds(&self) -> bool {
        self.entries.len() > self.max_entries || self.bytes > self.max_bytes
    }
}

/// Estimated encoded size of one entry: the fixed envelope plus every
/// variable-length field it will carry on the wire.
fn estimated_bytes(record: &EventRecord, actor: Option<&ActorRef>) -> usize {
    ENTRY_OVERHEAD_BYTES
        + resource_id_bytes(record.terminal.as_ref())
        + resource_id_bytes(record.parent.as_ref())
        + event_body_bytes(&record.event)
        + actor.map_or(0, actor_bytes)
}

fn resource_id_bytes(id: Option<&WireResourceId>) -> usize {
    match id {
        None => 0,
        Some(WireResourceId::Satellite { host, .. }) => 8 + host.as_str().len(),
        Some(_) => 8,
    }
}

fn actor_bytes(actor: &ActorRef) -> usize {
    8 + actor.credential_id.as_ref().map_or(0, String::len)
        + actor.client_name.as_ref().map_or(0, String::len)
}

fn event_body_bytes(event: &AgentEvent) -> usize {
    let variable = match event {
        AgentEvent::TitleChanged { title } => title.len(),
        AgentEvent::CwdChanged { cwd } => cwd.len(),
        AgentEvent::Asked {
            id,
            question,
            suggestions,
            ..
        } => id.len() + question.len() + suggestions.iter().map(|s| s.len() + 4).sum::<usize>(),
        AgentEvent::Unknown { body, .. } => body.len(),
        _ => 0,
    };
    16 + variable
}

/// The server's wall clock in Unix milliseconds, `0` if it reads before the
/// epoch.
#[must_use]
pub(crate) fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bell(terminal: u32) -> EventRecord {
        EventRecord::new(Some(WireResourceId::local(terminal)), AgentEvent::Bell)
    }

    fn seqs(replay: &Replay) -> Vec<u64> {
        replay.entries.iter().map(JournalEntry::seq).collect()
    }

    #[test]
    fn seq_starts_at_one_and_strictly_increases() {
        let mut journal = Journal::new(16, 1 << 20);
        let first = journal.record(bell(1), None, 5).expect("stamped");
        let second = journal.record(bell(2), None, 6).expect("stamped");
        assert_eq!((first.seq(), second.seq()), (1, 2));
        assert_eq!(journal.head(), 2);
        assert_eq!(second.stamp.ts_ms, 6);
    }

    #[test]
    fn the_no_replay_sentinel_is_never_assigned() {
        let mut journal = Journal::new(16, 1 << 20);
        journal.head = u64::MAX - 2;
        assert_eq!(journal.allocate_seq(), Some(u64::MAX - 1));
        assert_eq!(journal.allocate_seq(), None, "2^64 - 1 is never a seq");
        assert!(journal.record(bell(1), None, 0).is_none());
    }

    #[test]
    fn replay_after_returns_later_entries_in_order_filtered() {
        let mut journal = Journal::new(16, 1 << 20);
        for terminal in [1, 2, 1, 2] {
            let _ = journal.record(bell(terminal), None, 0);
        }
        let only_one = |entry: &JournalEntry| entry.terminal == Some(WireResourceId::local(1));
        let replay = journal.replay_after(0, only_one);
        assert_eq!(replay.gap, None);
        assert_eq!(seqs(&replay), vec![1, 3]);
        assert_eq!(seqs(&journal.replay_after(2, |_| true)), vec![3, 4]);
    }

    #[test]
    fn cursors_at_the_head_or_the_sentinel_are_owed_nothing() {
        let mut journal = Journal::new(16, 1 << 20);
        let _ = journal.record(bell(1), None, 0);
        assert!(journal.replay_after(1, |_| true).is_empty());
        assert!(journal.replay_after(u64::MAX, |_| true).is_empty());
    }

    #[test]
    fn a_cursor_ahead_of_the_head_is_void() {
        let mut journal = Journal::new(16, 1 << 20);
        assert_eq!(
            journal.replay_after(9, |_| true).gap,
            Some((1, 0)),
            "an empty journal names the empty range"
        );
        let _ = journal.record(bell(1), None, 0);
        let _ = journal.record(bell(1), None, 0);
        let replay = journal.replay_after(9, |_| true);
        assert_eq!(replay.gap, Some((1, 2)));
        assert!(replay.entries.is_empty());
    }

    #[test]
    fn eviction_by_count_reports_the_gap_it_opened() {
        let mut journal = Journal::new(2, 1 << 20);
        for _ in 0..5 {
            let _ = journal.record(bell(1), None, 0);
        }
        let replay = journal.replay_after(0, |_| true);
        assert_eq!(replay.gap, Some((1, 3)));
        assert_eq!(seqs(&replay), vec![4, 5]);
        assert_eq!(journal.replay_after(3, |_| true).gap, None);
    }

    #[test]
    fn eviction_by_bytes_drops_whole_entries_oldest_first() {
        let long = |n: usize| {
            EventRecord::new(
                Some(WireResourceId::local(1)),
                AgentEvent::TitleChanged {
                    title: "x".repeat(n),
                },
            )
        };
        let mut journal = Journal::new(1024, 400);
        let _ = journal.record(long(100), None, 0);
        let _ = journal.record(long(100), None, 0);
        let _ = journal.record(long(100), None, 0);
        assert!(journal.bytes <= 400);
        let replay = journal.replay_after(0, |_| true);
        assert_eq!(replay.gap, Some((1, 1)));
        assert_eq!(seqs(&replay), vec![2, 3]);
    }

    #[test]
    fn allocated_but_unretained_seqs_are_not_reported_as_gaps() {
        let mut journal = Journal::new(16, 1 << 20);
        let _ = journal.record(bell(1), None, 0);
        let _ = journal.allocate_seq();
        let _ = journal.record(bell(1), None, 0);
        let replay = journal.replay_after(0, |_| true);
        assert_eq!(replay.gap, None);
        assert_eq!(seqs(&replay), vec![1, 3]);
    }

    #[test]
    fn the_newest_admitted_seq_ignores_other_scopes() {
        let mut journal = Journal::new(3, 1 << 20);
        for terminal in [1, 2, 1, 2] {
            let _ = journal.record(bell(terminal), None, 0);
        }
        let only = |terminal: u32| {
            move |entry: &JournalEntry| entry.terminal == Some(WireResourceId::local(terminal))
        };
        assert_eq!(journal.newest_admitted(only(1)), 3);
        assert_eq!(journal.newest_admitted(only(2)), 4);
        assert_eq!(journal.newest_admitted(only(9)), 0, "nothing retained");
        assert_eq!(journal.evicted_through(), 1);
        assert_eq!(Journal::new(3, 1 << 20).newest_admitted(|_| true), 0);
    }

    #[test]
    fn narrowing_the_bounds_evicts_immediately() {
        let mut journal = Journal::new(16, 1 << 20);
        for _ in 0..4 {
            let _ = journal.record(bell(1), None, 0);
        }
        journal.set_bounds(1, 1 << 20);
        assert_eq!(journal.replay_after(0, |_| true).gap, Some((1, 3)));
    }
}
