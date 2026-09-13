//! Bounded, per-stream diagnostics for transport queue and write stalls.
//!
//! The global registry retains at most [`MAX_STREAMS`] entries. Each stream
//! retains metadata for at most [`MAX_QUEUED_ITEMS`] admitted ingress items;
//! payload bytes and caller-provided labels are never retained.

use std::array;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use serde::Serialize;

/// Maximum number of streams exposed by one diagnostic snapshot.
pub const MAX_STREAMS: usize = 128;
/// Maximum number of outstanding ingress queue items tracked per stream.
pub const MAX_QUEUED_ITEMS: usize = 256;
const MAX_ACTIVE_WRITES: usize = 8;

/// The fixed traffic class carried by a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamLane {
    /// Connection-level commands and lifecycle traffic.
    Control,
    /// Terminal output or other per-terminal traffic.
    Terminal,
}

/// Fixed identity and traffic class for a registered stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StreamContext {
    /// Runtime connection identity. This is an opaque number, not a label.
    pub connection_id: u64,
    /// Transport stream identity.
    pub stream_id: u64,
    /// Traffic class assigned when the stream is bound.
    pub lane: StreamLane,
}

/// The lifecycle event whose READY latency was measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadyKind {
    /// The stream's first READY after binding.
    Initial,
    /// READY completing an in-band resynchronization.
    Resync,
}

/// A bounded reason vocabulary for the most recent stream resynchronization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResyncReason {
    /// The consumer fell behind a bounded producer queue.
    Lagged,
    /// A sequence gap was observed.
    SequenceGap,
    /// The peer explicitly requested a fresh bootstrap.
    PeerRequested,
    /// Stream lifecycle or binding recovery required a fresh bootstrap.
    LifecycleRecovery,
    /// Terminal geometry changed.
    Resize,
    /// Native or compatibility codec could not preserve the generation.
    CodecFailure,
    /// A bounded wire reason not covered by the named categories.
    Other,
}

/// Latest READY observation for a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReadySnapshot {
    /// Bounded READY category.
    pub kind: ReadyKind,
    /// READY latency in microseconds.
    pub latency_us: u64,
}

/// A serializable, point-in-time view of one registered stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StreamSnapshot {
    /// Fixed stream identity and lane.
    pub context: StreamContext,
    /// Whether the runtime currently considers the binding active.
    pub active: bool,
    /// Number of admitted ingress items still outstanding.
    pub queue_items: u64,
    /// Payload bytes represented by outstanding ingress tickets.
    pub queue_bytes: u64,
    /// Age of the oldest outstanding ingress item, in microseconds.
    pub queue_oldest_age_us: Option<u64>,
    /// Ingress items omitted because all metadata slots were occupied.
    pub queue_overflow: u64,
    /// Age of the oldest in-progress transport write, in microseconds.
    pub write_in_progress_age_us: Option<u64>,
    /// Duration of the most recently completed transport write.
    pub write_last_duration_us: Option<u64>,
    /// Maximum completed transport-write duration since the last reset.
    pub write_max_duration_us: Option<u64>,
    /// Writes omitted because every fixed write metadata slot was occupied.
    pub write_overflow: u64,
    /// Most recent READY latency, when one has been recorded.
    pub ready: Option<ReadySnapshot>,
    /// Most recent bounded resynchronization reason.
    pub resync_reason: Option<ResyncReason>,
}

/// Bounded global diagnostic view returned to a reporting caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StreamDiagnosticsSnapshot {
    /// Registered streams, capped at [`MAX_STREAMS`].
    pub streams: Vec<StreamSnapshot>,
    /// Registration attempts suppressed while all stream slots were occupied.
    pub suppressed_streams: u64,
}

/// A fixed-cardinality registry that can also be instantiated by tests.
#[derive(Clone)]
pub struct StreamDiagnostics {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    state: Mutex<RegistryState>,
}

struct RegistryState {
    slots: [Option<RegistryEntry>; MAX_STREAMS],
    cursor: usize,
    next_generation: u64,
    suppressed_streams: u64,
}

