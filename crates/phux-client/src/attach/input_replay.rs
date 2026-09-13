//! Acknowledged-input replay journal for the interactive attach's remote
//! lanes (ADR-0053).
//!
//! The Ws/QUIC reconnect window (phux-i0e8.2.3, extended to the remote lanes
//! by the transport-aware reconnect work) resurrects the *session*, but any
//! input that was crossing the wire at the drop is simply gone — and ADR-0053
//! is explicit that replaying fire-and-forget `INPUT_*` frames is not a fix,
//! because a transport failure cannot reveal whether the first copy reached
//! the PTY. The acknowledged `APPLY_INPUT` surface exists for exactly this:
//! a consumer-generated 128-bit operation id names the batch, the
//! terminal-owning server caches the outcome by that id, and a same-id resend
//! after reconnect is answered from the cache instead of being written twice.
//!
//! This module is the client half of that contract for the attach TUI — the
//! native analogue of phux-mobile's `PendingInput` journal
//! (`rust/phux-mobile-ffi/src/wire/outbound.rs`), which is the reference
//! implementation this mirrors, invariant for invariant:
//!
//! - **One operation id per user action, forever.** A resend after reconnect
//!   reuses the id verbatim; a fresh id is precisely the duplicate the design
//!   exists to prevent.
//! - **Replay only against the same server incarnation** (`HELLO_OK.server_id`,
//!   ADR-0053 point 5). Dedupe state is process memory; a changed incarnation
//!   means the cache is gone, so an already-attempted operation resolves
//!   *unknown* — never a silent replay, never a silent drop.
//! - **Replay only inside [`INPUT_RETRY_HORIZON`]**, which matches the
//!   server's dedupe retention (`DEDUPE_RETENTION`,
//!   `phux-server/src/runtime/input_lane/acknowledged.rs`). Past it the
//!   server may have evicted the record, so a resend could write twice.
//! - **At most one operation per Terminal in flight**, submission order
//!   preserved independently for each Terminal. Unrelated Terminals may make
//!   progress while one waits, matching server admission scope.
//! - **Bounded retention.** Event and byte budgets turn overflow into an
//!   explicit refusal report rather than silently dropping input.
//! - **An attempted operation strands as *unknown*; a never-sent one as
//!   *refused*.** The distinction is the whole vocabulary: refused means
//!   nothing was written and retyping is safe, unknown means the pane must be
//!   read before anything is resent.
//!
//! The journal is deliberately transport- and UI-free: it holds state and
//! builds frames; the attach driver owns sending, receiving, and turning
//! [`ReplayReport`]s into status-bar notices. It is created per attach
//! invocation by the CLI's reconnect loop (`crates/phux`,
//! `attach_with_reconnect`) — for remote dials only — and shared across
//! attempts exactly like the `--rec` recorder, which is what lets an
//! operation outlive the socket that first carried it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use phux_protocol::ids::{InputOperationId, ResourceId};
use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind};

use crate::agent_prompt::operation_id_hex;

/// How long an unresolved operation remains eligible for a same-id resend.
///
/// Equal to the server's dedupe retention (`DEDUPE_RETENTION`, 10 minutes) and
/// to phux-mobile's `INPUT_RETRY_HORIZON`, and it must never exceed the
/// former: a resend after the server may have evicted the id-to-outcome
/// record is indistinguishable from a first send, which is the double-write
/// this journal exists to prevent.
pub const INPUT_RETRY_HORIZON: Duration = Duration::from_mins(10);

/// `HELLO_OK.server_id` length the protocol defines. Anything else is a peer
/// this journal must not trust with idempotency (mirrors the mobile bridge's
/// same check).
const SERVER_ID_LEN: usize = 16;
const MAX_DEFERRED_REPORTS: usize = 64;

/// Maximum number of input atoms retained across acknowledged operations.
pub const INPUT_JOURNAL_MAX_EVENTS: usize = 4096;
/// Maximum estimated in-memory bytes retained across acknowledged operations.
pub const INPUT_JOURNAL_MAX_BYTES: usize = 1024 * 1024;

/// One journaled acknowledged operation. The operation id and payload never
/// change; only connection-local bookkeeping (`attempts`, the in-flight
/// request id held by [`ConnectionContext`]) does.
#[derive(Debug)]
struct PendingOp {
    operation_id: InputOperationId,
    terminal_id: ResourceId,
    events: Vec<InputEvent>,
    /// The incarnation the first attempt was made against. `None` until the
    /// first attempt; bound at send time and compared on every reconnect.
    expected_server_id: Option<Vec<u8>>,
    created_at: Instant,
    /// Whether any attempt reached a socket. Decides the stranding verdict:
    /// attempted strands *unknown*, never-sent strands *refused*.
    attempts: u32,
    retained_bytes: usize,
}

#[derive(Debug)]
struct InFlightAttempt {
    operation_id: InputOperationId,
    terminal_id: ResourceId,
}

/// Per-connection state, reset by [`InputReplayJournal::begin_connection`].
#[derive(Debug)]
struct ConnectionContext {
    server_id: Vec<u8>,
    /// Outstanding attempts by request id. Dies with the connection — the
    /// operations themselves do not.
    in_flight: HashMap<u32, InFlightAttempt>,
}

