//! Acknowledged input delivery to an agent (ADR-0076 points 1-4, 6, 7;
//! ADR-0053).
//!
//! This is the client half of `phux agent prompt` and, since phux-w7z2.36, of
//! `phux agent send-keys` as well. Both verbs write to a pane that a human is
//! not watching, so both need the same thing fire-and-forget `ROUTE_INPUT`
//! cannot give: **a receipt with an operation id**, so that a caller which did
//! not get an answer can ask again without risking a duplicate.
//!
//! # What an `OK` actually attests, stated once
//!
//! `APPLY_INPUT`'s `OK` means `write_all` **and** `flush` completed on the PTY
//! **master** (`phux-server/src/terminal_actor/spawn.rs`): every byte of the
//! batch was accepted by the kernel into the tty's input queue for the slave.
//! That is strictly more than `ROUTE_INPUT` states (which acks `Ok` even when
//! a full pane mailbox drops the event) and strictly **less** than
//! consumption: a slave that calls `tcsetattr(…, TCSAFLUSH, …)` — which every
//! TUI does when it shells out and returns — discards an acknowledged batch
//! with no error and no trace. See `docs/spec/L1.md` §6.2.1. Nothing in this
//! module infers consumption from repaint activity; that is the output-inference
//! oracle ADR-0053 declined, and it is unsound in both directions.
//!
//! # The lane is server-wide, and that shapes the retry policy
//!
//! Verified against `phux-server/src/runtime/input_lane.rs`: the admission
//! gate is **one `AtomicBool` for the whole server**, not per Terminal, and it
//! is held across a bounded completion wait of **5 s** on the single lane
//! thread. Two consequences this module is built around:
//!
//! 1. The submit deadline must exceed that 5 s wait ([`SUBMIT_DEADLINE`]), or
//!    the CLI abandons an operation whose id is about to be bound to a result
//!    it will never see.
//! 2. `RESOURCE_EXHAUSTED` is the *common* error in a fleet, not the rare one
//!    — two orchestrators prompting two different panes collide — and the
//!    worst-case interval before the lane frees is that same ~5 s, so the
//!    backoff budget ([`backoff_schedule`]) runs past 6 s before giving up.
//!    Prompting a fleet in parallel is therefore not supported; serialize it.
//!
//! # The two rules that must never be softened
//!
//! - **A new operation id is never minted for a retry.** The id is generated
//!   once per process invocation by the caller and passed in here; every
//!   retry this module performs reuses it verbatim. A fresh id is precisely
//!   the duplicate the design exists to prevent.
//! - **`INPUT_DELIVERY_UNKNOWN` is terminal.** A same-id retry replays the
//!   cached unknown (the server caches every post-handoff outcome, unknown
//!   included) and a new-id retry duplicates. So the verb reports it and
//!   stops. The honest recovery is a *read* of the pane, by a human or by an
//!   orchestrator.
//!
//! # Reuse, not a second wait
//!
//! [`prompt_agent`]'s `--wait` half is driven by [`EdgeTracker`] — the same
//! predicate `phux agent wait` runs, imported from
//! [`crate::agent_wait`] rather than re-derived. The corpse rule (a level read
//! of `idle` never satisfies a completion gate) is therefore enforced by one
//! implementation for both verbs. What is local here is only the *ordering*:
//! ADR-0076 point 6 requires the subscribe, the baseline read, the submit and
//! the wait to share **one connection**, and the tracker is constructed only
//! **after** the `APPLY_INPUT` result, so a transition published before the
//! write is structurally incapable of satisfying the gate.

use std::path::Path;
use std::time::{Duration, Instant};

use phux_protocol::ids::{InputOperationId, TerminalId};
use phux_protocol::input::InputEvent;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, MAX_APPLY_INPUT_COMMAND_BODY,
    MAX_APPLY_INPUT_EVENTS, Scope,
};

use crate::agent_meta::{AgentMetaState, AgentRecord, TERMINAL_AGENT_KEY, parse_agent_record};
use crate::agent_wait::{
    AgentWaitResult, DepartureReason, EdgeSource, EdgeTracker, ObservedEdge, Verdict,
    fetch_agent_record,
};
use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::attach::input::StdinParser;
use crate::watch::{WatchItem, stream_items, subscribe};

/// Inline prompt-text ceiling, in bytes of UTF-8.
///
/// Far below the 64 KiB wire cap ([`MAX_APPLY_INPUT_COMMAND_BODY`]) on
/// purpose: the binding constraint is the **tty input queue**, not the wire.
/// The server's writer thread makes a blocking `write(2)` on the PTY master,
/// and the queue is 1024 bytes on darwin / 4096 on Linux, so a payload larger
/// than the free space blocks until the slave drains it — and an agent busy
/// in a tool call is not draining. The 5 s completion wait then expires and a
/// prompt that lands whole a moment later is reported as
/// `INPUT_DELIVERY_UNKNOWN`. 4096 keeps a compliant prompt to one
/// non-blocking write on every platform this codebase models.
pub const MAX_PROMPT_BYTES: usize = 4096;

/// Client-side deadline for one `APPLY_INPUT` round trip.
///
/// **Must exceed the server's 5 s completion wait**
/// (`ACKNOWLEDGED_COMPLETION_TIMEOUT`, `input_lane.rs`). Below it, the CLI
/// would abandon an operation whose id is about to be bound to a result, and
/// the caller would be told "unknown" for a batch the server is about to
/// resolve either way. That constant appears in no spec document, which is
/// why it is restated here.
pub const SUBMIT_DEADLINE: Duration = Duration::from_secs(8);

/// Unjittered `RESOURCE_EXHAUSTED` backoff steps.
///
/// Sums to 7.2 s, and the number that matters is the *jittered floor*: at
/// [`JITTER_PERMILLE`] the worst case is 6.12 s. The server-wide admission
/// flag is held across a 5 s completion wait, so a schedule whose floor is
/// under ~6 s fails spuriously against a single wedged pane. Every step is a
/// resubmission of the **same** operation id.
const BACKOFF_STEPS: &[Duration] = &[
    Duration::from_millis(200),
    Duration::from_millis(600),
    Duration::from_millis(1_600),
    Duration::from_millis(2_400),
    Duration::from_millis(2_400),
];

/// Jitter half-width, in permille of each backoff step: +/-15 %.
///
/// Wide enough to decorrelate two orchestrators colliding on the server-wide
/// lane, narrow enough that the schedule's floor still clears the 5 s
/// admission hold without inflating the ceiling past ~8.3 s.
const JITTER_PERMILLE: u64 = 150;

/// Correlation id for the pre-submit ownership read.
const OWNERSHIP_REQUEST_ID: u32 = 1;

/// Correlation id for the `APPLY_INPUT` submit. One greater than the
/// ownership read, on the same connection, so the two are consecutive frames
/// and the server answers them in that order.
///
/// Reused verbatim by a `RESOURCE_EXHAUSTED` retry, and safely so: a retry
/// only follows a *completed* round trip, because the one outcome that leaves
/// a reply outstanding — the [`SUBMIT_DEADLINE`] elapsing — is
/// [`ApplyVerdict::Unknown`], which is terminal and never retried. So no stale
/// frame carrying this id can ever be in flight when the next one is sent.
const SUBMIT_REQUEST_ID: u32 = 2;