struct RegistryEntry {
    generation: u64,
    tracker: Weak<TrackerInner>,
}

/// RAII registration whose drop removes the stream from its registry.
#[derive(Debug)]
pub struct StreamRegistration {
    registry: Weak<RegistryInner>,
    slot: Option<RegistrationSlot>,
    tracker: StreamTracker,
}

#[derive(Debug, Clone, Copy)]
struct RegistrationSlot {
    index: usize,
    generation: u64,
}

/// Cloneable handle used by queue, writer, and lifecycle production paths.
#[derive(Clone)]
pub struct StreamTracker {
    context: StreamContext,
    inner: Option<Arc<TrackerInner>>,
}

impl std::fmt::Debug for StreamTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamTracker")
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for StreamDiagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamDiagnostics").finish_non_exhaustive()
    }
}

struct TrackerInner {
    context: StreamContext,
    state: Mutex<TrackerState>,
}

struct TrackerState {
    active: bool,
    queue: [Option<QueueItem>; MAX_QUEUED_ITEMS],
    next_queue_id: u64,
    queue_bytes: u64,
    queue_overflow: u64,
    writes: [Option<WriteItem>; MAX_ACTIVE_WRITES],
    next_write_id: u64,
    write_last_duration: Option<Duration>,
    write_max_duration: Option<Duration>,
    write_overflow: u64,
    ready: Option<(ReadyKind, Duration)>,
    resync_reason: Option<ResyncReason>,
}

#[derive(Clone, Copy)]
struct QueueItem {
    id: u64,
    bytes: u64,
    enqueued_at: Instant,
}

#[derive(Clone, Copy)]
struct WriteItem {
    id: u64,
    started_at: Instant,
}

/// RAII metadata for one admitted ingress item.
#[derive(Debug)]
pub struct QueueTicket {
    tracker: Weak<TrackerInner>,
    slot: Option<usize>,
    id: u64,
}

/// RAII measurement for one in-progress transport write.
#[derive(Debug)]
pub struct WriteGuard {
    tracker: Weak<TrackerInner>,
    slot: Option<usize>,
    id: u64,
    started_at: Instant,
    finished: bool,
}

impl Default for StreamDiagnostics {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamDiagnostics {
    /// Construct an empty fixed-cardinality registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                state: Mutex::new(RegistryState {
                    slots: array::from_fn(|_| None),
                    cursor: 0,
                    next_generation: 1,
                    suppressed_streams: 0,
                }),
            }),
        }
    }

    /// Register a stream and return its RAII lifetime binding.
    ///
    /// When all 128 slots are live, the returned tracker remains safe to use
    /// but is absent from snapshots and `suppressed_streams` is incremented.
    #[must_use]
    pub fn register(&self, context: StreamContext) -> StreamRegistration {
        let (tracker, slot) = self.insert(context);
        StreamRegistration {
            registry: Arc::downgrade(&self.inner),
            slot,
            tracker,
        }
    }

    /// Snapshot at most 128 registered streams.
    #[must_use]
    pub fn snapshot(&self) -> StreamDiagnosticsSnapshot {
        self.snapshot_at(Instant::now())
    }

    /// Clear interval observations without unregistering streams or
    /// invalidating active queue tickets and write guards.
    pub fn reset(&self) {
        let mut registry = lock(&self.inner.state);
        registry.retain_and_apply(TrackerInner::reset);
        registry.suppressed_streams = 0;
    }

    fn insert(&self, context: StreamContext) -> (StreamTracker, Option<RegistrationSlot>) {
        let mut registry = lock(&self.inner.state);
        registry.prune_dead();
        let Some(index) = registry.vacant_slot() else {
            return (
                StreamTracker {
                    context,
                    inner: None,
                },
                None,
            );
        };
        let tracker = StreamTracker::new(context);
        let generation = registry.next_generation;
        registry.next_generation = registry.next_generation.wrapping_add(1).max(1);
        registry.slots[index] = Some(RegistryEntry {
            generation,
            tracker: tracker
                .inner
                .as_ref()
                .map_or_else(Weak::new, Arc::downgrade),
        });
        registry.cursor = (index + 1) % MAX_STREAMS;
        drop(registry);
        (tracker, Some(RegistrationSlot { index, generation }))
    }

    fn snapshot_at(&self, now: Instant) -> StreamDiagnosticsSnapshot {
        let mut registry = lock(&self.inner.state);
        let mut streams = Vec::with_capacity(MAX_STREAMS);
        registry.retain_and_apply(|tracker| streams.push(tracker.snapshot_at(now)));
        StreamDiagnosticsSnapshot {
            streams,
            suppressed_streams: registry.suppressed_streams,
        }
    }
}

