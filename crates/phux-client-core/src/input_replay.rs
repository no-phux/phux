//! Transport-free acknowledged-input replay policy (ADR-0053).
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
//! This module is the reusable client half of that contract. Native and web
//! hosts own clocks, operation-id generation, sockets, and UI delivery; this
//! journal owns only ordering, retention, replay eligibility, and result
//! classification:
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
//!   `phux-server/src/runtime/apps/terminal/input_lane/acknowledged.rs`). Past it the
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
//! builds frames. A host keeps one journal across connection attempts, sends
//! the returned frames in order, feeds command results back through
//! [`InputReplayJournal::resolve`], and presents [`ReplayReport`]s in its own
//! UI. Raw fire-and-forget input never enters this type.

use std::collections::{HashMap, HashSet, VecDeque};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::OnceLock;
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

use phux_protocol::ids::{InputOperationId, ResourceId};
use phux_protocol::input::InputEvent;

mod batch;
mod frames;
mod resolve;

pub use batch::{BatchTooLarge, validate_apply_input_batch};
#[cfg(test)]
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind};
pub use resolve::operation_id_hex;
use resolve::{disposition_index, retained_event_bytes};

/// How long an unresolved operation remains eligible for a same-id resend.
///
/// Equal to the server's dedupe retention (`DEDUPE_RETENTION`, 10 minutes) and
/// to phux-mobile's `INPUT_RETRY_HORIZON`, and it must never exceed the
/// former: a resend after the server may have evicted the id-to-outcome
/// record is indistinguishable from a first send, which is the double-write
/// this journal exists to prevent.
pub const INPUT_RETRY_HORIZON: Duration = Duration::from_mins(10);

const INPUT_RETRY_HORIZON_MS: u64 = 10 * 60 * 1_000;

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
    created_at_ms: u64,
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
    /// `Some` between [`Self::begin_connection_at`] and
    /// [`Self::connection_lost`] — i.e. while there is a live, negotiated
    /// socket whose incarnation is known and which advertised
    /// `ACKNOWLEDGED_INPUT`.
    connection: Option<ConnectionContext>,
    retained_events: usize,
    retained_bytes: usize,
    deferred_reports: VecDeque<ReplayReport>,
    suppressed_reports: [usize; 3],
    /// Terminals blocked after an operation's delivery became ambiguous.
    /// Only explicit host evidence may clear this fence.
    poisoned_terminals: HashSet<ResourceId>,
}

impl Default for InputReplayJournal {
    fn default() -> Self {
        Self::new()
    }
}

impl InputReplayJournal {
    /// An empty journal with no live connection.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ops: VecDeque::new(),
            connection: None,
            retained_events: 0,
            retained_bytes: 0,
            deferred_reports: VecDeque::new(),
            suppressed_reports: [0; 3],
            poisoned_terminals: HashSet::new(),
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
    /// - otherwise the operation stays queued for [`Self::next_frame_at`],
    ///   which will resend it under its original id. The server's dedupe
    ///   cache is what makes that resend idempotent — including the
    ///   session-switch case, where the first attempt's reply was already
    ///   emitted and dropped.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn begin_connection(
        &mut self,
        server_id: Option<&[u8]>,
        acknowledged_input: bool,
    ) -> Vec<ReplayReport> {
        self.begin_connection_at(server_id, acknowledged_input, monotonic_now_ms())
    }

    /// Caller-clock connection entry point, available on every target.
    ///
    /// `now_ms` is milliseconds from a host-chosen monotonic origin. A host
    /// that uses the explicit clock APIs must use the same origin for every
    /// operation during the journal's lifetime.
    pub fn begin_connection_at(
        &mut self,
        server_id: Option<&[u8]>,
        acknowledged_input: bool,
        now_ms: u64,
    ) -> Vec<ReplayReport> {
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
        self.sweep(now_ms)
    }

    /// Whether a paste should take the acknowledged path right now.
    #[must_use]
    pub const fn active(&self) -> bool {
        self.connection.is_some()
    }

    /// The live socket is gone. In-flight correlation dies; operations stay,
    /// to be re-decided by the next [`Self::begin_connection_at`] (or drained by
    /// [`Self::drain_unresolved`] if no reconnect succeeds).
    pub fn connection_lost(&mut self) {
        self.connection = None;
    }

    /// Journal one acknowledged batch under a caller-generated operation id.
    ///
    /// The caller must mint the id once per user action and must not reuse it
    /// for a different payload. The journal stores both id and payload and
    /// reuses them verbatim on every replay.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn submit(
        &mut self,
        operation_id: InputOperationId,
        terminal_id: ResourceId,
        events: Vec<InputEvent>,
    ) -> Result<(), ReplayReport> {
        self.submit_at(operation_id, terminal_id, events, monotonic_now_ms())
    }

    /// Caller-clock submission entry point, available on every target.
    pub fn submit_at(
        &mut self,
        operation_id: InputOperationId,
        terminal_id: ResourceId,
        events: Vec<InputEvent>,
        now_ms: u64,
    ) -> Result<(), ReplayReport> {
        if self.poisoned_terminals.contains(&terminal_id) {
            let report = ReplayReport {
                disposition: ReplayDisposition::Refused,
                operation_id: operation_id_hex(&operation_id),
                message: "an earlier acknowledged input has unknown delivery; read the pane before retrying"
                    .to_owned(),
            };
            self.defer_report(report.clone());
            return Err(report);
        }
        if let Err(refusal) = validate_apply_input_batch(&events) {
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
            created_at_ms: now_ms,
            attempts: 0,
            retained_bytes,
        });
        Ok(())
    }

    /// Whether later input for `terminal_id` must join the journal to remain
    /// behind an acknowledged operation already queued or in flight.
    #[must_use]
    pub fn must_order_after(&self, terminal_id: &ResourceId) -> bool {
        self.poisoned_terminals.contains(terminal_id)
            || self.ops.iter().any(|op| &op.terminal_id == terminal_id)
    }

    /// Whether ambiguous delivery fences new input for this Terminal.
    #[must_use]
    pub fn delivery_fenced(&self, terminal_id: &ResourceId) -> bool {
        self.poisoned_terminals.contains(terminal_id)
    }

    /// Clear a delivery fence after the host has presented fresh authoritative
    /// pane output to the user. Reconnect alone is not evidence and does not
    /// clear a fence.
    pub fn clear_delivery_fence(&mut self, terminal_id: &ResourceId) -> bool {
        self.poisoned_terminals.remove(terminal_id)
    }

    /// Retire every queued operation and delivery fence for a terminal that
    /// the host has authoritatively closed or permanently released.
    ///
    /// Attempted operations remain visible as unknown reports; never-sent
    /// operations are refused. The terminal fence is then removed because the
    /// host guarantees no further input can target this resource incarnation.
    pub fn retire_terminal(&mut self, terminal_id: &ResourceId, why: &str) -> Vec<ReplayReport> {
        let reports = self.strand_terminal(terminal_id, why);
        self.poisoned_terminals.remove(terminal_id);
        reports
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
}

#[cfg(not(target_arch = "wasm32"))]
fn monotonic_now_ms() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    u64::try_from(ORIGIN.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