/// How the batch ended up, as reported in `--json`'s `delivery` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Every byte was accepted into the pane's tty input queue.
    Acked,
    /// Delivery is indeterminate in both directions: some, all, or none of
    /// the bytes may have reached the tty, and a batch reported unknown can
    /// still complete afterwards. Terminal — never retried.
    Unknown,
    /// Nothing was written.
    Refused,
}

impl Delivery {
    /// The wire-facing word for this outcome.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Acked => "acked",
            Self::Unknown => "unknown",
            Self::Refused => "refused",
        }
    }
}

/// A refusal in which **nothing was written**, and which resubmitting the
/// identical batch cannot fix.
///
/// Every variant is exit 2 at the CLI. `RESOURCE_EXHAUSTED` is deliberately
/// absent: it also wrote nothing, but it *can* succeed unchanged, so it is
/// [`ApplyVerdict::Busy`] and is retried under the same id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The prompt text is empty; there is nothing to submit.
    EmptyText,
    /// The prompt text carries a raw newline (ADR-0076 point 3).
    ///
    /// `paste::encode` turns newlines into carriage returns when the pane has
    /// **not** set DEC 2004, a mode no client can observe, so a multi-line
    /// prompt can become N submissions. That is a silent N-plication —
    /// strictly worse than the silent double this verb exists to prevent —
    /// so it is refused rather than guessed at.
    MultilineText {
        /// How many raw newlines the text carries.
        newlines: usize,
    },
    /// The payload is over a size ceiling. `wire` distinguishes the wire caps
    /// (64 KiB body / 256 events) from the tighter inline prompt ceiling.
    TooLarge {
        /// What was measured (`"bytes"` or `"events"`).
        unit: &'static str,
        /// The measured size.
        measured: usize,
        /// The ceiling it exceeded.
        limit: usize,
        /// Whether `limit` is a protocol cap rather than a client policy.
        wire: bool,
    },
    /// The target is a federation satellite. `APPLY_INPUT` is local-only, so
    /// the acknowledged path does not exist there — and downgrading to
    /// `ROUTE_INPUT` would make the verb's success code mean different things
    /// on different hosts, the worst property a fleet primitive can have.
    SatelliteTarget {
        /// The satellite host token the target named.
        host: String,
    },
    /// The server does not advertise `ACKNOWLEDGED_INPUT`. Refused, never
    /// downgraded.
    NoAcknowledgedInput,
    /// The pane declares no `phux.agent/v1` record, so there is no agent
    /// identity to verify against.
    NoAgentRecord,
    /// The pane hosts a different agent than the caller named; the string
    /// describes the occupant.
    AgentMismatch(String),
    /// Another client holds the pane's input lease (ADR-0033). Pre-handoff,
    /// nothing written. This verb never acquires or seizes a lease: a prompt
    /// is the least urgent input in the system, and seizing would override
    /// exactly the signal the lease exists to carry.
    InputLeaseHeld(String),
    /// The pane's line discipline is canonical and the encoded batch has no
    /// terminator to flush it, so writing it would have truncated silently.
    /// Refused before any byte reached the pane — the only definitely-nothing
    /// -happened post-handoff answer — and it cannot succeed unchanged.
    CanonicalLimitExceeded(String),
    /// An untrusted paste failed the pane's safety policy. Structurally
    /// unreachable for these verbs, which always send `Trusted`; mapped so
    /// the vocabulary is total.
    UnsafePaste(String),
    /// The server rejected the batch structurally, or the operation id names
    /// input that differs from this batch.
    InvalidBatch(String),
    /// The server forbade the operation for this peer.
    PermissionDenied(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyText => f.write_str("the prompt text is empty"),
            Self::MultilineText { newlines } => write!(
                f,
                "the prompt text carries {newlines} raw newline(s); a pane that has not set \
                 bracketed-paste mode turns each one into a submission, and no client can \
                 observe that mode"
            ),
            Self::TooLarge {
                unit,
                measured,
                limit,
                wire,
            } => {
                let which = if *wire { "protocol cap" } else { "ceiling" };
                write!(
                    f,
                    "batch is {measured} {unit}, over the {limit}-{unit} {which}"
                )
            }
            Self::SatelliteTarget { host } => write!(
                f,
                "'{host}' is a federation satellite, and acknowledged input is local-only: \
                 the delivery receipt is owned by the machine that owns the PTY, and a hub \
                 cannot vouch for it"
            ),
            Self::NoAcknowledgedInput => f.write_str(
                "this server does not advertise ACKNOWLEDGED_INPUT, so there is no \
                 acknowledged input path to use",
            ),
            Self::NoAgentRecord => f.write_str(
                "the pane declares no phux.agent/v1 record, so there is no agent identity \
                 to verify",
            ),
            Self::AgentMismatch(who) => write!(f, "the pane is hosting {who}"),
            Self::InputLeaseHeld(message) => write!(f, "another client holds input: {message}"),
            Self::CanonicalLimitExceeded(message) => write!(
                f,
                "the pane refused the batch before writing any byte: {message}"
            ),
            Self::UnsafePaste(message) => {
                write!(f, "the paste failed the pane's policy: {message}")
            }
            Self::InvalidBatch(message) => write!(f, "the server rejected the batch: {message}"),
            Self::PermissionDenied(message) => write!(f, "permission denied: {message}"),
        }
    }
}

/// The one reading an `APPLY_INPUT` reply has.
///
/// Deliberately four cases and not more: every server error code collapses
/// into "written", "nothing written and retryable", "nothing written and
/// not retryable", or "indeterminate". A caller that has to guess which of
/// those a code means is a caller that will guess wrong under load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyVerdict {
    /// Every byte was accepted into the tty input queue and flushed.
    Acked,
    /// Pre-handoff refusal for want of the server-wide acknowledged lane.
    /// Nothing was written; resubmitting the identical batch under the same
    /// operation id is honest and is what this module does.
    Busy(String),
    /// Nothing was written and the identical batch cannot succeed.
    Refused(Refusal),
    /// The Terminal is gone or was never here.
    NotFound(String),
    /// Delivery is indeterminate. **Terminal**: a same-id retry replays this
    /// same cached answer, and a new-id retry is a duplicate.
    Unknown(String),
}