impl RegistryState {
    fn prune_dead(&mut self) {
        self.retain_and_apply(|_| {});
    }

    fn retain_and_apply(&mut self, mut apply: impl FnMut(&TrackerInner)) {
        for slot in &mut self.slots {
            let Some(entry) = slot else { continue };
            if let Some(tracker) = entry.tracker.upgrade() {
                apply(&tracker);
            } else {
                *slot = None;
            }
        }
    }

    fn vacant_slot(&mut self) -> Option<usize> {
        for offset in 0..MAX_STREAMS {
            let index = (self.cursor + offset) % MAX_STREAMS;
            if self.slots[index].is_none() {
                return Some(index);
            }
        }
        self.suppressed_streams = self.suppressed_streams.saturating_add(1);
        None
    }
}

impl Drop for StreamRegistration {
    fn drop(&mut self) {
        let (Some(registry), Some(slot)) = (self.registry.upgrade(), self.slot) else {
            return;
        };
        let mut state = lock(&registry.state);
        if state.slots[slot.index]
            .as_ref()
            .is_some_and(|entry| entry.generation == slot.generation)
        {
            state.slots[slot.index] = None;
        }
    }
}

impl StreamRegistration {
    /// Clone the lightweight tracker used by production queue and writer paths.
    #[must_use]
    pub fn tracker(&self) -> StreamTracker {
        self.tracker.clone()
    }

    /// Whether this stream received one of the registry's bounded slots.
    #[must_use]
    pub const fn is_registered(&self) -> bool {
        self.slot.is_some()
    }
}

impl StreamTracker {
    /// Attribute an admitted bootstrap tombstone using a fixed wire vocabulary.
    pub fn record_tombstone(&self, reason: phux_protocol::wire::frame::TombstoneReason) {
        use phux_protocol::wire::frame::TombstoneReason;
        let reason = match reason {
            TombstoneReason::RawReplayOverflow => ResyncReason::Lagged,
            TombstoneReason::OutboundGap => ResyncReason::SequenceGap,
            TombstoneReason::Resize => ResyncReason::Resize,
            TombstoneReason::RelayReconnect => ResyncReason::LifecycleRecovery,
            TombstoneReason::ExplicitReattach => ResyncReason::PeerRequested,
            TombstoneReason::CodecFailure => ResyncReason::CodecFailure,
            _ => ResyncReason::Other,
        };
        self.record_resync(reason);
    }

    /// Fixed correlation identity assigned at registration.
    #[must_use]
    pub const fn context(&self) -> StreamContext {
        self.context
    }

    fn new(context: StreamContext) -> Self {
        Self {
            context,
            inner: Some(Arc::new(TrackerInner {
                context,
                state: Mutex::new(TrackerState {
                    active: false,
                    queue: array::from_fn(|_| None),
                    next_queue_id: 1,
                    queue_bytes: 0,
                    queue_overflow: 0,
                    writes: array::from_fn(|_| None),
                    next_write_id: 1,
                    write_last_duration: None,
                    write_max_duration: None,
                    write_overflow: 0,
                    ready: None,
                    resync_reason: None,
                }),
            })),
        }
    }

    /// Bind or unbind this tracker from the runtime's active stream lifecycle.
    pub fn set_active(&self, active: bool) {
        if let Some(inner) = &self.inner {
            lock(&inner.state).active = active;
        }
    }

