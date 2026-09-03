//! Acknowledged-input bookkeeping: admission, dedupe, and the completion
//! waiter (ADR-0053, phux-w7z2.58).
//!
//! `APPLY_INPUT` is the one input surface that must answer *what happened*
//! rather than fire and forget, so it needs three pieces of state the
//! fire-and-forget surfaces do not: a gate that keeps two unresolved
//! operations from racing at one pane, a dedupe record so a reconnecting
//! client can safely resend, and a bounded wait for the PTY writer's verdict.
//!
//! ## Why admission is keyed by Terminal
//!
//! The first implementation admitted **one operation per server** and blocked
//! the lane thread on the PTY completion. Verification of phux-w7z2.29 traced
//! the two consequences: concurrent `APPLY_INPUT` to *unrelated* panes
//! collided into `RESOURCE_EXHAUSTED`, and one pane whose child had stopped
//! reading stdin froze the lane — every attached keystroke, for every pane, for
//! the full completion timeout, silently dropping attached input once the
//! lane's queue filled. Nothing downstream of the lane is shared (the pane
//! mailbox, the writer channel, and the writer thread are all per pane), so
//! that serialization was an artifact of the lane, not a property of the PTY
//! layer.
//!
//! Admission is therefore a set of Terminals with an unresolved operation, and
//! the completion wait happens on a separate thread that owns every pending
//! operation at once. The lane thread's only remaining acknowledged work is
//! synchronous: validate, bind the dedupe record, encode, register, hand off.
//!
//! ## What still serializes, on purpose
//!
//! Per Terminal, at most one operation is unresolved: the reservation is held
//! from admission until the completion (or timeout) is finalized. That is what
//! makes the dedupe record's `Pending` state unreachable while a write is in
//! flight, which is what keeps a same-id retry from writing twice.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use phux_protocol::ids::InputOperationId;
use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use crate::terminal_actor::{WriteCompletion, WriteCompletionSink};

pub(super) const DEDUPE_MAX_ENTRIES: usize = 65_536;
pub(super) const DEDUPE_RETENTION: Duration = Duration::from_secs(10 * 60);
pub(super) const ACKNOWLEDGED_COMPLETION_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Admission
// ---------------------------------------------------------------------------

/// The set of Terminals with an unresolved acknowledged operation.
///
/// Shared by every [`InputLaneHandle`](super::InputLaneHandle) clone, so the
/// gate is server-wide in *reach* while being per-Terminal in *scope*: two
/// clients targeting one pane still exclude each other.
#[derive(Debug, Default)]
pub(super) struct AcknowledgedAdmission {
    in_flight: Mutex<HashSet<phux_protocol::ids::TerminalId>>,
}

impl AcknowledgedAdmission {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<phux_protocol::ids::TerminalId>> {
        self.in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(super) fn is_in_flight(&self, terminal_id: &phux_protocol::ids::TerminalId) -> bool {
        self.lock().contains(terminal_id)
    }
}

/// Proof that this Terminal admitted the operation, released on drop.
///
/// Held from the caller's admission check all the way through the completion
/// wait, so the window it covers is exactly "this Terminal has an unresolved
/// acknowledged operation".
#[derive(Debug)]
#[allow(
    clippy::redundant_pub_crate,
    reason = "escapes this private module as a field of the pub(crate) RoutedInputKind"
)]
pub(crate) struct AcknowledgedReservation {
    admission: Arc<AcknowledgedAdmission>,
    terminal_id: phux_protocol::ids::TerminalId,
    admitted_at: Instant,
}

impl AcknowledgedReservation {
    pub(super) fn try_acquire(
        admission: &Arc<AcknowledgedAdmission>,
        terminal_id: &phux_protocol::ids::TerminalId,
    ) -> Option<Self> {
        if !admission.lock().insert(terminal_id.clone()) {
            return None;
        }
        Some(Self {
            admission: Arc::clone(admission),
            terminal_id: terminal_id.clone(),
            admitted_at: Instant::now(),
        })
    }

    /// The dedupe record's retention clock starts here, not at completion, so
    /// a client's ten-minute horizon is measured from the moment the server
    /// took the operation.
    pub(super) const fn admitted_at(&self) -> Instant {
        self.admitted_at
    }
}