/// Map one `APPLY_INPUT` reply onto its single reading.
///
/// `INTERNAL_ERROR` is pessimized to [`ApplyVerdict::Unknown`] on purpose:
/// the server raises it both pre-handoff ("pane actor unavailable") and
/// post-handoff ("input lane stopped before `APPLY_INPUT` completed"), and the
/// code alone does not separate them, so the caller must assume the worse of
/// the two. Any code this build does not know is pessimized the same way —
/// `ErrorCode` is `#[non_exhaustive]`, and inventing an optimistic default
/// for a future code is how a duplicate gets written.
#[must_use]
pub fn classify(result: &CommandResult) -> ApplyVerdict {
    let (code, message) = match result {
        CommandResult::Ok | CommandResult::OkWith(_) => return ApplyVerdict::Acked,
        CommandResult::Error { code, message } => (*code, message.clone()),
        // `CommandResult` is `#[non_exhaustive]`. A reply shape this build
        // cannot name is not evidence that nothing was written, so it
        // pessimizes with every other unknown.
        _ => {
            return ApplyVerdict::Unknown(
                "the server answered APPLY_INPUT with a result this build cannot read".to_owned(),
            );
        }
    };
    match code {
        ErrorCode::ResourceExhausted => ApplyVerdict::Busy(message),
        ErrorCode::InputLeaseHeld => ApplyVerdict::Refused(Refusal::InputLeaseHeld(message)),
        ErrorCode::CanonicalLimitExceeded => {
            ApplyVerdict::Refused(Refusal::CanonicalLimitExceeded(message))
        }
        ErrorCode::UnsafePaste => ApplyVerdict::Refused(Refusal::UnsafePaste(message)),
        ErrorCode::InvalidCommand | ErrorCode::MalformedMessage | ErrorCode::UnknownMessageType => {
            ApplyVerdict::Refused(Refusal::InvalidBatch(message))
        }
        ErrorCode::PermissionDenied => ApplyVerdict::Refused(Refusal::PermissionDenied(message)),
        ErrorCode::UnsupportedSatelliteRoute | ErrorCode::SatelliteUnreachable => {
            ApplyVerdict::Refused(Refusal::SatelliteTarget { host: message })
        }
        ErrorCode::TerminalNotFound => ApplyVerdict::NotFound(message),
        // `InputDeliveryUnknown` shares this arm with `InternalError` and with
        // every code a newer server may add. That is the pessimization, not an
        // oversight: the named code and the unnameable ones lead to the same
        // published reading, and a caller must not be able to tell "the server
        // said it could not confirm" from "this build could not read what the
        // server said" — both mean *go read the pane*.
        _ => ApplyVerdict::Unknown(message),
    }
}

/// The backoff schedule for `operation_id`, jittered +/-25 %.
///
/// The jitter is derived from the operation id rather than from a second RNG.
/// The id is already CSPRNG material generated once per invocation, so two
/// processes colliding on the server-wide lane get independent schedules and
/// decorrelate — which is the only thing jitter is for — with no clock, no
/// global state, and a schedule that is reproducible from the id in a bug
/// report.
#[must_use]
pub fn backoff_schedule(operation_id: &InputOperationId) -> Vec<Duration> {
    let bytes = operation_id.as_bytes();
    BACKOFF_STEPS
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let seed = u64::from(bytes[index % bytes.len()]);
            // (1 - j) .. (1 + j) in integer permille arithmetic, no floats.
            let scale = (1_000 - JITTER_PERMILLE)
                + seed.saturating_mul(JITTER_PERMILLE.saturating_mul(2)) / 255;
            let millis = u64::try_from(step.as_millis()).unwrap_or(u64::MAX);
            Duration::from_millis(millis.saturating_mul(scale) / 1000)
        })
        .collect()
}

/// The events one prompt submits: `[Paste(trusted, text), Key(Enter)]`.
///
/// One batch, `Enter` **last**, and that ordering carries the whole
/// truncation argument. The server encodes every event of a batch against one
/// mode snapshot into one byte vector and hands it to the pane as a single
/// write, so a partial write can only truncate the *tail* — and the tail is
/// the submission. Split into two operations, the outcome space gains
/// `{text unknown, Enter acked}`: an unknown-length prefix of the prompt
/// submitted with confidence, which is exactly the class `docs/spec/input.md`
/// exists to forbid.
///
/// The `Enter` atom is built by feeding `\r` through the same [`StdinParser`]
/// the interactive client uses, so the key event is byte-identical to the one
/// `phux send-keys … Enter` produces rather than a hand-rolled table entry.
#[must_use]
pub fn prompt_events(text: &str) -> Vec<InputEvent> {
    let mut events = vec![InputEvent::Paste(PasteEvent {
        trust: PasteTrust::Trusted,
        data: text.as_bytes().to_vec(),
    })];
    events.extend(StdinParser::default().feed(b"\r"));
    events
}

/// Refuse prompt text that cannot be delivered honestly.
///
/// Runs before a connection is opened: an empty prompt, a multi-line one
/// (see [`Refusal::MultilineText`]), or one over [`MAX_PROMPT_BYTES`].
///
/// # Errors
///
/// Returns the [`Refusal`] the text earned; nothing is written in any case.
pub fn validate_prompt_text(text: &str) -> Result<(), Refusal> {
    if text.is_empty() {
        return Err(Refusal::EmptyText);
    }
    let newlines = text.bytes().filter(|byte| *byte == b'\n').count();
    if newlines > 0 {
        return Err(Refusal::MultilineText { newlines });
    }
    if text.len() > MAX_PROMPT_BYTES {
        return Err(Refusal::TooLarge {
            unit: "bytes",
            measured: text.len(),
            limit: MAX_PROMPT_BYTES,
            wire: false,
        });
    }
    Ok(())
}

/// Conservative upper bound on the encoded `APPLY_INPUT` command body for
/// `events`: every event's payload plus a generous per-event TLV allowance
/// plus the fixed operation-id / terminal-id / count header.
fn encoded_body_bound(events: &[InputEvent]) -> usize {
    /// Tag byte, 16-byte operation id, a worst-case satellite terminal id,
    /// and the `u16` event count.
    const HEADER: usize = 1 + 16 + 264 + 2;
    /// Per-event tag plus length prefix plus the widest fixed key/mouse body.
    const PER_EVENT: usize = 32;
    events.iter().fold(HEADER, |total, event| {
        let payload = match event {
            InputEvent::Paste(paste) => paste.data.len(),
            _ => 0,
        };
        total.saturating_add(PER_EVENT).saturating_add(payload)
    })
}

/// Refuse a batch the wire cannot carry, before it is on the wire.
///
/// The two protocol caps are [`MAX_APPLY_INPUT_EVENTS`] (256) and
/// [`MAX_APPLY_INPUT_COMMAND_BODY`] (64 KiB). Both are checked client-side so
/// an oversized batch is a local usage error naming the actual size, not a
/// `DecodeError` the server answers with a bare `INVALID_COMMAND` — and,
/// critically, so a caller never learns about the cap by having the batch
/// silently split. Splitting one logical payload across operations is
/// forbidden by `docs/spec/input.md`, and neither verb does it.
///
/// # Errors
///
/// Returns [`Refusal::TooLarge`] with `wire: true` for either cap.
pub fn validate_batch(events: &[InputEvent]) -> Result<(), Refusal> {
    if events.len() > MAX_APPLY_INPUT_EVENTS {
        return Err(Refusal::TooLarge {
            unit: "events",
            measured: events.len(),
            limit: MAX_APPLY_INPUT_EVENTS,
            wire: true,
        });
    }
    let bound = encoded_body_bound(events);
    if bound > MAX_APPLY_INPUT_COMMAND_BODY {
        return Err(Refusal::TooLarge {
            unit: "bytes",
            measured: bound,
            limit: MAX_APPLY_INPUT_COMMAND_BODY,
            wire: true,
        });
    }
    Ok(())
}

/// Whether `conn`'s server advertised `ACKNOWLEDGED_INPUT`.
///
/// A connection with no negotiated bootstrap is the crate's raw in-process
/// test seam; it reports `false` so a caller can only ever *refuse* on the
/// unnegotiated path, never silently proceed.
#[must_use]
pub fn supports_acknowledged_input(conn: &Connection) -> bool {
    conn.negotiated_bootstrap().is_some_and(|bootstrap| {
        bootstrap
            .server_features
            .contains(phux_protocol::caps::ServerFeature::AcknowledgedInput)
    })
}