    /// Track bytes admitted to the stream's ingress queue.
    ///
    /// Dropping the returned ticket removes the exact item even when work is
    /// cancelled or tickets complete out of enqueue order.
    #[must_use]
    pub fn enqueue(&self, bytes: u64) -> QueueTicket {
        self.enqueue_at(bytes, Instant::now())
    }

    /// Begin measuring a potentially blocked transport write.
    ///
    /// Finishing or dropping the guard records its duration and removes it
    /// from the in-progress gauge.
    #[must_use]
    pub fn begin_write(&self) -> WriteGuard {
        self.begin_write_at(Instant::now())
    }

    /// Record the latest READY latency using a bounded lifecycle category.
    pub fn record_ready_latency(&self, kind: ReadyKind, latency: Duration) {
        if let Some(inner) = &self.inner {
            lock(&inner.state).ready = Some((kind, latency));
        }
    }

    /// Record the latest resynchronization reason from a bounded vocabulary.
    pub fn record_resync(&self, reason: ResyncReason) {
        if let Some(inner) = &self.inner {
            lock(&inner.state).resync_reason = Some(reason);
        }
    }

    fn enqueue_at(&self, bytes: u64, now: Instant) -> QueueTicket {
        let Some(inner) = &self.inner else {
            return QueueTicket {
                tracker: Weak::new(),
                slot: None,
                id: 0,
            };
        };
        let mut state = lock(&inner.state);
        let id = state.next_queue_id;
        state.next_queue_id = state.next_queue_id.wrapping_add(1).max(1);
        let slot = state.queue.iter().position(Option::is_none);
        if let Some(index) = slot {
            state.queue[index] = Some(QueueItem {
                id,
                bytes,
                enqueued_at: now,
            });
            state.queue_bytes = state.queue_bytes.saturating_add(bytes);
        } else {
            state.queue_overflow = state.queue_overflow.saturating_add(1);
        }
        drop(state);
        QueueTicket {
            tracker: Arc::downgrade(inner),
            slot,
            id,
        }
    }

    fn begin_write_at(&self, now: Instant) -> WriteGuard {
        let Some(inner) = &self.inner else {
            return WriteGuard {
                tracker: Weak::new(),
                slot: None,
                id: 0,
                started_at: now,
                finished: false,
            };
        };
        let mut state = lock(&inner.state);
        let id = state.next_write_id;
        state.next_write_id = state.next_write_id.wrapping_add(1).max(1);
        let slot = state.writes.iter().position(Option::is_none);
        if let Some(index) = slot {
            state.writes[index] = Some(WriteItem {
                id,
                started_at: now,
            });
        } else {
            state.write_overflow = state.write_overflow.saturating_add(1);
        }
        drop(state);
        WriteGuard {
            tracker: Arc::downgrade(inner),
            slot,
            id,
            started_at: now,
            finished: false,
        }
    }
}

impl TrackerInner {
    fn snapshot_at(&self, now: Instant) -> StreamSnapshot {
        let state = lock(&self.state);
        let queue_oldest = state
            .queue
            .iter()
            .flatten()
            .map(|item| item.enqueued_at)
            .min();
        let write_oldest = state
            .writes
            .iter()
            .flatten()
            .map(|item| item.started_at)
            .min();
        StreamSnapshot {
            context: self.context,
            active: state.active,
            queue_items: state.queue.iter().flatten().count() as u64,
            queue_bytes: state.queue_bytes,
            queue_oldest_age_us: queue_oldest.map(|started| elapsed_us(now, started)),
            queue_overflow: state.queue_overflow,
            write_in_progress_age_us: write_oldest.map(|started| elapsed_us(now, started)),
            write_last_duration_us: state.write_last_duration.map(duration_us),
            write_max_duration_us: state.write_max_duration.map(duration_us),
            write_overflow: state.write_overflow,
            ready: state.ready.map(|(kind, latency)| ReadySnapshot {
                kind,
                latency_us: duration_us(latency),
            }),
            resync_reason: state.resync_reason,
        }
    }