impl Drop for AcknowledgedReservation {
    fn drop(&mut self) {
        self.admission.lock().remove(&self.terminal_id);
    }
}

// ---------------------------------------------------------------------------
// Dedupe record
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct CachedOperation {
    digest: [u8; 32],
    result: Option<CommandResult>,
    inserted_at: Instant,
}

#[derive(Debug, PartialEq)]
pub(super) enum CacheBinding {
    New,
    Pending,
    Final(CommandResult),
    Conflict,
    Full,
}

#[derive(Debug, Default)]
struct OperationCache {
    entries: HashMap<InputOperationId, CachedOperation>,
    expiry: std::collections::VecDeque<(Instant, InputOperationId)>,
}

impl OperationCache {
    fn prune_expired(&mut self, now: Instant) {
        while self.expiry.front().is_some_and(|(inserted_at, _)| {
            now.saturating_duration_since(*inserted_at) >= DEDUPE_RETENTION
        }) {
            let Some((inserted_at, operation_id)) = self.expiry.pop_front() else {
                break;
            };
            if self
                .entries
                .get(&operation_id)
                .is_some_and(|entry| entry.inserted_at == inserted_at)
            {
                self.entries.remove(&operation_id);
            }
        }
    }

    fn is_full(&self) -> bool {
        self.entries.len() >= DEDUPE_MAX_ENTRIES
    }

    fn bind_at(
        &mut self,
        operation_id: InputOperationId,
        digest: [u8; 32],
        admitted_at: Instant,
    ) -> CacheBinding {
        self.prune_expired(admitted_at);
        if let Some(cached) = self.entries.get(&operation_id) {
            if cached.digest != digest {
                return CacheBinding::Conflict;
            }
            return cached
                .result
                .as_ref()
                .map_or(CacheBinding::Pending, |result| {
                    CacheBinding::Final(result.clone())
                });
        }
        if self.is_full() {
            return CacheBinding::Full;
        }
        self.entries.insert(
            operation_id,
            CachedOperation {
                digest,
                result: None,
                inserted_at: admitted_at,
            },
        );
        self.expiry.push_back((admitted_at, operation_id));
        CacheBinding::New
    }

    fn set_final(&mut self, operation_id: InputOperationId, result: CommandResult) {
        if let Some(cached) = self.entries.get_mut(&operation_id) {
            cached.result = Some(result);
        }
    }
}

/// The dedupe record, shared between the lane thread (which binds ids) and the
/// completion waiter (which writes final results).
#[derive(Clone, Debug, Default)]
pub(super) struct SharedOperationCache(Arc<Mutex<OperationCache>>);

impl SharedOperationCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, OperationCache> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn bind_at(
        &self,
        operation_id: InputOperationId,
        digest: [u8; 32],
        admitted_at: Instant,
    ) -> CacheBinding {
        self.lock().bind_at(operation_id, digest, admitted_at)
    }

    fn set_final(&self, operation_id: InputOperationId, result: CommandResult) {
        self.lock().set_final(operation_id, result);
    }
}

pub(super) fn operation_digest(
    operation_id: InputOperationId,
    terminal_id: &phux_protocol::ids::TerminalId,
    events: Vec<InputEvent>,
) -> [u8; 32] {
    let frame = FrameKind::Command {
        request_id: 0,
        command: Command::ApplyInput {
            operation_id,
            terminal_id: terminal_id.clone(),
            events,
        },
    };
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    Sha256::digest(&encoded).into()
}

// ---------------------------------------------------------------------------
// Completion waiter
// ---------------------------------------------------------------------------

/// Names one registered operation on the completion queue.
///
/// Monotonic and never reused, so a completion that arrives after its
/// operation was already resolved (a late writer reply past the timeout, or a
/// request dropped after the handoff was abandoned) matches nothing and is
/// discarded instead of resolving somebody else's operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct CompletionTicket(u64);

/// Mints tickets. Lives on the lane thread; not shared.
#[derive(Debug, Default)]
pub(super) struct TicketSource(u64);

impl TicketSource {
    pub(super) const fn next_ticket(&mut self) -> CompletionTicket {
        self.0 = self.0.wrapping_add(1);
        CompletionTicket(self.0)
    }
}

