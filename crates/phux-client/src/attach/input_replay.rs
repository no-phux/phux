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
//! - **At most one operation in flight**, submission order preserved. The
//!   server admits one unresolved operation per Terminal; a serialized queue
//!   means the journal never manufactures its own `RESOURCE_EXHAUSTED`.
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

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use phux_protocol::ids::{InputOperationId, TerminalId};
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
pub const INPUT_RETRY_HORIZON: Duration = Duration::from_secs(10 * 60);

/// `HELLO_OK.server_id` length the protocol defines. Anything else is a peer
/// this journal must not trust with idempotency (mirrors the mobile bridge's
/// same check).
const SERVER_ID_LEN: usize = 16;

/// One journaled acknowledged operation. The operation id and payload never
/// change; only connection-local bookkeeping (`attempted`, the in-flight
/// request id held by [`ConnectionContext`]) does.
#[derive(Debug)]
struct PendingOp {
    operation_id: InputOperationId,
    terminal_id: TerminalId,
    events: Vec<InputEvent>,
    /// The incarnation the first attempt was made against. `None` until the
    /// first attempt; bound at send time and compared on every reconnect.
    expected_server_id: Option<Vec<u8>>,
    created_at: Instant,
    /// Whether any attempt reached a socket. Decides the stranding verdict:
    /// attempted strands *unknown*, never-sent strands *refused*.
    attempted: bool,
}

/// Per-connection state, reset by [`InputReplayJournal::begin_connection`].
#[derive(Debug)]
struct ConnectionContext {
    server_id: Vec<u8>,
    /// Request id of the front operation's outstanding attempt, if any. Dies
    /// with the connection — the operation itself does not.
    in_flight: Option<u32>,
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
    /// Front = oldest = the only operation eligible for an attempt.
    ops: VecDeque<PendingOp>,
    /// `Some` between [`Self::begin_connection`] and
    /// [`Self::connection_lost`] — i.e. while there is a live, negotiated
    /// socket whose incarnation is known and which advertised
    /// `ACKNOWLEDGED_INPUT`.
    connection: Option<ConnectionContext>,
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
            in_flight: None,
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
    pub fn submit(&mut self, terminal_id: TerminalId, events: Vec<InputEvent>) {
        self.ops.push_back(PendingOp {
            operation_id: mint_operation_id(),
            terminal_id,
            events,
            expected_server_id: None,
            created_at: Instant::now(),
            attempted: false,
        });
    }

    /// Whether `request_id` correlates to this journal's outstanding attempt.
    #[must_use]
    pub fn owns(&self, request_id: u32) -> bool {
        self.connection
            .as_ref()
            .is_some_and(|ctx| ctx.in_flight == Some(request_id))
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
        let now = Instant::now();
        let mut reports = Vec::new();
        let Some(ctx) = self.connection.as_ref() else {
            return (reports, None);
        };
        if ctx.in_flight.is_some() {
            return (reports, None);
        }
        let server_id = ctx.server_id.clone();
        let frame = loop {
            let Some(mut op) = self.ops.pop_front() else {
                break None;
            };
            if now.duration_since(op.created_at) >= INPUT_RETRY_HORIZON {
                reports.push(strand_report(
                    &op,
                    "the acknowledged-input retry horizon expired",
                ));
                continue;
            }
            let request_id = *next_request_id;
            *next_request_id = next_request_id.wrapping_add(1);
            op.attempted = true;
            op.expected_server_id
                .get_or_insert_with(|| server_id.clone());
            let frame = FrameKind::Command {
                request_id,
                command: Command::ApplyInput {
                    operation_id: op.operation_id,
                    terminal_id: op.terminal_id.clone(),
                    events: op.events.clone(),
                },
            };
            self.ops.push_front(op);
            if let Some(ctx) = self.connection.as_mut() {
                ctx.in_flight = Some(request_id);
            }
            break Some(frame);
        };
        (reports, frame)
    }

