//! Acknowledged-input bookkeeping for `APPLY_INPUT` (ADR-0053): per-Terminal
//! admission, the dedupe record, and the completion waiter.
//!
//! Admission is keyed by Terminal and the completion wait runs on its own
//! thread (phux-w7z2.58): serializing per server made unrelated panes collide
//! and let one pane whose child stopped reading stdin freeze every keystroke
//! for the full completion timeout. Per Terminal, at most one operation is
//! unresolved, from admission until its completion or timeout is finalized;
//! that keeps a same-id retry from writing twice.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use phux_protocol::ids::InputOperationId;
use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use crate::runtime::operation_dedupe::{
    CachedOutcome, Claim, OperationDedupe, OperationDomain, OperationKey, Waiter,
};
use crate::terminal_actor::{WriteCompletion, WriteCompletionSink};

pub(super) const ACKNOWLEDGED_COMPLETION_TIMEOUT: Duration = Duration::from_secs(5);

/// The Terminals with an unresolved acknowledged operation, shared by every
/// lane handle: two clients targeting one pane still exclude each other.
#[derive(Debug, Default)]
pub(super) struct AcknowledgedAdmission {
    in_flight: Mutex<HashSet<phux_protocol::ids::ResourceId>>,
}

impl AcknowledgedAdmission {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<phux_protocol::ids::ResourceId>> {
        self.in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(super) fn is_in_flight(&self, terminal_id: &phux_protocol::ids::ResourceId) -> bool {
        self.lock().contains(terminal_id)
    }
}

/// Proof that this Terminal admitted the operation, held through the
/// completion wait and released on drop.
#[derive(Debug)]
#[allow(
    clippy::redundant_pub_crate,
    reason = "escapes this private module as a field of the pub(crate) RoutedInputKind"
)]
pub(crate) struct AcknowledgedReservation {
    admission: Arc<AcknowledgedAdmission>,
    terminal_id: phux_protocol::ids::ResourceId,
}

impl AcknowledgedReservation {
    pub(super) fn try_acquire(
        admission: &Arc<AcknowledgedAdmission>,
        terminal_id: &phux_protocol::ids::ResourceId,
    ) -> Option<Self> {
        if !admission.lock().insert(terminal_id.clone()) {
            return None;
        }
        Some(Self {
            admission: Arc::clone(admission),
            terminal_id: terminal_id.clone(),
        })
    }
}

impl Drop for AcknowledgedReservation {
    fn drop(&mut self) {
        self.admission.lock().remove(&self.terminal_id);
    }
}

#[derive(Debug)]
pub(super) enum CacheClaim {
    Owner,
    Pending(oneshot::Receiver<CommandResult>),
    PendingUncertain,
    Final(CommandResult),
    Conflict,
    Full,
}

/// The `APPLY_INPUT` view of the server's shared dedupe record
/// ([`OperationDedupe`], ADR-0126), mapped onto `CommandResult`. The lane
/// thread binds ids; the completion waiter writes final results.
#[derive(Clone, Debug, Default)]
pub(super) struct SharedOperationCache(OperationDedupe);

impl SharedOperationCache {
    /// View `dedupe` as the input lane's record.
    pub(super) const fn new(dedupe: OperationDedupe) -> Self {
        Self(dedupe)
    }

    const fn key(operation_id: InputOperationId) -> OperationKey {
        OperationKey::new(OperationDomain::Input, *operation_id.as_bytes())
    }

    pub(super) fn claim_at(
        &self,
        operation_id: InputOperationId,
        digest: [u8; 32],
        admitted_at: Instant,
    ) -> CacheClaim {
        match self
            .0
            .claim_at(Self::key(operation_id), digest, admitted_at, join_input)
        {
            Claim::Owner => CacheClaim::Owner,
            Claim::Pending(result) => CacheClaim::Pending(result),
            Claim::PendingUncertain => CacheClaim::PendingUncertain,
            Claim::Final(CachedOutcome::Input(result)) => CacheClaim::Final(result),
            // The input domain only ever records input results.
            Claim::Final(_) | Claim::Conflict => CacheClaim::Conflict,
            Claim::Full => CacheClaim::Full,
        }
    }

    pub(super) fn set_final(&self, operation_id: InputOperationId, result: &CommandResult) {
        self.0.set_final(
            Self::key(operation_id),
            &CachedOutcome::Input(result.clone()),
        );
    }

    pub(super) fn set_retryable(&self, operation_id: InputOperationId, result: &CommandResult) {
        self.0.set_retryable(
            Self::key(operation_id),
            &CachedOutcome::Input(result.clone()),
        );
    }
}

/// A same-id retry joining an unresolved operation: it receives the result
/// the operation resolves with.
fn join_input() -> (Waiter, oneshot::Receiver<CommandResult>) {
    let (reply, result) = oneshot::channel();
    let waiter: Waiter = Box::new(move |outcome: &CachedOutcome| {
        if let CachedOutcome::Input(outcome) = outcome {
            let _ = reply.send(outcome.clone());
        }
    });
    (waiter, result)
}