/// One operation handed to the writer and awaiting its verdict.
#[derive(Debug)]
pub(super) struct PendingCompletion {
    ticket: CompletionTicket,
    operation_id: InputOperationId,
    deadline: Instant,
    reservation: AcknowledgedReservation,
    reply: oneshot::Sender<CommandResult>,
}

impl PendingCompletion {
    pub(super) const fn new(
        ticket: CompletionTicket,
        operation_id: InputOperationId,
        deadline: Instant,
        reservation: AcknowledgedReservation,
        reply: oneshot::Sender<CommandResult>,
    ) -> Self {
        Self {
            ticket,
            operation_id,
            deadline,
            reservation,
            reply,
        }
    }

    /// Resolve without consulting the writer: the lane refused the operation
    /// after registering it but before (or instead of) handing it off.
    pub(super) fn resolve_without_writer(self, result: CommandResult) {
        // No `set_final`: a pre-handoff refusal keeps the id-to-digest binding
        // but not the refusal result, so the unchanged operation may be
        // evaluated again once its cause is repaired (SPEC L1 §6.2.1).
        drop(self.reservation);
        let _ = self.reply.send(result);
    }

    fn finalize(self, cache: &SharedOperationCache, result: CommandResult) {
        // Order matters: the dedupe record must carry the final result before
        // the reservation is released, or a retry admitted in the gap would
        // read `Pending` and write the same batch a second time.
        cache.set_final(self.operation_id, result.clone());
        drop(self.reservation);
        let _ = self.reply.send(result);
    }
}

#[derive(Debug)]
enum WaiterMessage {
    Registered(Box<PendingCompletion>),
    Completed {
        ticket: CompletionTicket,
        outcome: WriteCompletion,
    },
    Abandoned {
        ticket: CompletionTicket,
        result: CommandResult,
    },
    Shutdown,
}

/// The lane thread's channel to the completion waiter.
#[derive(Clone, Debug)]
pub(super) struct CompletionWaiterHandle {
    tx: std::sync::mpsc::Sender<WaiterMessage>,
}

impl CompletionWaiterHandle {
    /// Register an operation that is about to be handed off.
    ///
    /// Order is load-bearing and easy to get wrong: this call MUST complete
    /// **before** the request reaches the pane's mailbox. Registration and the
    /// writer's completion travel the same queue, and the writer cannot begin
    /// its send until the handoff it observes has happened — which is after
    /// this send returned — so the registration always takes the earlier slot.
    /// Registering *after* the handoff would let a fast write report against a
    /// ticket the waiter has never seen, which the waiter discards, leaving the
    /// caller to wait out the full completion timeout for a write that
    /// succeeded.
    ///
    /// # Errors
    ///
    /// Returns the operation back if the waiter thread is gone (shutdown); the
    /// caller answers it directly.
    pub(super) fn register(&self, pending: PendingCompletion) -> Result<(), PendingCompletion> {
        self.tx
            .send(WaiterMessage::Registered(Box::new(pending)))
            .map_err(|err| match err.0 {
                WaiterMessage::Registered(pending) => *pending,
                _ => unreachable!("only a Registered message is sent here"),
            })
    }

    /// The writer's report path for `ticket`. Dropped unfired, it reports
    /// [`WriteCompletion::Failed`].
    pub(super) fn sink(&self, ticket: CompletionTicket) -> WriteCompletionSink {
        let tx = self.tx.clone();
        WriteCompletionSink::new(move |outcome| {
            let _ = tx.send(WaiterMessage::Completed { ticket, outcome });
        })
    }

    /// Resolve a registered operation that never reached the writer.
    ///
    /// Must be called *before* the unsent request (and so its sink) is
    /// dropped: the sink's drop reports `Failed`, and whichever message
    /// reaches the waiter first wins.
    pub(super) fn abandon(&self, ticket: CompletionTicket, result: CommandResult) {
        let _ = self.tx.send(WaiterMessage::Abandoned { ticket, result });
    }
}

/// Owns the completion-waiter thread. Dropping it stops the thread and drops
/// every pending operation, which closes its caller's reply channel.
#[derive(Debug)]
pub(super) struct CompletionWaiter {
    handle: CompletionWaiterHandle,
    join: Option<std::thread::JoinHandle<()>>,
}