/// Submit `events` to `terminal` once, under `operation_id`, and classify the
/// reply.
///
/// Returns the verdict plus every frame the server interleaved ahead of the
/// result. The interleave is **returned rather than dropped**: this
/// connection holds a metadata subscription, so a `METADATA_CHANGED` arriving
/// between the submit and the result is already off the socket and nothing
/// re-sends it. That is the one trap in this file, and it is why no call site
/// here uses `into_result_ignoring_interleaved`.
///
/// The round trip is bounded by [`SUBMIT_DEADLINE`], which exceeds the
/// server's own 5 s completion wait. Elapsing it is [`ApplyVerdict::Unknown`]
/// — never a refusal, and never a reason to mint a second id.
///
/// # Errors
///
/// Propagates [`AttachError`] from the transport.
pub async fn apply_input_once(
    conn: &mut Connection,
    terminal: &TerminalId,
    operation_id: InputOperationId,
    events: Vec<InputEvent>,
    request_id: u32,
) -> Result<(ApplyVerdict, Vec<FrameKind>), AttachError> {
    let command = Command::ApplyInput {
        operation_id,
        terminal_id: terminal.clone(),
        events,
    };
    let sent = tokio::time::timeout(SUBMIT_DEADLINE, conn.request(request_id, command)).await;
    match sent {
        Ok(reply) => {
            let (result, interleaved) = reply?.into_parts();
            Ok((classify(&result), interleaved))
        }
        Err(_elapsed) => Ok((
            ApplyVerdict::Unknown(format!(
                "no answer within {}s; the server's own completion wait is 5s, so the batch \
                 may still be resolving",
                SUBMIT_DEADLINE.as_secs()
            )),
            Vec::new(),
        )),
    }
}

/// Run `attempt` under `operation_id`, retrying only [`ApplyVerdict::Busy`],
/// on `schedule`, **always with the same id**.
///
/// `attempt` receives the id explicitly so the invariant is visible at the
/// type level: there is no path through this function that constructs an
/// `InputOperationId`. Returns the final verdict and how many attempts it
/// took.
///
/// # Errors
///
/// Propagates whatever `attempt` fails with.
pub async fn submit_with_backoff<F, E>(
    operation_id: InputOperationId,
    schedule: &[Duration],
    mut attempt: F,
) -> Result<(ApplyVerdict, u32), E>
where
    F: AsyncFnMut(InputOperationId) -> Result<ApplyVerdict, E>,
{
    let mut attempts: u32 = 0;
    let mut verdict = ApplyVerdict::Busy(String::new());
    for delay in std::iter::once(None).chain(schedule.iter().map(|step| Some(*step))) {
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        attempts = attempts.saturating_add(1);
        verdict = attempt(operation_id).await?;
        if !matches!(verdict, ApplyVerdict::Busy(_)) {
            return Ok((verdict, attempts));
        }
    }
    Ok((verdict, attempts))
}

/// What `--wait` was asked for.
#[derive(Debug, Clone)]
pub struct PromptWait {
    /// The states a post-submit transition must land in.
    pub targets: Vec<AgentMetaState>,
    /// Give up after this long. `None` waits forever.
    pub timeout: Option<Duration>,
    /// `GET_METADATA` poll-floor cadence, recovering an edge a dropped
    /// notification never delivered.
    pub poll_interval: Duration,
}

/// Everything one acknowledged submit produced.
#[derive(Debug, Clone)]
pub struct PromptOutcome {
    /// What the receipt attests.
    pub delivery: Delivery,
    /// Lowercase hex of the operation id, so a caller can correlate a retry,
    /// a server log line, and this run. Redacted in `Debug` on the wire type
    /// by design; disclosed here to the caller who generated it, because
    /// ADR-0076 point 2 requires the id in the failure report.
    pub operation_id: String,
    /// The record the ownership check passed on, immediately before the
    /// submit.
    pub agent: AgentRecord,
    /// That record's state.
    pub pre_submit_state: AgentMetaState,
    /// How many submits it took (`>1` only after `RESOURCE_EXHAUSTED`).
    pub attempts: u32,
    /// Wall time spent on the submit leg.
    pub submit_ms: u64,
    /// The `--wait` result, when one was asked for.
    pub wait: Option<AgentWaitResult>,
    /// Whether the push half of the wait ended and the poll floor carried it.
    pub degraded_to_polling: bool,
}

impl PromptOutcome {
    /// Whether a post-submit transition into a target state was observed.
    /// Always `false` without `--wait`.
    #[must_use]
    pub fn transition_observed(&self) -> bool {
        self.wait.as_ref().is_some_and(AgentWaitResult::satisfied)
    }
}

/// Why an acknowledged submit did not produce a [`PromptOutcome`].
#[derive(Debug, thiserror::Error)]
pub enum PromptError {
    /// Nothing was written and the identical batch cannot succeed. Exit 2.
    #[error("{0}")]
    Refused(Refusal),
    /// The lane never freed. Nothing was written across every attempt, so
    /// re-running the command is safe — but it is a failure, not a refusal.
    /// Exit 1.
    #[error(
        "the server-wide acknowledged input lane stayed busy across {attempts} attempts \
         ({budget_ms}ms): {message}"
    )]
    LaneBusy {
        /// How many submits were made, all under the same operation id.
        attempts: u32,
        /// The total backoff budget spent.
        budget_ms: u64,
        /// The server's last diagnostic.
        message: String,
        /// The operation id, in hex.
        operation_id: String,
    },
    /// The Terminal is gone. Exit 1.
    #[error("terminal not found: {0}")]
    NotFound(String),
    /// **Terminal, and the one rule an agent can violate catastrophically.**
    /// Some, all, or none of the bytes reached the tty; a same-id retry
    /// replays this cached answer and a new-id retry duplicates the prompt.
    /// The recovery is to *read* the pane. Exit 1.
    #[error("delivery unknown (operation {operation_id}): {message}")]
    DeliveryUnknown {
        /// The operation id, in hex — the only handle on what happened.
        operation_id: String,
        /// The server's diagnostic.
        message: String,
    },
    /// The pane's occupant changed while the batch was in flight, so the
    /// bytes went to an unknown occupant. Exit 1.
    #[error("the pane's occupant changed while the batch was in flight: {detail}")]
    OccupantChanged {
        /// What changed.
        detail: String,
        /// The operation id, in hex.
        operation_id: String,
        /// Whether the submit was acknowledged before the change was seen.
        delivery: Delivery,
    },
    /// The agent went away during `--wait`. Not success, not a timeout.
    #[error("the agent departed from '{}' ({})", .from.as_str(), .reason.as_str())]
    Departed {
        /// The state held before the departure.
        from: AgentMetaState,
        /// How it departed.
        reason: DepartureReason,
        /// The submit's own outcome, which succeeded.
        delivery: Delivery,
        /// The operation id, in hex.
        operation_id: String,
    },
    /// Transport or protocol failure.
    #[error(transparent)]
    Transport(#[from] AttachError),
}