/// How a journaled operation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayDisposition {
    /// The server acknowledged the write (possibly from its dedupe cache on
    /// a replay — indistinguishable by design, and equally true).
    Delivered,
    /// Some, all, or none of the bytes may have reached the pane, and no
    /// same-id retry can ever say which. The honest recovery is to read the
    /// pane before retyping.
    Unknown,
    /// Nothing was written; retyping the input is safe.
    Refused,
}

/// One resolved operation, for the driver to surface (or stay silent about —
/// [`ReplayDisposition::Delivered`] warrants no chrome).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayReport {
    /// How the operation ended.
    pub disposition: ReplayDisposition,
    /// Lowercase hex of the operation id — the only durable handle a user or
    /// a server log has on what happened.
    pub operation_id: String,
    /// Diagnostic detail (server message or local stranding cause).
    pub message: String,
}

impl ReplayReport {
    /// The status-bar line for a non-delivered outcome.
    #[must_use]
    pub fn notice_line(&self) -> String {
        let verdict = match self.disposition {
            ReplayDisposition::Delivered => "delivered",
            ReplayDisposition::Unknown => "delivery unknown — read the pane before retyping",
            ReplayDisposition::Refused => "not delivered — safe to retype",
        };
        if self.message.is_empty() {
            format!("paste {verdict} (op {})", self.operation_id)
        } else {
            format!(
                "paste {verdict} (op {}): {}",
                self.operation_id, self.message
            )
        }
    }
}

/// The journal. See the module docs for the contract; every public method is
/// synchronous and non-blocking so it can live inside the attach driver's
/// select loop without adding an arm.
#[derive(Debug)]
pub struct InputReplayJournal {
    /// Global submission order; the earliest due operation for each Terminal
    /// is independently eligible for an attempt.
    ops: VecDeque<PendingOp>,
    /// `Some` between [`Self::begin_connection`] and
    /// [`Self::connection_lost`] — i.e. while there is a live, negotiated
    /// socket whose incarnation is known and which advertised
    /// `ACKNOWLEDGED_INPUT`.
    connection: Option<ConnectionContext>,
    retained_events: usize,
    retained_bytes: usize,
    deferred_reports: VecDeque<ReplayReport>,
    suppressed_reports: [usize; 3],
}

impl Default for InputReplayJournal {
    fn default() -> Self {
        Self::new()
    }
}