    /// Fold one `COMMAND_RESULT` for the outstanding attempt into a verdict.
    ///
    /// The classification mirrors the mobile reference exactly: `Ok` is the
    /// receipt; `INPUT_DELIVERY_UNKNOWN` — and any reply shape this build
    /// cannot read — is *unknown*, terminal, never retried; every other error
    /// wrote nothing and is *refused*. `RESOURCE_EXHAUSTED` lands in the
    /// refused arm deliberately: it can only mean another client holds the
    /// pane's acknowledged slot, the journal's own lane is serialized, and a
    /// TUI user retypes a refused paste far more naturally than they audit a
    /// background backoff loop.
    ///
    /// Returns `None` for a request id this journal does not own.
    pub fn resolve(&mut self, request_id: u32, result: &CommandResult) -> Option<ReplayReport> {
        if !self.owns(request_id) {
            return None;
        }
        if let Some(ctx) = self.connection.as_mut() {
            ctx.in_flight = None;
        }
        let op = self.ops.pop_front()?;
        let (disposition, message) = match result {
            CommandResult::Ok | CommandResult::OkWith(_) => {
                (ReplayDisposition::Delivered, String::new())
            }
            CommandResult::Error { code, message } => (
                if *code == ErrorCode::InputDeliveryUnknown {
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
        for op in self.ops.drain(..) {
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
            }
        }
        self.ops = kept;
        reports
    }

    fn strand_all(&mut self, why: &str) -> Vec<ReplayReport> {
        self.ops
            .drain(..)
            .map(|op| strand_report(&op, why))
            .collect()
    }
}

/// The stranding verdict: an attempted operation is *unknown* (its bytes may
/// be in the pane), a never-sent one is a deterministic *refusal*.
fn strand_report(op: &PendingOp, message: &str) -> ReplayReport {
    ReplayReport {
        disposition: if op.attempted {
            ReplayDisposition::Unknown
        } else {
            ReplayDisposition::Refused
        },
        operation_id: operation_id_hex(&op.operation_id),
        message: message.to_owned(),
    }
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

    fn tid(n: u32) -> TerminalId {
        TerminalId::local(n)
    }

    fn paste(text: &str) -> Vec<InputEvent> {
        use phux_protocol::input::paste::{PasteEvent, PasteTrust};
        vec![InputEvent::Paste(PasteEvent {
            trust: PasteTrust::Untrusted,
            data: text.as_bytes().to_vec(),
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
        journal.submit(tid(1), paste("ship it"));
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
        journal.submit(tid(1), paste("first"));
        journal.submit(tid(1), paste("second"));
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

    // ---- point 5: incarnation binding --------------------------------

    /// An attempted operation must not be replayed against a different
    /// incarnation: the dedupe cache died with the old process, so the only
    /// honest verdict is unknown.
    #[test]
    fn an_attempted_op_strands_unknown_when_the_incarnation_changes() {
        let mut journal = armed_journal();
        journal.submit(tid(1), paste("ship it"));
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
        journal.submit(tid(1), paste("queued while offline"));
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
        journal.submit(tid(1), paste("attempted"));
        let mut next = 1_u32;
        let _ = send_one(&mut journal, &mut next);
        journal.submit(tid(1), paste("never sent"));

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
        journal.submit(tid(1), paste("queued"));
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
        journal.submit(tid(1), paste("stale"));
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
        journal.submit(tid(1), paste("stale"));
        let mut next = 1_u32;
        let _ = send_one(&mut journal, &mut next);
        journal.submit(tid(1), paste("fresh"));
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
            journal.submit(tid(1), paste("x"));
            let mut next = 1_u32;
            let (request, _) = send_one(&mut journal, &mut next);
            let report = journal.resolve(request, &result).expect("owned");
            assert_eq!(report.disposition, expected, "{result:?}");
        }
    }

    /// A result for a request id the journal does not own is not consumed —
    /// it belongs to some other correlation and must fall through to
    /// whatever owns it.
    #[test]
    fn foreign_request_ids_are_not_consumed() {
        let mut journal = armed_journal();
        journal.submit(tid(1), paste("x"));
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
        journal.submit(tid(1), paste("attempted"));
        let mut next = 1_u32;
        let _ = send_one(&mut journal, &mut next);
        journal.submit(tid(2), paste("never sent"));
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
        journal.submit(tid(1), paste("mid-switch"));
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