/// Hex of an operation id, for correlation in diagnostics and `--json`.
#[must_use]
pub fn operation_id_hex(operation_id: &InputOperationId) -> String {
    use std::fmt::Write as _;
    operation_id
        .as_bytes()
        .iter()
        .fold(String::with_capacity(32), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// The record an interleaved `METADATA_CHANGED` carries for `terminal`.
///
/// The inner `Option` is the record: `Some(None)` is a tombstone. Frame-shape
/// matching only — the wait *predicate* is [`EdgeTracker`]'s and is not
/// duplicated here.
#[allow(
    clippy::option_option,
    reason = "the outer Option answers 'is this frame ours', the inner one \
              'record or tombstone'; collapsing them erases the tombstone"
)]
fn record_from_frame(frame: &FrameKind, terminal: &TerminalId) -> Option<Option<AgentRecord>> {
    let FrameKind::MetadataChanged { scope, key, value } = frame else {
        return None;
    };
    if key != TERMINAL_AGENT_KEY {
        return None;
    }
    let Scope::Terminal(id) = scope else {
        return None;
    };
    if id != terminal {
        return None;
    }
    Some(value.as_deref().and_then(parse_agent_record))
}

/// Read the pane's `phux.agent/v1` record over `conn`, folding in anything
/// the server pushed ahead of the answer.
///
/// Returns the **last** record observed at or before the answer, which is the
/// value the ownership check runs against: an interleaved `METADATA_CHANGED`
/// that arrived after the subscribe registered is newer than the `GET_METADATA`
/// answer's own snapshot only in the sense that it is later in the connection's
/// arrival order, and later is what "immediately before the write" means here.
async fn read_pre_submit_record(
    conn: &mut Connection,
    terminal: &TerminalId,
) -> Result<Option<AgentRecord>, AttachError> {
    let (answer, interleaved) = conn
        .request_metadata(
            OWNERSHIP_REQUEST_ID,
            Scope::Terminal(terminal.clone()),
            TERMINAL_AGENT_KEY.to_owned(),
        )
        .await?
        .into_parts();
    let value = answer.map_err(|refusal| AttachError::Refused(refusal.to_string()))?;
    let mut latest = value.as_deref().and_then(parse_agent_record);
    for frame in &interleaved {
        if let Some(observed) = record_from_frame(frame, terminal) {
            latest = observed;
        }
    }
    Ok(latest)
}

/// Deliver an already-built, already-validated batch to `terminal` with a
/// receipt, re-verifying the pane's occupant on the same connection first.
///
/// The full ordering, which is the contract:
///
/// 1. Refuse a satellite target before a socket is opened.
/// 2. `subscribe` — `SUBSCRIBE_EVENTS` + `SUBSCRIBE_METADATA`, one connection.
/// 3. Gate on `ACKNOWLEDGED_INPUT`; refuse rather than downgrade.
/// 4. `GET_METADATA` (id 1) and run `verify` against the record.
/// 5. `APPLY_INPUT` (id 2) under `operation_id`, retried only on
///    `RESOURCE_EXHAUSTED` and only under that same id.
/// 6. Report an occupant change seen before the result as delivery to an
///    unknown occupant.
/// 7. When `wait` is set, seed an [`EdgeTracker`] **after** the result and
///    wait for a transition on the same connection.
///
/// Steps 4 and 5 are consecutive frames on one connection and the server
/// handles a connection's frames in arrival order, so no frame from this
/// client interleaves between them. Since the whole batch is now a single
/// frame rather than one `ROUTE_INPUT` per event, that bound is *tighter*
/// than the one `send-keys` shipped with: there is no longer a window
/// between the first and last byte of the batch at all.
///
/// # Errors
///
/// See [`PromptError`]. `verify` returning `Some(description)` is
/// [`Refusal::AgentMismatch`].
#[allow(
    clippy::too_many_arguments,
    reason = "one call site per verb, and every argument is a distinct \
              contract knob the caller must choose deliberately"
)]
#[allow(
    clippy::too_many_lines,
    reason = "the subscribe / read / submit / wait ordering is the contract \
              and is only auditable as one sequence"
)]
#[allow(
    clippy::future_not_send,
    reason = "ADR-0003 binds the CLI to a current-thread runtime; the wait \
              half shares one EdgeTracker across two futures on that thread"
)]
pub async fn deliver_acknowledged(
    socket: &Path,
    terminal: &TerminalId,
    operation_id: InputOperationId,
    events: Vec<InputEvent>,
    verify: &dyn Fn(&AgentRecord) -> Option<String>,
    wait: Option<&PromptWait>,
) -> Result<PromptOutcome, PromptError> {
    let hex = operation_id_hex(&operation_id);
    // 1. Satellites, before a socket is opened. `route_to_satellite` returns
    //    `None` for `ApplyInput` and the server refuses a non-local id, so the
    //    safety is structural either way; refusing here buys a better message
    //    and no wasted round trip.
    if let Some(host) = terminal.host() {
        return Err(PromptError::Refused(Refusal::SatelliteTarget {
            host: host.as_str().to_owned(),
        }));
    }
    validate_batch(&events).map_err(PromptError::Refused)?;

    // 2. One connection, subscribed before anything is read. `Connection::
    //    connect` declares `Layer::L3`, without which the subscribe would be
    //    dropped silently (ADR-0076 point 7).
    let mut conn = subscribe(socket, Some(terminal.clone())).await?;

    // 3. The capability gate. Never a downgrade to ROUTE_INPUT: that command
    //    acks `Ok` for an event a full mailbox drops, so the same verb would
    //    return the same success code for two different guarantees.
    if !supports_acknowledged_input(&conn) {
        return Err(PromptError::Refused(Refusal::NoAcknowledgedInput));
    }

    // 4. Ownership re-verification from the published record. Not a fresh
    //    syscall (no wire verb exposes one) and not a server-side gate:
    //    detection fails safe toward `idle`, but input must fail safe toward
    //    delivery, so a text-matching manifest must never be able to make a
    //    pane start refusing keystrokes.
    let Some(record) = read_pre_submit_record(&mut conn, terminal).await? else {
        return Err(PromptError::Refused(Refusal::NoAgentRecord));
    };
    if let Some(mismatch) = verify(&record) {
        return Err(PromptError::Refused(Refusal::AgentMismatch(mismatch)));
    }
    let pre_submit_state = record.state;

    // 5. The submit. One id, every attempt.
    let schedule = backoff_schedule(&operation_id);
    let budget_ms = schedule
        .iter()
        .map(|step| u64::try_from(step.as_millis()).unwrap_or(u64::MAX))
        .sum();
    let started = Instant::now();
    let mut interleaved: Vec<FrameKind> = Vec::new();
    let (verdict, attempts) = submit_with_backoff(operation_id, &schedule, async |id| {
        let (verdict, frames) =
            apply_input_once(&mut conn, terminal, id, events.clone(), SUBMIT_REQUEST_ID).await?;
        interleaved.extend(frames);
        Ok::<_, AttachError>(verdict)
    })
    .await?;
    let submit_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let delivery = match verdict {
        ApplyVerdict::Acked => Delivery::Acked,
        ApplyVerdict::Busy(message) => {
            return Err(PromptError::LaneBusy {
                attempts,
                budget_ms,
                message,
                operation_id: hex,
            });
        }
        ApplyVerdict::Refused(refusal) => return Err(PromptError::Refused(refusal)),
        ApplyVerdict::NotFound(message) => return Err(PromptError::NotFound(message)),
        ApplyVerdict::Unknown(message) => {
            return Err(PromptError::DeliveryUnknown {
                operation_id: hex,
                message,
            });
        }
    };

    // 6. Anything the server pushed ahead of the result is, by ADR-0076 point
    //    6, incapable of satisfying a completion gate — it may be pre-write.
    //    It is still evidence about *who* the bytes went to, which is the one
    //    thing point 4 asks it for.
    let mut level = record.clone();
    for frame in &interleaved {
        let Some(observed) = record_from_frame(frame, terminal) else {
            continue;
        };
        let Some(observed) = observed else {
            return Err(PromptError::OccupantChanged {
                detail: "the phux.agent/v1 record was deleted".to_owned(),
                operation_id: hex,
                delivery,
            });
        };
        if !observed.name.eq_ignore_ascii_case(&record.name) {
            return Err(PromptError::OccupantChanged {
                detail: format!("'{}' replaced '{}'", observed.name, record.name),
                operation_id: hex,
                delivery,
            });
        }
        if observed.state == AgentMetaState::Unknown && record.state != AgentMetaState::Unknown {
            return Err(PromptError::OccupantChanged {
                detail: format!("'{}' withdrew its state to unknown", observed.name),
                operation_id: hex,
                delivery,
            });
        }
        level = observed;
    }

    let Some(wait) = wait else {
        return Ok(PromptOutcome {
            delivery,
            operation_id: hex,
            agent: record,
            pre_submit_state,
            attempts,
            submit_ms,
            wait: None,
            degraded_to_polling: false,
        });
    };

    // 7. The gate. Built here, after the result, so the pre-submit level is
    //    not merely unevaluated but unrepresentable.
    let seeded = EdgeTracker::new(level.state, &wait.targets);
    let (result, degraded) = drive_wait(&mut conn, socket, terminal, seeded, level, wait)
        .await
        .map_err(|err| match err {
            // The submit succeeded; only the wait failed, so the report must
            // carry the operation id the submit was made under.
            PromptError::Departed {
                from,
                reason,
                delivery,
                ..
            } => PromptError::Departed {
                from,
                reason,
                delivery,
                operation_id: hex.clone(),
            },
            other => other,
        })?;
    Ok(PromptOutcome {
        delivery,
        operation_id: hex,
        agent: record,
        pre_submit_state,
        attempts,
        submit_ms,
        wait: Some(result),
        degraded_to_polling: degraded,
    })
}