impl InputReplayJournal {
    /// An empty journal with no live connection.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ops: VecDeque::new(),
            connection: None,
            retained_events: 0,
            retained_bytes: 0,
            deferred_reports: VecDeque::new(),
            suppressed_reports: [0; 3],
        }
    }

    /// Adopt a (re)connected socket's identity and decide every queued
    /// operation's fate against it.
    ///
    /// Called once per `main_loop` entry — a fresh dial after the reconnect
    /// window and an in-connection session switch both pass through here,
    /// and both need the same treatment: any in-flight correlation is dead
    /// (the reply either died with the socket or was discarded by the
    /// session-switch drain), while the operations themselves survive and
    /// are re-decided:
    ///
    /// - no usable incarnation, or no `ACKNOWLEDGED_INPUT` — every queued
    ///   operation strands (attempted ⇒ unknown, never-sent ⇒ refused) and
    ///   the journal deactivates until the next connection;
    /// - incarnation changed since an operation's first attempt — that
    ///   operation strands the same way (ADR-0053 point 5);
    /// - horizon expired — strands;
    /// - otherwise the operation stays queued for [`Self::next_frame`],
    ///   which will resend it under its original id. The server's dedupe
    ///   cache is what makes that resend idempotent — including the
    ///   session-switch case, where the first attempt's reply was already
    ///   emitted and dropped.
    pub fn begin_connection(
        &mut self,
        server_id: Option<&[u8]>,
        acknowledged_input: bool,
    ) -> Vec<ReplayReport> {
        let now = Instant::now();
        let usable = server_id.filter(|id| id.len() == SERVER_ID_LEN && acknowledged_input);
        let Some(server_id) = usable else {
            self.connection = None;
            let why = if acknowledged_input {
                "the server did not provide a usable incarnation identity"
            } else {
                "the server does not support acknowledged input"
            };
            return self.strand_all(why);
        };
        self.connection = Some(ConnectionContext {
            server_id: server_id.to_vec(),
            in_flight: HashMap::new(),
        });
        self.sweep(now)
    }

    /// Whether a paste should take the acknowledged path right now.
    #[must_use]
    pub const fn active(&self) -> bool {
        self.connection.is_some()
    }

    /// The live socket is gone. In-flight correlation dies; operations stay,
    /// to be re-decided by the next [`Self::begin_connection`] (or drained by
    /// [`Self::drain_unresolved`] if no reconnect succeeds).
    pub fn connection_lost(&mut self) {
        self.connection = None;
    }

    /// Journal one acknowledged batch. The operation id is minted here, once,
    /// and never again for this batch.
    pub fn submit(
        &mut self,
        terminal_id: ResourceId,
        events: Vec<InputEvent>,
    ) -> Result<(), ReplayReport> {
        let operation_id = mint_operation_id();
        if let Err(refusal) = crate::agent_prompt::validate_batch(&events) {
            let report = ReplayReport {
                disposition: ReplayDisposition::Refused,
                operation_id: operation_id_hex(&operation_id),
                message: refusal.to_string(),
            };
            self.defer_report(report.clone());
            return Err(report);
        }
        let retained_bytes = retained_event_bytes(&events, events.capacity());
        if events.is_empty()
            || self.retained_events.saturating_add(events.len()) > INPUT_JOURNAL_MAX_EVENTS
            || self.retained_bytes.saturating_add(retained_bytes) > INPUT_JOURNAL_MAX_BYTES
        {
            let report = ReplayReport {
                disposition: ReplayDisposition::Refused,
                operation_id: operation_id_hex(&operation_id),
                message: format!(
                    "acknowledged-input queue full (max {INPUT_JOURNAL_MAX_EVENTS} events / {INPUT_JOURNAL_MAX_BYTES} bytes)"
                ),
            };
            self.defer_report(report.clone());
            return Err(report);
        }
        self.retained_events += events.len();
        self.retained_bytes += retained_bytes;
        self.ops.push_back(PendingOp {
            operation_id,
            terminal_id,
            events,
            expected_server_id: None,
            created_at: Instant::now(),
            attempts: 0,
            retained_bytes,
        });
        Ok(())
    }

    /// Whether later input for `terminal_id` must join the journal to remain
    /// behind an acknowledged operation already queued or in flight.
    #[must_use]
    pub fn must_order_after(&self, terminal_id: &ResourceId) -> bool {
        self.ops.iter().any(|op| &op.terminal_id == terminal_id)
    }

    /// Drain locally generated outcomes such as bounded-queue overflow. The
    /// attach driver calls this after input dispatch and maps each report to
    /// the same visible notice path used for server replies.
    pub fn take_reports(&mut self) -> Vec<ReplayReport> {
        let mut reports: Vec<_> = self.deferred_reports.drain(..).collect();
        for (disposition, message) in [
            (
                ReplayDisposition::Delivered,
                "additional delivered input outcomes were coalesced",
            ),
            (
                ReplayDisposition::Unknown,
                "additional input outcomes with unknown delivery were coalesced",
            ),
            (
                ReplayDisposition::Refused,
                "additional local input refusals were coalesced",
            ),
        ] {
            let count = self.suppressed_reports[disposition_index(disposition)];
            if count > 0 {
                reports.push(ReplayReport {
                    disposition,
                    operation_id: "multiple".to_owned(),
                    message: format!("{count} {message}"),
                });
            }
        }
        self.suppressed_reports = [0; 3];
        reports
    }

    /// Preserve reports a lower layer observed until the driver's visible
    /// notice drain runs.
    pub fn defer_reports(&mut self, reports: Vec<ReplayReport>) {
        for report in reports {
            self.defer_report(report);
        }
    }

    /// Whether `request_id` correlates to this journal's outstanding attempt.
    #[must_use]
    pub fn owns(&self, request_id: u32) -> bool {
        self.connection
            .as_ref()
            .is_some_and(|ctx| ctx.in_flight.contains_key(&request_id))
    }

    /// Build the next `APPLY_INPUT` attempt, if one is due.
    ///
    /// Serialized: nothing is built while an attempt is outstanding. Expired
    /// operations encountered at the front strand (reported) rather than
    /// being sent past the server's dedupe retention. The returned frame has
    /// already been recorded as in flight under a request id drawn from
    /// `next_request_id` — the caller's only obligation is to put it on the
    /// wire (a send failure ends in [`Self::connection_lost`] anyway).
    pub fn next_frame(
        &mut self,
        next_request_id: &mut u32,
    ) -> (Vec<ReplayReport>, Option<FrameKind>) {
        let (reports, mut frames) = self.build_frames(next_request_id, 1);
        (reports, frames.pop())
    }

    /// Build one due attempt per Terminal. Independent terminals can therefore
    /// progress while an earlier operation waits for its PTY outcome.
    pub fn next_frames(
        &mut self,
        next_request_id: &mut u32,
    ) -> (Vec<ReplayReport>, Vec<FrameKind>) {
        self.build_frames(next_request_id, usize::MAX)
    }

    /// Roll back a suffix returned by [`Self::next_frames`] that the caller
    /// never handed to its connection.
    ///
    /// A sequential sender must exclude the frame whose `send` returned an
    /// error: that handoff is uncertain. Only later frames are provably
    /// unsent. Rolling those back removes their connection-local request ids
    /// and reverses this build's attempt, while preserving any earlier attempt
    /// (and therefore its unknown-delivery risk) across a replay.
    pub fn rollback_unsent(&mut self, frames: &[FrameKind]) {
        for frame in frames {
            let FrameKind::Command { request_id, .. } = frame else {
                continue;
            };
            let Some(attempt) = self
                .connection
                .as_mut()
                .and_then(|ctx| ctx.in_flight.remove(request_id))
            else {
                continue;
            };
            let Some(op) = self
                .ops
                .iter_mut()
                .find(|op| op.operation_id == attempt.operation_id)
            else {
                continue;
            };
            op.attempts = op.attempts.saturating_sub(1);
            if op.attempts == 0 {
                op.expected_server_id = None;
            }
        }
    }

    fn build_frames(
        &mut self,
        next_request_id: &mut u32,
        limit: usize,
    ) -> (Vec<ReplayReport>, Vec<FrameKind>) {
        let mut reports = self.take_reports();
        reports.extend(self.sweep_expired(Instant::now()));
        let Some(ctx) = self.connection.as_mut() else {
            return (reports, Vec::new());
        };
        let server_id = ctx.server_id.clone();
        let mut busy: HashSet<_> = ctx
            .in_flight
            .values()
            .map(|attempt| attempt.terminal_id.clone())
            .collect();
        let mut frames = Vec::new();
        for op in &mut self.ops {
            if frames.len() == limit || !busy.insert(op.terminal_id.clone()) {
                continue;
            }
            let request_id = *next_request_id;
            *next_request_id = next_request_id.wrapping_add(1);
            op.attempts = op.attempts.saturating_add(1);
            op.expected_server_id
                .get_or_insert_with(|| server_id.clone());
            frames.push(FrameKind::Command {
                request_id,
                command: Command::ApplyInput {
                    operation_id: op.operation_id,
                    terminal_id: op.terminal_id.clone(),
                    events: op.events.clone(),
                },
            });
            ctx.in_flight.insert(
                request_id,
                InFlightAttempt {
                    operation_id: op.operation_id,
                    terminal_id: op.terminal_id.clone(),
                },
            );
        }
        (reports, frames)
    }

    /// Fold one `COMMAND_RESULT` for the outstanding attempt into a verdict.
    ///
    /// `Ok` is the receipt. On a first attempt, explicit server errors retain
    /// their pre-handoff refusal meaning except `INPUT_DELIVERY_UNKNOWN`. On a
    /// replay, every non-OK answer is unknown: an older compatible server can
    /// refuse the retry before consulting dedupe even though the original may
    /// have written, so the retry's refusal cannot establish operation-level
    /// certainty.
    ///
    /// Returns `None` for a request id this journal does not own.
    pub fn resolve(&mut self, request_id: u32, result: &CommandResult) -> Option<ReplayReport> {
        if !self.owns(request_id) {
            return None;
        }
        let attempt = self.connection.as_mut()?.in_flight.remove(&request_id)?;
        let position = self
            .ops
            .iter()
            .position(|op| op.operation_id == attempt.operation_id)?;
        let op = self.remove_at(position)?;
        let (disposition, message) = match result {
            CommandResult::Ok | CommandResult::OkWith(_) => {
                (ReplayDisposition::Delivered, String::new())
            }
            CommandResult::Error { code, message } => (
                if *code == ErrorCode::InputDeliveryUnknown || op.attempts > 1 {
                    ReplayDisposition::Unknown
                } else {
                    ReplayDisposition::Refused
                },
                message.clone(),
            ),
            // `CommandResult` is `#[non_exhaustive]`: a reply this build
            // cannot read is not evidence that nothing was written.
            _ => (
                ReplayDisposition::Unknown,
                "the server answered APPLY_INPUT with a result this build cannot read".to_owned(),
            ),
        };
        Some(ReplayReport {
            disposition,
            operation_id: operation_id_hex(&op.operation_id),
            message,
        })
    }

    /// Resolve everything still queued — the no-more-reconnects teardown.
    pub fn drain_unresolved(&mut self, why: &str) -> Vec<ReplayReport> {
        self.connection = None;
        self.strand_all(why)
    }

    /// Strand queued operations that can no longer be replayed against the
    /// current connection: expired, or first-attempted against a different
    /// incarnation.
    fn sweep(&mut self, now: Instant) -> Vec<ReplayReport> {
        let server_id = self
            .connection
            .as_ref()
            .map(|ctx| ctx.server_id.clone())
            .unwrap_or_default();
        let mut reports = Vec::new();
        let mut kept = VecDeque::with_capacity(self.ops.len());
        while let Some(op) = self.ops.pop_front() {
            if now.duration_since(op.created_at) >= INPUT_RETRY_HORIZON {
                reports.push(strand_report(
                    &op,
                    "the acknowledged-input retry horizon expired",
                ));
            } else if op
                .expected_server_id
                .as_ref()
                .is_some_and(|expected| *expected != server_id)
            {
                reports.push(strand_report(&op, "the server restarted in between"));
            } else {
                kept.push_back(op);
                continue;
            }
            self.release_accounting(&op);
        }
        self.ops = kept;
        reports
    }

    fn strand_all(&mut self, why: &str) -> Vec<ReplayReport> {
        let mut reports = Vec::with_capacity(self.ops.len());
        reports.extend(self.take_reports());
        while let Some(op) = self.ops.pop_front() {
            reports.push(strand_report(&op, why));
            self.release_accounting(&op);
        }
        reports
    }

    fn sweep_expired(&mut self, now: Instant) -> Vec<ReplayReport> {
        let mut reports = Vec::new();
        let mut index = 0;
        while index < self.ops.len() {
            if now.duration_since(self.ops[index].created_at) < INPUT_RETRY_HORIZON {
                index += 1;
                continue;
            }
            let Some(op) = self.remove_at(index) else {
                break;
            };
            reports.push(strand_report(
                &op,
                "the acknowledged-input retry horizon expired",
            ));
        }
        reports
    }

    fn remove_at(&mut self, index: usize) -> Option<PendingOp> {
        let op = self.ops.remove(index)?;
        if let Some(ctx) = self.connection.as_mut() {
            ctx.in_flight
                .retain(|_, attempt| attempt.operation_id != op.operation_id);
        }
        self.release_accounting(&op);
        Some(op)
    }

    const fn release_accounting(&mut self, op: &PendingOp) {
        self.retained_events = self.retained_events.saturating_sub(op.events.len());
        self.retained_bytes = self.retained_bytes.saturating_sub(op.retained_bytes);
    }

    fn defer_report(&mut self, report: ReplayReport) {
        if self.deferred_reports.len() < MAX_DEFERRED_REPORTS {
            self.deferred_reports.push_back(report);
        } else {
            let count = &mut self.suppressed_reports[disposition_index(report.disposition)];
            *count = count.saturating_add(1);
        }
    }
}