impl CompletionWaiter {
    /// Spawn the waiter thread.
    ///
    /// # Errors
    ///
    /// Returns the OS error if the thread cannot be spawned.
    pub(super) fn spawn(cache: SharedOperationCache) -> std::io::Result<Self> {
        let (tx, rx) = std::sync::mpsc::channel();
        let join = std::thread::Builder::new()
            .name("phux-input-ack".to_owned())
            .spawn(move || {
                crate::perf::promote_helper_thread("phux-input-ack");
                run_waiter(&rx, &cache);
            })?;
        Ok(Self {
            handle: CompletionWaiterHandle { tx },
            join: Some(join),
        })
    }

    pub(super) fn handle(&self) -> CompletionWaiterHandle {
        self.handle.clone()
    }
}

impl Drop for CompletionWaiter {
    fn drop(&mut self) {
        // Every outstanding sink also holds a sender, and a sink can outlive
        // the server on a pane writer thread, so channel closure is not a
        // reliable stop signal. Say so explicitly instead.
        let _ = self.handle.tx.send(WaiterMessage::Shutdown);
        if let Some(join) = self.join.take()
            && let Err(err) = join.join()
        {
            tracing::warn!(?err, "acknowledged completion waiter panicked on shutdown");
        }
    }
}

fn completion_result(outcome: WriteCompletion) -> CommandResult {
    match outcome {
        WriteCompletion::Delivered => CommandResult::Ok,
        WriteCompletion::CanonicalLimitExceeded { limit } => CommandResult::Error {
            code: ErrorCode::CanonicalLimitExceeded,
            message: format!(
                "the pane is in canonical (cooked) mode and this batch's encoded PTY \
                 bytes contain a line longer than {limit} bytes with no line \
                 terminator; split it into newline-terminated lines, or have the pane \
                 enter raw mode (e.g. run a program that disables ICANON) before \
                 sending it"
            ),
        },
        WriteCompletion::Failed => delivery_unknown(),
        WriteCompletion::NotWritten => not_written(),
    }
}

fn delivery_unknown() -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::InputDeliveryUnknown,
        message: "PTY input delivery could not be confirmed".to_owned(),
    }
}

/// phux-w7z2.60: the request never reached a live PTY writer, so `write(2)`
/// was never invoked for it — proven at the point [`WriteCompletion::NotWritten`]
/// was raised, not inferred here. Distinct from [`delivery_unknown`]: that
/// reading forbids a retry under any id, this one does not, because there is
/// nothing already written for a retry to duplicate.
fn not_written() -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::InputNotWritten,
        message: "PTY input was not written; the pane's writer never received it".to_owned(),
    }
}

/// Pending operations, indexed by ticket and ordered by deadline.
#[derive(Default)]
struct PendingSet {
    by_ticket: HashMap<CompletionTicket, PendingCompletion>,
    by_deadline: BTreeSet<(Instant, CompletionTicket)>,
}

impl PendingSet {
    fn insert(&mut self, pending: PendingCompletion) {
        self.by_deadline.insert((pending.deadline, pending.ticket));
        self.by_ticket.insert(pending.ticket, pending);
    }

    fn take(&mut self, ticket: CompletionTicket) -> Option<PendingCompletion> {
        let pending = self.by_ticket.remove(&ticket)?;
        self.by_deadline.remove(&(pending.deadline, ticket));
        Some(pending)
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.by_deadline.first().map(|(deadline, _)| *deadline)
    }

    /// Take everything already past its deadline, oldest first.
    fn take_expired(&mut self, now: Instant) -> Vec<PendingCompletion> {
        let mut expired = Vec::new();
        while let Some(&(deadline, ticket)) = self.by_deadline.first() {
            if deadline > now {
                break;
            }
            self.by_deadline.remove(&(deadline, ticket));
            if let Some(pending) = self.by_ticket.remove(&ticket) {
                expired.push(pending);
            }
        }
        expired
    }
}