    fn reset(&self) {
        let mut state = lock(&self.state);
        state.queue_overflow = 0;
        state.write_last_duration = None;
        state.write_max_duration = None;
        state.write_overflow = 0;
        state.ready = None;
        state.resync_reason = None;
    }
}

impl Drop for QueueTicket {
    fn drop(&mut self) {
        let (Some(tracker), Some(index)) = (self.tracker.upgrade(), self.slot) else {
            return;
        };
        let mut state = lock(&tracker.state);
        let Some(item) = state.queue[index] else {
            return;
        };
        if item.id == self.id {
            state.queue[index] = None;
            state.queue_bytes = state.queue_bytes.saturating_sub(item.bytes);
        }
    }
}

impl WriteGuard {
    /// Finish the write now. Dropping without calling this method has the same
    /// cancellation-safe accounting behavior.
    pub fn finish(mut self) {
        self.finish_at(Instant::now());
    }

    fn finish_at(&mut self, now: Instant) {
        if self.finished {
            return;
        }
        self.finished = true;
        let Some(tracker) = self.tracker.upgrade() else {
            return;
        };
        let duration = now.saturating_duration_since(self.started_at);
        let mut state = lock(&tracker.state);
        if let Some(index) = self.slot
            && state.writes[index].is_some_and(|item| item.id == self.id)
        {
            state.writes[index] = None;
        }
        state.write_last_duration = Some(duration);
        state.write_max_duration = Some(
            state
                .write_max_duration
                .map_or(duration, |maximum| maximum.max(duration)),
        );
    }
}

impl Drop for WriteGuard {
    fn drop(&mut self) {
        self.finish_at(Instant::now());
    }
}

fn global() -> &'static StreamDiagnostics {
    static GLOBAL: OnceLock<StreamDiagnostics> = OnceLock::new();
    GLOBAL.get_or_init(StreamDiagnostics::new)
}

/// Register a stream in the process-global bounded registry.
#[must_use]
pub fn register(context: StreamContext) -> StreamRegistration {
    global().register(context)
}

/// Snapshot the process-global registry for an additive `GET_PERF` field.
#[must_use]
pub fn snapshot() -> StreamDiagnosticsSnapshot {
    global().snapshot()
}