const fn disposition_index(disposition: ReplayDisposition) -> usize {
    match disposition {
        ReplayDisposition::Delivered => 0,
        ReplayDisposition::Unknown => 1,
        ReplayDisposition::Refused => 2,
    }
}

/// The stranding verdict: an attempted operation is *unknown* (its bytes may
/// be in the pane), a never-sent one is a deterministic *refusal*.
fn strand_report(op: &PendingOp, message: &str) -> ReplayReport {
    ReplayReport {
        disposition: if op.attempts > 0 {
            ReplayDisposition::Unknown
        } else {
            ReplayDisposition::Refused
        },
        operation_id: operation_id_hex(&op.operation_id),
        message: message.to_owned(),
    }
}

fn retained_event_bytes(events: &[InputEvent], allocation_slots: usize) -> usize {
    events.iter().fold(
        allocation_slots.saturating_mul(std::mem::size_of::<InputEvent>()),
        |total, event| {
            let payload = match event {
                InputEvent::Key(key) => key.text.as_ref().map_or(0, String::capacity),
                InputEvent::Paste(paste) => paste.data.capacity(),
                _ => 0,
            };
            total.saturating_add(payload)
        },
    )
}

/// A fresh non-zero 128-bit operation id (ADR-0053 point 2). UUID v4 supplies
/// the OS-sourced randomness; the loop covers the astronomically unlikely
/// all-zero draw the wire type refuses.
fn mint_operation_id() -> InputOperationId {
    loop {
        if let Some(id) = InputOperationId::new(uuid::Uuid::new_v4().into_bytes()) {
            return id;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

    use super::*;

    const SERVER_A: [u8; 16] = [0xAA; 16];
    const SERVER_B: [u8; 16] = [0xBB; 16];

    fn tid(n: u32) -> ResourceId {
        ResourceId::local(n)
    }

    fn paste(text: &str) -> Vec<InputEvent> {
        use phux_protocol::input::paste::{PasteEvent, PasteTrust};
        vec![InputEvent::Paste(PasteEvent {
            trust: PasteTrust::Untrusted,
            data: text.as_bytes().to_vec(),
        })]
    }

    fn enter() -> Vec<InputEvent> {
        use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
        vec![InputEvent::Key(KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::Enter,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        })]
    }

    fn armed_journal() -> InputReplayJournal {
        let mut journal = InputReplayJournal::new();
        assert!(journal.begin_connection(Some(&SERVER_A), true).is_empty());
        journal
    }

    /// Pull the outstanding attempt's frame pieces or panic.
    fn send_one(journal: &mut InputReplayJournal, next: &mut u32) -> (u32, InputOperationId) {
        let (reports, frame) = journal.next_frame(next);
        assert!(reports.is_empty(), "{reports:?}");
        match frame.expect("an attempt is due") {
            FrameKind::Command {
                request_id,
                command:
                    Command::ApplyInput {
                        operation_id,
                        terminal_id: _,
                        events: _,
                    },
            } => (request_id, operation_id),
            other => panic!("not an APPLY_INPUT: {other:?}"),
        }
    }

    // ---- the id is the contract -------------------------------------

    /// A resend after a lost socket reuses the SAME operation id under a
    /// fresh request id. This is the whole point of the journal: the id is
    /// what lets the server's dedupe cache answer instead of writing twice.
    #[test]
    fn a_replay_reuses_the_operation_id_and_not_the_request_id() {
        let mut journal = armed_journal();
        journal.submit(tid(1), paste("ship it")).expect("queue");
        let mut next = 1_u32;
        let (first_request, first_op) = send_one(&mut journal, &mut next);

        journal.connection_lost();
        let reports = journal.begin_connection(Some(&SERVER_A), true);
        assert!(reports.is_empty(), "{reports:?}");

        let (second_request, second_op) = send_one(&mut journal, &mut next);
        assert_eq!(first_op, second_op, "a retry must never mint a fresh id");
        assert_ne!(
            first_request, second_request,
            "the request id is connection-local and must not be reused"
        );
    }

    /// Only one attempt is outstanding at a time; the second queued paste
    /// goes on the wire only after the first resolves.
    #[test]
    fn attempts_are_serialized_in_submission_order() {
        let mut journal = armed_journal();
        journal.submit(tid(1), paste("first")).expect("queue");
        journal.submit(tid(1), paste("second")).expect("queue");
        let mut next = 1_u32;
        let (request, _) = send_one(&mut journal, &mut next);
        let (reports, frame) = journal.next_frame(&mut next);
        assert!(reports.is_empty() && frame.is_none(), "{frame:?}");

        let report = journal
            .resolve(request, &CommandResult::Ok)
            .expect("owned request id");
        assert_eq!(report.disposition, ReplayDisposition::Delivered);

        let (second_request, _) = send_one(&mut journal, &mut next);
        assert!(journal.owns(second_request));
    }

    #[test]
    fn delayed_paste_keeps_later_paste_and_enter_in_terminal_order() {
        let mut journal = armed_journal();
        journal.submit(tid(1), paste("A")).expect("queue A");
        let mut next = 1;
        let (a_request, _) = send_one(&mut journal, &mut next);
        journal.submit(tid(1), paste("B")).expect("queue B");
        journal.submit(tid(1), enter()).expect("queue Enter");
        let (_, blocked) = journal.next_frames(&mut next);
        assert!(blocked.is_empty(), "one Terminal has one active operation");

        journal
            .resolve(a_request, &CommandResult::Ok)
            .expect("resolve A");
        let (_, b_frames) = journal.next_frames(&mut next);
        assert_eq!(b_frames.len(), 1);
        let b_request = command_parts(&b_frames[0]).0;
        assert!(matches!(
            command_parts(&b_frames[0]).1[0],
            InputEvent::Paste(_)
        ));

        journal
            .resolve(b_request, &CommandResult::Ok)
            .expect("resolve B");
        let (_, enter_frames) = journal.next_frames(&mut next);
        assert_eq!(enter_frames.len(), 1);
        assert!(matches!(
            command_parts(&enter_frames[0]).1[0],
            InputEvent::Key(_)
        ));
    }

    #[test]
    fn an_unrelated_terminal_progresses_while_first_terminal_waits() {
        let mut journal = armed_journal();
        journal.submit(tid(1), paste("held")).expect("queue held");
        let mut next = 1;
        let _ = send_one(&mut journal, &mut next);
        journal.submit(tid(2), paste("other")).expect("queue other");
        let (_, frames) = journal.next_frames(&mut next);
        assert_eq!(frames.len(), 1);
        assert_eq!(command_parts(&frames[0]).2, &tid(2));
    }

    #[test]
    fn unsent_suffix_rolls_back_to_definite_refusal_after_send_failure() {
        let mut journal = armed_journal();
        for terminal in 1..=3 {
            journal
                .submit(tid(terminal), paste(&terminal.to_string()))
                .expect("queue");
        }
        let (_, frames) = journal.next_frames(&mut 1);
        assert_eq!(frames.len(), 3);

        // Sending A failed. A reached the transport API, but B/C never did.
        journal.rollback_unsent(&frames[1..]);
        let reports = journal.drain_unresolved("the connection send failed");
        let dispositions: Vec<_> = reports.iter().map(|report| report.disposition).collect();
        assert_eq!(
            dispositions,
            vec![
                ReplayDisposition::Unknown,
                ReplayDisposition::Refused,
                ReplayDisposition::Refused,
            ]
        );
    }

    #[test]
    fn rollback_of_replay_suffix_preserves_prior_attempt_uncertainty() {
        let mut journal = armed_journal();
        for terminal in 1..=3 {
            journal
                .submit(tid(terminal), paste(&terminal.to_string()))
                .expect("queue");
        }
        let _ = journal.next_frames(&mut 1);
        journal.connection_lost();
        assert!(journal.begin_connection(Some(&SERVER_A), true).is_empty());

        let (_, replay_frames) = journal.next_frames(&mut 10);
        assert_eq!(replay_frames.len(), 3);
        journal.rollback_unsent(&replay_frames[1..]);
        let reports = journal.drain_unresolved("the replay send failed");
        assert!(
            reports
                .iter()
                .all(|report| report.disposition == ReplayDisposition::Unknown),
            "every operation retains its prior attempt: {reports:?}"
        );
    }

    #[test]
    fn queue_overflow_is_explicit_and_does_not_consume_capacity() {
        let mut journal = armed_journal();
        let mut refused = None;
        while refused.is_none() {
            let batch = vec![InputEvent::Paste(phux_protocol::input::paste::PasteEvent {
                trust: phux_protocol::input::paste::PasteTrust::Trusted,
                data: vec![b'x'; 60 * 1024],
            })];
            refused = journal.submit(tid(1), batch).err();
        }
        let report = refused.expect("journal byte cap must refuse");
        assert_eq!(report.disposition, ReplayDisposition::Refused);
        assert!(report.notice_line().contains("queue full"));
        let (reports, frames) = journal.next_frames(&mut 1);
        assert_eq!(reports, vec![report]);
        assert_eq!(frames.len(), 1);
        let _ = journal.drain_unresolved("test cleanup");
        assert_eq!(journal.retained_events, 0);
        assert_eq!(journal.retained_bytes, 0);
        journal
            .submit(tid(1), paste("fits"))
            .expect("capacity remains");
    }

    #[test]
    fn queue_event_count_is_bounded_independently_of_payload_bytes() {
        let mut journal = armed_journal();
        for _ in 0..(INPUT_JOURNAL_MAX_EVENTS / phux_protocol::MAX_APPLY_INPUT_EVENTS) {
            journal
                .submit(
                    tid(1),
                    vec![
                        InputEvent::Focus(phux_protocol::input::focus::FocusEvent::Gained);
                        phux_protocol::MAX_APPLY_INPUT_EVENTS
                    ],
                )
                .expect("within aggregate event cap");
        }
        let report = journal
            .submit(
                tid(1),
                vec![InputEvent::Focus(
                    phux_protocol::input::focus::FocusEvent::Gained,
                )],
            )
            .expect_err("aggregate event cap");
        assert_eq!(report.disposition, ReplayDisposition::Refused);
        assert_eq!(journal.retained_events, INPUT_JOURNAL_MAX_EVENTS);
    }

    #[test]
    fn coalesced_mixed_reports_preserve_delivery_certainty() {
        let mut journal = InputReplayJournal::new();
        for index in 0..MAX_DEFERRED_REPORTS {
            journal.defer_report(ReplayReport {
                disposition: ReplayDisposition::Refused,
                operation_id: index.to_string(),
                message: "bounded refusal".to_owned(),
            });
        }
        journal.defer_reports(vec![
            ReplayReport {
                disposition: ReplayDisposition::Unknown,
                operation_id: "unknown".to_owned(),
                message: "socket closed after send".to_owned(),
            },
            ReplayReport {
                disposition: ReplayDisposition::Refused,
                operation_id: "refused".to_owned(),
                message: "queue full".to_owned(),
            },
        ]);

        let reports = journal.take_reports();
        let summaries = &reports[MAX_DEFERRED_REPORTS..];
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].disposition, ReplayDisposition::Unknown);
        assert!(!summaries[0].notice_line().contains("safe to retype"));
        assert_eq!(summaries[1].disposition, ReplayDisposition::Refused);
        assert!(summaries[1].notice_line().contains("safe to retype"));
        assert!(journal.take_reports().is_empty());
    }

    fn command_parts(frame: &FrameKind) -> (u32, &[InputEvent], &ResourceId) {
        let FrameKind::Command {
            request_id,
            command:
                Command::ApplyInput {
                    terminal_id,
                    events,
                    ..
                },
        } = frame
        else {
            panic!("not APPLY_INPUT: {frame:?}");
        };
        (*request_id, events, terminal_id)
    }

    // ---- point 5: incarnation binding --------------------------------

    /// An attempted operation must not be replayed against a different
    /// incarnation: the dedupe cache died with the old process, so the only
    /// honest verdict is unknown.
    #[test]
    fn an_attempted_op_strands_unknown_when_the_incarnation_changes() {
        let mut journal = armed_journal();
        journal.submit(tid(1), paste("ship it")).expect("queue");
        let mut next = 1_u32;
        let _ = send_one(&mut journal, &mut next);

        journal.connection_lost();
        let reports = journal.begin_connection(Some(&SERVER_B), true);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].disposition, ReplayDisposition::Unknown);
        let (more, frame) = journal.next_frame(&mut next);
        assert!(more.is_empty() && frame.is_none());
    }

    /// A never-sent operation carries no incarnation binding: it is simply
    /// sent to whichever server is there now. Nothing was ever written, so
    /// there is nothing to double.
    #[test]
    fn a_never_sent_op_survives_an_incarnation_change() {
        let mut journal = armed_journal();
        journal
            .submit(tid(1), paste("queued while offline"))
            .expect("queue");
        journal.connection_lost();
        let reports = journal.begin_connection(Some(&SERVER_B), true);
        assert!(reports.is_empty(), "{reports:?}");
        let mut next = 1_u32;
        let (_, op) = send_one(&mut journal, &mut next);
        let _ = op;
    }

    /// A reconnected server without `ACKNOWLEDGED_INPUT` (or with a malformed
    /// incarnation id) can honor nothing: everything strands, by the
    /// attempted/never-sent rule.
    #[test]
    fn a_server_without_the_feature_strands_everything() {
        let mut journal = armed_journal();
        journal.submit(tid(1), paste("attempted")).expect("queue");
        let mut next = 1_u32;
        let _ = send_one(&mut journal, &mut next);
        journal.submit(tid(1), paste("never sent")).expect("queue");

        journal.connection_lost();
        let reports = journal.begin_connection(Some(&SERVER_A), false);
        let dispositions: Vec<_> = reports.iter().map(|r| r.disposition).collect();
        assert_eq!(
            dispositions,
            vec![ReplayDisposition::Unknown, ReplayDisposition::Refused]
        );
        assert!(!journal.active());
    }

    #[test]
    fn a_short_server_id_is_not_a_usable_incarnation() {
        let mut journal = InputReplayJournal::new();
        journal.submit(tid(1), paste("queued")).expect("queue");
        let reports = journal.begin_connection(Some(&[0xAA; 4]), true);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].disposition, ReplayDisposition::Refused);
        assert!(!journal.active());
    }

    // ---- the horizon --------------------------------------------------

    /// An operation older than the horizon is never resent — the server's
    /// dedupe record may be evicted, so a resend could write twice. It
    /// strands by the attempted/never-sent rule instead.
    #[test]
    fn the_horizon_strands_instead_of_resending() {
        let mut journal = armed_journal();
        journal.submit(tid(1), paste("stale")).expect("queue");
        journal.ops.front_mut().expect("just submitted").created_at = Instant::now()
            .checked_sub(INPUT_RETRY_HORIZON)
            .expect("the clock supports ten minutes ago");
        let mut next = 1_u32;
        let (reports, frame) = journal.next_frame(&mut next);
        assert!(frame.is_none());
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].disposition, ReplayDisposition::Refused);
    }

    /// The same expiry applies at reconnect: a stale attempted op is
    /// unknown, and the fresh one behind it still replays.
    #[test]
    fn reconnect_expires_the_stale_and_replays_the_fresh() {
        let mut journal = armed_journal();
        journal.submit(tid(1), paste("stale")).expect("queue");
        let mut next = 1_u32;
        let _ = send_one(&mut journal, &mut next);
        journal.submit(tid(1), paste("fresh")).expect("queue");
        journal.ops.front_mut().expect("two queued").created_at = Instant::now()
            .checked_sub(INPUT_RETRY_HORIZON)
            .expect("the clock supports ten minutes ago");

        journal.connection_lost();
        let reports = journal.begin_connection(Some(&SERVER_A), true);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].disposition, ReplayDisposition::Unknown);
        let (more, frame) = journal.next_frame(&mut next);
        assert!(more.is_empty());
        assert!(frame.is_some(), "the fresh op must still be attempted");
    }

    // ---- verdicts -----------------------------------------------------

    /// The result classification is the mobile reference's: OK is the
    /// receipt, `INPUT_DELIVERY_UNKNOWN` is terminal-unknown, anything else
    /// wrote nothing.
    #[test]
    fn verdicts_mirror_the_reference_classification() {
        for (result, expected) in [
            (CommandResult::Ok, ReplayDisposition::Delivered),
            (
                CommandResult::Error {
                    code: ErrorCode::InputDeliveryUnknown,
                    message: "writer stalled".to_owned(),
                },
                ReplayDisposition::Unknown,
            ),
            (
                CommandResult::Error {
                    code: ErrorCode::ResourceExhausted,
                    message: "another client holds the slot".to_owned(),
                },
                ReplayDisposition::Refused,
            ),
            (
                CommandResult::Error {
                    code: ErrorCode::UnsafePaste,
                    message: "policy".to_owned(),
                },
                ReplayDisposition::Refused,
            ),
        ] {
            let mut journal = armed_journal();
            journal.submit(tid(1), paste("x")).expect("queue");
            let mut next = 1_u32;
            let (request, _) = send_one(&mut journal, &mut next);
            let report = journal.resolve(request, &result).expect("owned");
            assert_eq!(report.disposition, expected, "{result:?}");
        }
    }

    #[test]
    fn ordinary_error_on_a_retry_never_claims_safe_to_retype() {
        for code in [ErrorCode::ResourceExhausted, ErrorCode::InputNotWritten] {
            let mut journal = armed_journal();
            journal.submit(tid(1), paste("x")).expect("queue");
            let mut next = 1;
            let _ = send_one(&mut journal, &mut next);
            journal.connection_lost();
            assert!(journal.begin_connection(Some(&SERVER_A), true).is_empty());
            let (retry_request, _) = send_one(&mut journal, &mut next);
            let report = journal
                .resolve(
                    retry_request,
                    &CommandResult::Error {
                        code,
                        message: "retry refusal".to_owned(),
                    },
                )
                .expect("retry result");
            assert_eq!(report.disposition, ReplayDisposition::Unknown, "{code:?}");
            assert!(!report.notice_line().contains("safe to retype"));
        }
    }

    /// A result for a request id the journal does not own is not consumed —
    /// it belongs to some other correlation and must fall through to
    /// whatever owns it.
    #[test]
    fn foreign_request_ids_are_not_consumed() {
        let mut journal = armed_journal();
        journal.submit(tid(1), paste("x")).expect("queue");
        let mut next = 1_u32;
        let (request, _) = send_one(&mut journal, &mut next);
        assert!(journal.resolve(request + 7, &CommandResult::Ok).is_none());
        assert!(journal.owns(request), "the real attempt must stay pending");
    }

    /// Final teardown: whatever is left resolves by the attempted/never-sent
    /// rule so the user hears about every journaled paste exactly once.
    #[test]
    fn drain_unresolved_reports_every_op_once() {
        let mut journal = armed_journal();
        journal.submit(tid(1), paste("attempted")).expect("queue");
        let mut next = 1_u32;
        let _ = send_one(&mut journal, &mut next);
        journal.submit(tid(2), paste("never sent")).expect("queue");
        let reports = journal.drain_unresolved("the reconnect window closed");
        let dispositions: Vec<_> = reports.iter().map(|r| r.disposition).collect();
        assert_eq!(
            dispositions,
            vec![ReplayDisposition::Unknown, ReplayDisposition::Refused]
        );
        assert!(journal.drain_unresolved("again").is_empty());
    }

    /// The session-switch shape: same connection re-enters `main_loop`, so
    /// `begin_connection` runs again with the SAME incarnation while an
    /// attempt is outstanding (its reply was discarded by the switch drain).
    /// The op must be resent under its original id, not stranded — the
    /// server's cache answers it.
    #[test]
    fn a_same_incarnation_reentry_replays_an_outstanding_attempt() {
        let mut journal = armed_journal();
        journal.submit(tid(1), paste("mid-switch")).expect("queue");
        let mut next = 1_u32;
        let (_, first_op) = send_one(&mut journal, &mut next);

        // No connection_lost: the socket survived; only the correlation was
        // dropped by the drain.
        let reports = journal.begin_connection(Some(&SERVER_A), true);
        assert!(reports.is_empty(), "{reports:?}");
        let (_, second_op) = send_one(&mut journal, &mut next);
        assert_eq!(first_op, second_op);
    }
}