pub(super) fn operation_digest(
    operation_id: InputOperationId,
    terminal_id: &phux_protocol::ids::ResourceId,
    events: Vec<InputEvent>,
) -> ([u8; 32], Vec<InputEvent>) {
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
    let digest = Sha256::digest(&encoded).into();
    let FrameKind::Command {
        command: Command::ApplyInput { events, .. },
        ..
    } = frame
    else {
        unreachable!("constructed APPLY_INPUT frame changed variant")
    };
    (digest, events)
}

/// Names one registered operation. Never reused, so a late completion for an
/// already-resolved operation matches nothing instead of someone else's.
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
    pub(super) fn resolve_without_writer(
        self,
        cache: &SharedOperationCache,
        result: CommandResult,
    ) {
        // No `set_final`: a pre-handoff refusal keeps the id-to-digest binding
        // but not the refusal result, so the unchanged operation may be
        // evaluated again once its cause is repaired (SPEC L1 §6.2.1).
        drop(self.reservation);
        cache.set_retryable(self.operation_id, &result);
        let _ = self.reply.send(result);
    }

    fn finalize(self, cache: &SharedOperationCache, result: CommandResult) {
        // Order matters: the dedupe record must carry the final result before
        // the reservation is released, or a retry admitted in the gap would
        // read `Pending` and write the same batch a second time.
        cache.set_final(self.operation_id, &result);
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
    /// Must complete **before** the request reaches the pane's mailbox:
    /// registration and the writer's completion share one queue, so
    /// registering first guarantees a fast write never reports against a
    /// ticket the waiter has not seen (which would strand the caller until
    /// the timeout).
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

pub(super) fn delivery_unknown() -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::InputDeliveryUnknown,
        message: "PTY input delivery could not be confirmed".to_owned(),
    }
}

/// The request provably never reached a PTY writer, so unlike
/// [`delivery_unknown`] a retry under any id cannot duplicate it.
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
                    operation.resolve_without_writer(cache, result);
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

    #[test]
    fn operation_digest_is_canonical_without_cloning_event_payloads() {
        use phux_protocol::input::paste::{PasteEvent, PasteTrust};

        let operation_id = operation_id(19);
        let terminal_id = phux_protocol::ResourceId::local(7);
        let events = vec![InputEvent::Paste(PasteEvent {
            trust: PasteTrust::Trusted,
            data: vec![b'x'; 4096],
        })];
        let InputEvent::Paste(paste) = &events[0] else {
            unreachable!()
        };
        let payload_ptr = paste.data.as_ptr();
        let reference = FrameKind::Command {
            request_id: 0,
            command: Command::ApplyInput {
                operation_id,
                terminal_id: terminal_id.clone(),
                events: events.clone(),
            },
        };
        let mut encoded = BytesMut::new();
        reference.encode(&mut encoded);
        let expected: [u8; 32] = Sha256::digest(&encoded).into();

        let (actual, recovered) = operation_digest(operation_id, &terminal_id, events);
        assert_eq!(actual, expected, "digest remains the canonical frame hash");
        let InputEvent::Paste(recovered_paste) = &recovered[0] else {
            unreachable!()
        };
        assert_eq!(
            recovered_paste.data.as_ptr(),
            payload_ptr,
            "digesting transfers and recovers the event allocation instead of cloning it"
        );
    }

    /// phux-w7z2.60: a partial write (`Failed`) is unknown and must not be
    /// retried under a fresh id; a request that never reached a writer
    /// (`NotWritten`) has its own code.
    #[test]
    fn not_written_and_failed_map_to_distinct_error_codes() {
        assert_eq!(
            completion_result(WriteCompletion::NotWritten),
            not_written()
        );
        assert_eq!(
            completion_result(WriteCompletion::Failed),
            delivery_unknown()
        );
        assert!(matches!(
            not_written(),
            CommandResult::Error {
                code: ErrorCode::InputNotWritten,
                ..
            }
        ));
    }

    /// The facade keeps the lane's reading of the shared record: a pending
    /// retry receives the owner's final result, a refusal before handoff
    /// reaches the waiters without being cached, and a repeat after it owns
    /// the operation again. The record's own bounds are pinned in
    /// `runtime::operation_dedupe`.
    #[test]
    fn facade_maps_the_shared_record_onto_command_results() {
        let cache = SharedOperationCache::default();
        let id = operation_id(21);
        let now = Instant::now();
        assert!(matches!(
            cache.claim_at(id, [0x21; 32], now),
            CacheClaim::Owner
        ));
        let CacheClaim::Pending(mut waiter) = cache.claim_at(id, [0x21; 32], now) else {
            panic!("a same-id retry joins the unresolved operation");
        };
        let refusal = not_written();
        cache.set_retryable(id, &refusal);
        assert_eq!(waiter.try_recv().expect("refusal delivered"), refusal);
        assert!(matches!(
            cache.claim_at(id, [0x21; 32], now),
            CacheClaim::Owner
        ));
        cache.set_final(id, &CommandResult::Ok);
        assert!(matches!(
            cache.claim_at(id, [0x21; 32], now),
            CacheClaim::Final(CommandResult::Ok)
        ));
        assert!(matches!(
            cache.claim_at(id, [0x22; 32], now),
            CacheClaim::Conflict
        ));
        assert!(matches!(
            cache.claim_at(operation_id_from_u64(22), [0x22; 32], now),
            CacheClaim::Owner
        ));
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
            let terminal_id = phux_protocol::ResourceId::local(terminal);
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