/// Wait for a post-result transition on the already-subscribed `conn`.
///
/// The predicate is `tracker`'s, and `tracker` is `phux agent wait`'s
/// [`EdgeTracker`]: a level read never satisfies it, a repeated level is not
/// an edge, a tombstone is a departure. What is local is only the two halves —
/// the push stream on the connection that carried the submit (which is what
/// makes every frame here strictly post-write) and the `GET_METADATA` poll
/// floor that recovers an edge a dropped `try_send` notification never
/// delivered.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0003 binds the CLI to a current-thread runtime"
)]
async fn drive_wait(
    conn: &mut Connection,
    socket: &Path,
    terminal: &TerminalId,
    tracker: EdgeTracker,
    seed: AgentRecord,
    wait: &PromptWait,
) -> Result<(AgentWaitResult, bool), PromptError> {
    use std::cell::{Cell, RefCell};

    let tracker = RefCell::new(tracker);
    let latest = RefCell::new(Some(seed));
    let pushes = Cell::new(0_u32);
    let polls = Cell::new(0_u32);
    let push_ended = Cell::new(false);

    let push_half = async {
        let mut decided: Option<(Verdict, EdgeSource)> = None;
        let _ = stream_items(conn, |item| {
            let WatchItem::AgentState(update) = item else {
                return true;
            };
            pushes.set(pushes.get().saturating_add(1));
            let verdict = tracker.borrow_mut().observe(update.record.as_ref());
            if update.record.is_some() {
                latest.borrow_mut().clone_from(&update.record);
            }
            if matches!(verdict, Verdict::Pending) {
                return true;
            }
            decided = Some((verdict, EdgeSource::Push));
            false
        })
        .await;
        push_ended.set(true);
        match decided {
            Some(decision) => decision,
            // A stream that ends without deciding degrades to the poll floor
            // rather than to a wrong answer.
            None => std::future::pending().await,
        }
    };

    let poll_half = async {
        loop {
            tokio::time::sleep(wait.poll_interval).await;
            if let Ok(record) = fetch_agent_record(socket, terminal).await {
                polls.set(polls.get().saturating_add(1));
                let verdict = tracker.borrow_mut().observe(record.as_ref());
                if record.is_some() {
                    latest.borrow_mut().clone_from(&record);
                }
                if !matches!(verdict, Verdict::Pending) {
                    return (verdict, EdgeSource::Poll);
                }
            }
        }
    };

    let deadline = async {
        match wait.timeout {
            Some(limit) => tokio::time::sleep(limit).await,
            None => std::future::pending::<()>().await,
        }
    };

    let decision = tokio::select! {
        decided = push_half => Some(decided),
        decided = poll_half => Some(decided),
        () = deadline => None,
    };

    let tracker = tracker.into_inner();
    let latest = latest.into_inner();
    let degraded = push_ended.get();
    let result = AgentWaitResult {
        edge: None,
        baseline: tracker.baseline(),
        last: tracker.last(),
        record: latest,
        edges: tracker.edges(),
        polls: polls.get(),
        pushes: pushes.get(),
    };
    match decision {
        Some((Verdict::Satisfied { from, to }, via)) => Ok((
            AgentWaitResult {
                edge: Some(ObservedEdge { from, to, via }),
                ..result
            },
            degraded,
        )),
        Some((Verdict::Departed { from, reason }, _)) => Err(PromptError::Departed {
            from,
            reason,
            delivery: Delivery::Acked,
            operation_id: String::new(),
        }),
        Some((Verdict::Pending, _)) | None => Ok((result, degraded)),
    }
}