/// Reset process-global interval observations without invalidating trackers.
pub fn reset() {
    global().reset();
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn elapsed_us(now: Instant, started: Instant) -> u64 {
    duration_us(now.saturating_duration_since(started))
}

fn duration_us(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(stream_id: u64) -> StreamContext {
        StreamContext {
            connection_id: 7,
            stream_id,
            lane: StreamLane::Terminal,
        }
    }

    #[test]
    fn cancelled_ticket_removes_queued_bytes() {
        let diagnostics = StreamDiagnostics::new();
        let registration = diagnostics.register(context(1));
        let tracker = registration.tracker();
        let ticket = tracker.enqueue(42);
        assert_eq!(diagnostics.snapshot().streams[0].queue_bytes, 42);

        drop(ticket);
        let stream = &diagnostics.snapshot().streams[0];
        assert_eq!(stream.queue_items, 0);
        assert_eq!(stream.queue_bytes, 0);
    }

    #[test]
    fn queue_age_and_out_of_order_ticket_drop_are_exact() {
        let diagnostics = StreamDiagnostics::new();
        let registration = diagnostics.register(context(2));
        let tracker = registration.tracker();
        let epoch = Instant::now();
        let oldest = tracker.enqueue_at(10, epoch);
        let newest = tracker.enqueue_at(20, epoch + Duration::from_millis(3));

        drop(newest);
        let snapshot = diagnostics.snapshot_at(epoch + Duration::from_millis(8));
        assert_eq!(snapshot.streams[0].queue_items, 1);
        assert_eq!(snapshot.streams[0].queue_bytes, 10);
        assert_eq!(snapshot.streams[0].queue_oldest_age_us, Some(8_000));

        drop(oldest);
        assert_eq!(diagnostics.snapshot().streams[0].queue_items, 0);
    }

    #[test]
    fn queue_metadata_overflow_is_counted_and_bounded() {
        let diagnostics = StreamDiagnostics::new();
        let registration = diagnostics.register(context(3));
        let tracker = registration.tracker();
        let tickets: Vec<_> = (0..=MAX_QUEUED_ITEMS).map(|_| tracker.enqueue(1)).collect();

        let stream = &diagnostics.snapshot().streams[0];
        assert_eq!(stream.queue_items, MAX_QUEUED_ITEMS as u64);
        assert_eq!(stream.queue_bytes, MAX_QUEUED_ITEMS as u64);
        assert_eq!(stream.queue_overflow, 1);
        drop(tickets);
    }

    #[test]
    fn write_guard_reports_in_progress_finish_and_cancellation() {
        let diagnostics = StreamDiagnostics::new();
        let registration = diagnostics.register(context(4));
        let tracker = registration.tracker();
        let epoch = Instant::now();
        let mut guard = tracker.begin_write_at(epoch);
        let during = diagnostics.snapshot_at(epoch + Duration::from_millis(5));
        assert_eq!(during.streams[0].write_in_progress_age_us, Some(5_000));

        guard.finish_at(epoch + Duration::from_millis(9));
        drop(guard);
        let finished = &diagnostics.snapshot().streams[0];
        assert_eq!(finished.write_in_progress_age_us, None);
        assert_eq!(finished.write_last_duration_us, Some(9_000));
        assert_eq!(finished.write_max_duration_us, Some(9_000));

        let cancelled = tracker.begin_write();
        drop(cancelled);
        assert!(
            diagnostics.snapshot().streams[0]
                .write_last_duration_us
                .is_some()
        );
    }

    #[test]
    fn registration_is_bounded_and_raii_slot_is_reused() {
        let diagnostics = StreamDiagnostics::new();
        let mut registrations: Vec<_> = (0..MAX_STREAMS)
            .map(|id| diagnostics.register(context(id as u64)))
            .collect();
        assert!(registrations.iter().all(StreamRegistration::is_registered));
        let suppressed = diagnostics.register(context(999));
        assert!(!suppressed.is_registered());
        let no_op = suppressed.tracker();
        assert_eq!(no_op.context(), context(999));
        assert!(
            no_op.inner.is_none(),
            "suppressed streams allocate no tracker storage"
        );
        assert!(no_op.enqueue(123).tracker.upgrade().is_none());
        assert!(no_op.begin_write().tracker.upgrade().is_none());
        let snapshot = diagnostics.snapshot();
        assert_eq!(snapshot.streams.len(), MAX_STREAMS);
        assert_eq!(snapshot.suppressed_streams, 1);

        registrations.pop();
        let replacement = diagnostics.register(context(1_000));
        assert!(replacement.is_registered());
        assert_eq!(diagnostics.snapshot().streams.len(), MAX_STREAMS);
    }

    #[test]
    fn reset_preserves_lifecycle_and_outstanding_metadata() {
        let diagnostics = StreamDiagnostics::new();
        let registration = diagnostics.register(context(5));
        let tracker = registration.tracker();
        tracker.set_active(true);
        tracker.record_ready_latency(ReadyKind::Initial, Duration::from_millis(4));
        tracker.record_resync(ResyncReason::SequenceGap);
        let ticket = tracker.enqueue(17);
        let write = tracker.begin_write();

        diagnostics.reset();
        let stream = &diagnostics.snapshot().streams[0];
        assert!(stream.active);
        assert_eq!(stream.queue_bytes, 17);
        assert!(stream.write_in_progress_age_us.is_some());
        assert_eq!(stream.ready, None);
        assert_eq!(stream.resync_reason, None);

        drop(write);
        drop(ticket);
    }

    #[test]
    fn dropping_registration_unregisters_even_if_tracker_survives() {
        let diagnostics = StreamDiagnostics::new();
        let registration = diagnostics.register(context(6));
        let tracker = registration.tracker();
        drop(registration);
        tracker.set_active(true);
        assert!(diagnostics.snapshot().streams.is_empty());
    }
}