fn run_waiter(rx: &std::sync::mpsc::Receiver<WaiterMessage>, cache: &SharedOperationCache) {
    let mut pending = PendingSet::default();
    loop {
        // Park until the next event, or until the earliest deadline expires.
        // Nothing here touches the lane thread, so a wedged PTY writer costs
        // its own operation and nothing else.
        let message = match pending.next_deadline() {
            Some(deadline) => {
                match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(message) => Some(message),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            None => match rx.recv() {
                Ok(message) => Some(message),
                Err(_) => break,
            },
        };
        for expired in pending.take_expired(Instant::now()) {
            // Bounded wait expired after handoff: the batch may still land
            // later, so the result is unknown rather than failed, and it is
            // cached as final because a same-id retry must not write again.
            expired.finalize(cache, delivery_unknown());
        }
        match message {
            None => {}
            Some(WaiterMessage::Shutdown) => break,
            Some(WaiterMessage::Registered(registered)) => pending.insert(*registered),
            Some(WaiterMessage::Completed { ticket, outcome }) => {
                if let Some(operation) = pending.take(ticket) {
                    operation.finalize(cache, completion_result(outcome));
                }
            }
            Some(WaiterMessage::Abandoned { ticket, result }) => {
                if let Some(operation) = pending.take(ticket) {
                    operation.resolve_without_writer(result);
                }
            }
        }
    }
    tracing::debug!("acknowledged completion waiter exiting");
}

/// Bound the completion wait for an operation registered now. An unrepresentable
/// deadline (only reachable near `Instant`'s ceiling) expires immediately rather
/// than panicking, which costs one operation an `INPUT_DELIVERY_UNKNOWN`.
pub(super) fn deadline_from(now: Instant, completion_timeout: Duration) -> Instant {
    now.checked_add(completion_timeout).unwrap_or(now)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn operation_id(byte: u8) -> InputOperationId {
        InputOperationId::new([byte; 16]).expect("non-zero operation id")
    }

    fn operation_id_from_u64(value: u64) -> InputOperationId {
        let mut bytes = [0; 16];
        bytes[8..].copy_from_slice(&value.to_be_bytes());
        InputOperationId::new(bytes).expect("non-zero operation id")
    }

    /// phux-w7z2.60: `NotWritten` and `Failed` both mean the batch did not
    /// land, but they are not the same reading. `Failed` — a real write was
    /// attempted and something went wrong partway through — stays
    /// `InputDeliveryUnknown`: a same-id retry replays it, a fresh-id retry
    /// risks a duplicate. `NotWritten` — the request never reached a writer at
    /// all — is a distinct, honest code, and this is the one place the two
    /// map to `CommandResult`, so pin the split here rather than only at the
    /// call sites that raise each `WriteCompletion` variant.
    #[test]
    fn not_written_and_failed_map_to_distinct_error_codes() {
        assert_eq!(
            completion_result(WriteCompletion::NotWritten),
            CommandResult::Error {
                code: ErrorCode::InputNotWritten,
                message: not_written_message(),
            }
        );
        assert_eq!(
            completion_result(WriteCompletion::Failed),
            CommandResult::Error {
                code: ErrorCode::InputDeliveryUnknown,
                message: "PTY input delivery could not be confirmed".to_owned(),
            }
        );
        assert_ne!(
            completion_result(WriteCompletion::NotWritten),
            completion_result(WriteCompletion::Failed),
        );
    }

    fn not_written_message() -> String {
        let CommandResult::Error { message, .. } = not_written() else {
            unreachable!("not_written always returns Error");
        };
        message
    }

    /// Two Terminals admit independently; one Terminal excludes itself until
    /// the reservation is released.
    #[test]
    fn admission_is_scoped_to_one_terminal() {
        let admission = Arc::new(AcknowledgedAdmission::default());
        let first = phux_protocol::TerminalId::local(1);
        let second = phux_protocol::TerminalId::local(2);

        let held = AcknowledgedReservation::try_acquire(&admission, &first).expect("first admits");
        assert!(
            AcknowledgedReservation::try_acquire(&admission, &first).is_none(),
            "one Terminal admits one unresolved operation"
        );
        let other =
            AcknowledgedReservation::try_acquire(&admission, &second).expect("second admits");
        assert!(admission.is_in_flight(&first) && admission.is_in_flight(&second));

        drop(held);
        assert!(!admission.is_in_flight(&first));
        assert!(
            AcknowledgedReservation::try_acquire(&admission, &first).is_some(),
            "releasing the reservation readmits the Terminal"
        );
        drop(other);
    }

    #[test]
    fn operation_cache_prunes_only_expired_prefix_and_treats_over_capacity_as_full() {
        let start = Instant::now();
        let first = operation_id_from_u64(1);
        let second = operation_id_from_u64(2);
        let mut cache = OperationCache::default();
        assert_eq!(cache.bind_at(first, [1; 32], start), CacheBinding::New);
        cache.set_final(first, CommandResult::Ok);
        assert_eq!(
            cache.bind_at(second, [2; 32], start + Duration::from_secs(1)),
            CacheBinding::New
        );
        cache.set_final(second, CommandResult::Ok);
        cache.prune_expired(start + DEDUPE_RETENTION);
        assert!(!cache.entries.contains_key(&first));
        assert!(cache.entries.contains_key(&second));
        assert_eq!(cache.expiry.len(), 1);

        for value in 3..=u64::try_from(DEDUPE_MAX_ENTRIES + 1).unwrap() {
            assert_eq!(
                cache.bind_at(
                    operation_id_from_u64(value),
                    [0; 32],
                    start + Duration::from_secs(1),
                ),
                CacheBinding::New
            );
        }
        assert_eq!(cache.entries.len(), DEDUPE_MAX_ENTRIES);
        assert!(cache.is_full(), "capacity check must use >=");
        assert_eq!(
            cache.bind_at(
                operation_id_from_u64(u64::try_from(DEDUPE_MAX_ENTRIES + 2).unwrap()),
                [0; 32],
                start + Duration::from_secs(1),
            ),
            CacheBinding::Full
        );
        assert_eq!(cache.entries.len(), DEDUPE_MAX_ENTRIES);
    }

    #[test]
    fn cache_uses_admission_time_for_retry_at_expiry_boundary() {
        let start = Instant::now();
        let id = operation_id(18);
        let digest = [0x18; 32];
        let mut cache = OperationCache::default();
        assert_eq!(cache.bind_at(id, digest, start), CacheBinding::New);
        cache.set_final(id, CommandResult::Ok);

        let admitted_before_expiry = start
            + DEDUPE_RETENTION
                .checked_sub(Duration::from_nanos(1))
                .expect("retention exceeds one nanosecond");
        let processed_after_expiry = start + DEDUPE_RETENTION + Duration::from_secs(1);
        assert!(processed_after_expiry > start + DEDUPE_RETENTION);
        let binding = cache.bind_at(id, digest, admitted_before_expiry);
        cache.prune_expired(processed_after_expiry);
        assert_eq!(
            binding,
            CacheBinding::Final(CommandResult::Ok),
            "a retry valid at admission must use the cached result without rebinding"
        );
        assert!(cache.expiry.is_empty());
    }

    /// Deadlines are assigned in registration order, so the waiter's ordered
    /// index must hand back the earliest first regardless of insertion order.
    #[test]
    fn pending_set_orders_by_deadline_and_forgets_taken_tickets() {
        let admission = Arc::new(AcknowledgedAdmission::default());
        let mut tickets = TicketSource::default();
        let now = Instant::now();
        let mut pending = PendingSet::default();

        let mut register = |pending: &mut PendingSet, terminal: u32, offset_ms: u64| {
            let terminal_id = phux_protocol::TerminalId::local(terminal);
            let reservation = AcknowledgedReservation::try_acquire(&admission, &terminal_id)
                .expect("distinct terminals admit");
            // The receiver is dropped immediately: this test exercises the
            // ordered index, not delivery.
            let (reply, _rx) = oneshot::channel();
            let ticket = tickets.next_ticket();
            pending.insert(PendingCompletion::new(
                ticket,
                operation_id(terminal.try_into().expect("small")),
                now + Duration::from_millis(offset_ms),
                reservation,
                reply,
            ));
            ticket
        };

        let early = register(&mut pending, 1, 10);
        let late = register(&mut pending, 2, 30);
        let middle = register(&mut pending, 3, 20);

        assert_eq!(
            pending.next_deadline(),
            Some(now + Duration::from_millis(10))
        );
        assert!(pending.take(middle).is_some());
        assert!(
            pending.take(middle).is_none(),
            "a taken ticket must not resolve twice"
        );
        assert_eq!(
            pending.take_expired(now + Duration::from_millis(15)).len(),
            1,
            "only the earliest deadline has passed"
        );
        assert_eq!(
            pending.next_deadline(),
            Some(now + Duration::from_millis(30))
        );
        assert!(pending.take(early).is_none());
        assert!(pending.take(late).is_some());
        assert!(pending.next_deadline().is_none());
    }
}