/// `phux agent prompt`'s client half: validate the text, build the one batch,
/// and deliver it with a receipt.
///
/// # Errors
///
/// See [`PromptError`].
#[allow(
    clippy::future_not_send,
    reason = "ADR-0003 binds the CLI to a current-thread runtime"
)]
pub async fn prompt_agent(
    socket: &Path,
    terminal: &TerminalId,
    text: &str,
    operation_id: InputOperationId,
    verify: &dyn Fn(&AgentRecord) -> Option<String>,
    wait: Option<&PromptWait>,
) -> Result<PromptOutcome, PromptError> {
    validate_prompt_text(text).map_err(PromptError::Refused)?;
    deliver_acknowledged(
        socket,
        terminal,
        operation_id,
        prompt_events(text),
        verify,
        wait,
    )
    .await
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests"
    )]
    #![allow(
        clippy::future_not_send,
        reason = "the futures under test are !Send by design"
    )]

    use std::cell::RefCell;

    use tokio::net::UnixListener;

    use crate::testkit::{EndOfScript, ScriptSpec, ScriptedServer};

    use super::*;

    fn op_id(fill: u8) -> InputOperationId {
        InputOperationId::new([fill.max(1); 16]).expect("non-zero operation id")
    }

    fn error(code: ErrorCode) -> CommandResult {
        CommandResult::Error {
            code,
            message: "scripted".to_owned(),
        }
    }

    // ---- point 3: the batch shape ------------------------------------

    /// The batch is exactly `[Paste(trusted), Key(Enter)]`, in that order.
    /// Enter last is the whole truncation argument: a partial write can then
    /// only drop the submission, never submit a truncated prompt.
    #[test]
    fn a_prompt_is_one_trusted_paste_then_enter() {
        let events = prompt_events("ship it");
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(
            events[0],
            InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Trusted,
                data: b"ship it".to_vec(),
            })
        );
        assert!(
            matches!(events[1], InputEvent::Key(_)),
            "Enter must be last: {events:?}"
        );
    }

    /// Text that would otherwise be read as a key spec is still prompt text.
    /// `phux agent prompt @7 Enter` prompts the word, it does not press the
    /// key — the paste is built directly rather than through `events_for`'s
    /// named-key grammar precisely so that cannot happen.
    #[test]
    fn prompt_text_is_never_reinterpreted_as_a_key_spec() {
        let events = prompt_events("Enter");
        assert_eq!(
            events[0],
            InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Trusted,
                data: b"Enter".to_vec(),
            })
        );
        assert_eq!(events.len(), 2);
    }

    /// ADR-0076 point 3: a raw newline is refused, not guessed at. The pane
    /// may not have set DEC 2004, in which case the server turns each newline
    /// into a carriage return — N submissions, silently.
    #[test]
    fn multiline_prompt_text_is_refused_before_any_round_trip() {
        assert_eq!(
            validate_prompt_text("do this\nthen that"),
            Err(Refusal::MultilineText { newlines: 1 })
        );
        assert_eq!(validate_prompt_text("").unwrap_err(), Refusal::EmptyText);
        assert_eq!(validate_prompt_text("one line"), Ok(()));
    }

    // ---- oversized payloads ------------------------------------------

    /// The inline ceiling binds at exactly 4096 bytes, and the refusal names
    /// the measured size and the limit so a caller can act on it.
    #[test]
    fn the_inline_prompt_ceiling_binds_at_4096_bytes() {
        let at_limit = "x".repeat(MAX_PROMPT_BYTES);
        assert_eq!(validate_prompt_text(&at_limit), Ok(()));

        let over = "x".repeat(MAX_PROMPT_BYTES + 1);
        assert_eq!(
            validate_prompt_text(&over),
            Err(Refusal::TooLarge {
                unit: "bytes",
                measured: MAX_PROMPT_BYTES + 1,
                limit: MAX_PROMPT_BYTES,
                wire: false,
            })
        );
    }

    /// The wire caps are 65536 bytes and 256 events. A prompt is 2 events, so
    /// the event cap never binds on this verb; the byte cap is checked
    /// client-side so an oversized batch is a local usage error naming the
    /// size, never a bare `INVALID_COMMAND` and never a silent split.
    #[test]
    fn the_wire_caps_are_enforced_client_side_and_never_split() {
        assert_eq!(MAX_APPLY_INPUT_COMMAND_BODY, 64 * 1024);
        assert_eq!(MAX_APPLY_INPUT_EVENTS, 256);
        // A prompt at the inline ceiling is comfortably inside the wire cap.
        assert_eq!(validate_batch(&prompt_events(&"x".repeat(4096))), Ok(()));

        // A payload past the wire cap is refused with `wire: true`, so the
        // message can say "protocol cap" rather than "ceiling".
        let huge = prompt_events(&"x".repeat(MAX_APPLY_INPUT_COMMAND_BODY));
        match validate_batch(&huge) {
            Err(Refusal::TooLarge {
                unit,
                limit,
                wire: true,
                ..
            }) => {
                assert_eq!(unit, "bytes");
                assert_eq!(limit, MAX_APPLY_INPUT_COMMAND_BODY);
            }
            other => panic!("a 64 KiB payload must be refused on the wire cap: {other:?}"),
        }

        // And the event cap, which is what `agent send-keys` can actually
        // reach: a literal run not followed by Enter is one event per char.
        let many: Vec<InputEvent> = std::iter::repeat_n(
            InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Trusted,
                data: Vec::new(),
            }),
            MAX_APPLY_INPUT_EVENTS + 1,
        )
        .collect();
        assert_eq!(
            validate_batch(&many),
            Err(Refusal::TooLarge {
                unit: "events",
                measured: MAX_APPLY_INPUT_EVENTS + 1,
                limit: MAX_APPLY_INPUT_EVENTS,
                wire: true,
            })
        );
    }

    // ---- point 2: one reading per typed error -------------------------

    /// `OK` is the receipt.
    #[test]
    fn ok_is_acked() {
        assert_eq!(classify(&CommandResult::Ok), ApplyVerdict::Acked);
    }

    /// `INPUT_DELIVERY_UNKNOWN` is indeterminate and terminal. It is NOT a
    /// refusal (nothing-written) and NOT a busy (retryable): reading it as
    /// either is the mistake that writes a duplicate prompt.
    #[test]
    fn input_delivery_unknown_is_unknown_and_never_retryable() {
        let verdict = classify(&error(ErrorCode::InputDeliveryUnknown));
        assert!(matches!(verdict, ApplyVerdict::Unknown(_)), "{verdict:?}");
        assert!(!matches!(verdict, ApplyVerdict::Busy(_)));
    }

    /// `INTERNAL_ERROR` is raised both pre- and post-handoff and the code
    /// does not separate them, so it is pessimized to unknown. So is any code
    /// a newer server may add — `ErrorCode` is `#[non_exhaustive]`, and an
    /// optimistic default for an unknown code is how a duplicate gets
    /// written.
    #[test]
    fn internal_error_and_unknown_codes_pessimize_to_unknown() {
        assert!(matches!(
            classify(&error(ErrorCode::InternalError)),
            ApplyVerdict::Unknown(_)
        ));
        assert!(matches!(
            classify(&error(ErrorCode::NotAttached)),
            ApplyVerdict::Unknown(_)
        ));
    }

    /// `RESOURCE_EXHAUSTED` is pre-handoff: nothing was written, and the
    /// identical batch under the identical id may succeed once the
    /// server-wide lane frees.
    #[test]
    fn resource_exhausted_is_the_only_retryable_reading() {
        assert!(matches!(
            classify(&error(ErrorCode::ResourceExhausted)),
            ApplyVerdict::Busy(_)
        ));
    }

    /// The nothing-written-and-cannot-succeed family, each with its own
    /// refusal so the CLI message can say which one happened.
    #[test]
    fn each_pre_handoff_refusal_keeps_its_own_reading() {
        /// One code and the refusal shape it must produce.
        type Case = (ErrorCode, fn(&Refusal) -> bool);
        let cases: &[Case] = &[
            (ErrorCode::InputLeaseHeld, |r| {
                matches!(r, Refusal::InputLeaseHeld(_))
            }),
            (ErrorCode::CanonicalLimitExceeded, |r| {
                matches!(r, Refusal::CanonicalLimitExceeded(_))
            }),
            (ErrorCode::UnsafePaste, |r| {
                matches!(r, Refusal::UnsafePaste(_))
            }),
            (ErrorCode::InvalidCommand, |r| {
                matches!(r, Refusal::InvalidBatch(_))
            }),
            (ErrorCode::PermissionDenied, |r| {
                matches!(r, Refusal::PermissionDenied(_))
            }),
            (ErrorCode::UnsupportedSatelliteRoute, |r| {
                matches!(r, Refusal::SatelliteTarget { .. })
            }),
        ];
        for (code, expect) in cases {
            match classify(&error(*code)) {
                ApplyVerdict::Refused(refusal) => {
                    assert!(expect(&refusal), "{code:?} produced {refusal:?}");
                }
                other => panic!("{code:?} must be a refusal, got {other:?}"),
            }
        }
        assert!(matches!(
            classify(&error(ErrorCode::TerminalNotFound)),
            ApplyVerdict::NotFound(_)
        ));
    }

    // ---- idempotency: the id is never regenerated ---------------------

    /// The rule the whole design rests on: a retry reuses the operation id.
    /// Minting a fresh one is precisely the duplicate `APPLY_INPUT` exists to
    /// prevent, so this asserts every attempt carried the same id — and that
    /// the retry happened at all, since `RESOURCE_EXHAUSTED` wrote nothing.
    #[tokio::test(start_paused = true)]
    async fn a_retry_reuses_the_operation_id_and_never_mints_a_new_one() {
        let id = op_id(0x5a);
        let seen: RefCell<Vec<InputOperationId>> = RefCell::new(Vec::new());
        let (verdict, attempts) = submit_with_backoff(id, &backoff_schedule(&id), |attempt_id| {
            seen.borrow_mut().push(attempt_id);
            let count = seen.borrow().len();
            async move {
                Ok::<_, AttachError>(if count < 3 {
                    ApplyVerdict::Busy("lane busy".to_owned())
                } else {
                    ApplyVerdict::Acked
                })
            }
        })
        .await
        .expect("the fake never fails");

        assert_eq!(verdict, ApplyVerdict::Acked);
        assert_eq!(attempts, 3);
        let seen = seen.into_inner();
        assert_eq!(seen.len(), 3);
        assert!(
            seen.iter().all(|observed| *observed == id),
            "every attempt must carry the id generated once for this invocation"
        );
    }

    /// A verdict that is not `Busy` is never retried — in particular an
    /// unknown, which a retry would either replay verbatim (same id) or
    /// duplicate (new id).
    #[tokio::test(start_paused = true)]
    async fn an_unknown_result_is_submitted_exactly_once() {
        let id = op_id(0x11);
        let calls = std::cell::Cell::new(0_u32);
        let (verdict, attempts) = submit_with_backoff(id, &backoff_schedule(&id), |_| {
            calls.set(calls.get() + 1);
            async { Ok::<_, AttachError>(ApplyVerdict::Unknown("delivery unknown".to_owned())) }
        })
        .await
        .expect("the fake never fails");

        assert!(matches!(verdict, ApplyVerdict::Unknown(_)));
        assert_eq!(attempts, 1);
        assert_eq!(calls.get(), 1);
    }

    /// The backoff budget outlives the server's 5 s completion wait, which is
    /// how long the server-wide admission flag can stay held — **at every
    /// jitter draw**, not merely on average. A floor under ~6 s would fail
    /// spuriously against one wedged pane, so the floor is what is asserted:
    /// the worst schedule any operation id can produce.
    #[test]
    fn the_backoff_floor_outlasts_the_servers_completion_wait() {
        let floor: Duration = BACKOFF_STEPS
            .iter()
            .map(|step| *step * u32::try_from(1_000 - JITTER_PERMILLE).unwrap_or(1) / 1_000)
            .sum();
        assert!(
            floor > Duration::from_secs(6),
            "the worst-case backoff budget {floor:?} must exceed the ~5s admission hold"
        );
        assert!(
            SUBMIT_DEADLINE > Duration::from_secs(5),
            "the submit deadline must outlast the server's own completion wait"
        );
        // Every id's schedule clears the floor and stays inside the band.
        for fill in [0x01, 0x7f, 0xfe, 0xff] {
            let schedule = backoff_schedule(&op_id(fill));
            let total: Duration = schedule.iter().sum();
            assert!(total >= floor, "{fill:#x} produced {total:?}");
            for (jittered, base) in schedule.iter().zip(BACKOFF_STEPS) {
                assert!(*jittered >= base.mul_f64(0.849), "{jittered:?} vs {base:?}");
                assert!(*jittered <= base.mul_f64(1.151), "{jittered:?} vs {base:?}");
            }
        }
        // Jitter is real and id-derived: two ids give different schedules, so
        // two orchestrators colliding on the server-wide lane decorrelate.
        assert_ne!(
            backoff_schedule(&op_id(0x01)),
            backoff_schedule(&op_id(0xfe))
        );
    }

    // ---- satellites: refuse, never downgrade --------------------------

    /// A satellite target is refused **before a socket is opened**, so there
    /// is no path on which it could fall back to fire-and-forget
    /// `ROUTE_INPUT`. Downgrading would make the verb's own success code mean
    /// "in the kernel queue" on one host and "accepted for delivery, maybe
    /// dropped" on another.
    #[tokio::test]
    async fn a_satellite_target_is_refused_rather_than_downgraded() {
        let outcome = prompt_agent(
            // A socket path that cannot exist: reaching it would itself be
            // the bug, since the refusal must precede the connect.
            Path::new("/nonexistent/phux-must-not-connect.sock"),
            &TerminalId::satellite("devbox", 3),
            "ship it",
            op_id(0x22),
            &|_record| None,
            None,
        )
        .await;
        match outcome {
            Err(PromptError::Refused(Refusal::SatelliteTarget { host })) => {
                assert_eq!(host, "devbox");
            }
            other => panic!("a satellite target must be refused: {other:?}"),
        }
    }

    /// The same refusal covers the server-side code, in case a future
    /// topology puts a satellite id past the client-side gate.
    #[test]
    fn the_server_side_satellite_code_is_also_a_refusal() {
        assert!(matches!(
            classify(&error(ErrorCode::UnsupportedSatelliteRoute)),
            ApplyVerdict::Refused(Refusal::SatelliteTarget { .. })
        ));
    }

    // ---- the acknowledged path is required, never approximated -------

    /// A server that does not advertise `ACKNOWLEDGED_INPUT` is **refused**,
    /// not downgraded. The shared scripted server negotiates
    /// `ServerCapabilities::new()` — no features — so it is exactly the older
    /// server this gate exists for.
    ///
    /// Downgrading to `ROUTE_INPUT` would make one verb's success code mean
    /// "every byte is in the kernel queue" against one server and "accepted
    /// for delivery, possibly dropped on a full mailbox" against another. A
    /// caller cannot branch on a code whose meaning depends on the host.
    ///
    /// The rest of the wire contract — the receipt, the same-id retry, the
    /// terminal unknown, and the post-result edge gate — needs a server that
    /// advertises the capability *and* can script an `APPLY_INPUT` refusal,
    /// neither of which the shared harness expresses. It lives in
    /// `tests/agent_prompt_wire.rs`.
    #[tokio::test]
    async fn a_server_without_acknowledged_input_is_refused_not_downgraded() {
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = UnixListener::bind(&socket).expect("bind scripted server");
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let spec = ScriptSpec::new()
                    .metadata(|_scope, key| {
                        (key == TERMINAL_AGENT_KEY).then(|| {
                            br#"{"name":"reviewer","kind":"claude","state":"working"}"#.to_vec()
                        })
                    })
                    .end(EndOfScript::ServeUntilDetach);
                tokio::spawn(async move {
                    ScriptedServer::on_stream(stream, spec).run().await;
                });
            }
        });

        let outcome = prompt_agent(
            &socket,
            &TerminalId::local(7),
            "ship it",
            op_id(0x33),
            &|_record| None,
            None,
        )
        .await;
        assert!(
            matches!(
                outcome,
                Err(PromptError::Refused(Refusal::NoAcknowledgedInput))
            ),
            "an older server must be refused rather than downgraded: {outcome:?}"
        );
    }
}
