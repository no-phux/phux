//! Acknowledged input delivery to an agent (ADR-0053, ADR-0076): the client
//! half of `phux agent prompt` and `phux agent send-keys`.
//!
//! `APPLY_INPUT`'s `OK` means every byte was written and flushed into the
//! pane's tty input queue (L1 §6.2.1): more than `ROUTE_INPUT` attests, less
//! than consumption. Admission is per Terminal and held across a 5 s
//! completion wait, so the submit deadline and the `RESOURCE_EXHAUSTED`
//! backoff both outlast it; only a concurrent write to the same pane collides,
//! and prompts to different panes proceed in parallel.
//!
//! Two rules never soften: a retry reuses the caller's operation id (a fresh
//! id is exactly the duplicate this exists to prevent), and
//! `INPUT_DELIVERY_UNKNOWN` is terminal (recover by reading the pane). The
//! `--wait` half reuses [`crate::agent_wait`]'s [`EdgeTracker`], seeded only
//! after the `APPLY_INPUT` result on the one subscribed connection, so a
//! pre-write transition cannot satisfy it (ADR-0076 point 6).

use std::path::Path;
use std::time::{Duration, Instant};

use phux_client_core::input_replay::operation_id_hex;
use phux_protocol::ids::{InputOperationId, ResourceId};
use phux_protocol::input::InputEvent;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, MAX_APPLY_INPUT_COMMAND_BODY,
    MAX_APPLY_INPUT_EVENTS, Scope,
};

use crate::agent_meta::{AgentMetaState, AgentRecord, RESOURCE_AGENT_KEY, parse_agent_record};
use crate::agent_wait::{
    AgentWaitError, AgentWaitResult, DepartureReason, EdgeTracker, WaitShared, deadline, finish,
    poll_floor, record_from_frame, watch_pushes,
};
use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::attach::input::StdinParser;
use crate::watch::subscribe;

/// Inline prompt-text ceiling, in bytes.
///
/// Bound by the tty input queue, not the 64 KiB wire cap: a larger write can
/// block on an agent that is not draining and read as unknown.
pub const MAX_PROMPT_BYTES: usize = 4096;

/// Client-side deadline for one `APPLY_INPUT` round trip; must exceed the
/// server's 5 s completion wait (`ACKNOWLEDGED_COMPLETION_TIMEOUT`).
pub const SUBMIT_DEADLINE: Duration = Duration::from_secs(8);

/// Unjittered `RESOURCE_EXHAUSTED` backoff steps (same operation id each
/// time). The jittered floor, 6.12 s, must clear the 5 s admission hold.
const BACKOFF_STEPS: &[Duration] = &[
    Duration::from_millis(200),
    Duration::from_millis(600),
    Duration::from_millis(1_600),
    Duration::from_millis(2_400),
    Duration::from_millis(2_400),
];

/// Jitter half-width, in permille of each backoff step: +/-15 %.
const JITTER_PERMILLE: u64 = 150;

/// Correlation id for the pre-submit ownership read.
const OWNERSHIP_REQUEST_ID: u32 = 1;

/// Correlation id for the `APPLY_INPUT` submit, right after the ownership
/// read. Safe to reuse on retry: only a completed round trip is retried (a
/// timeout is terminal `Unknown`), so no stale reply can be in flight.
const SUBMIT_REQUEST_ID: u32 = 2;

/// How the batch ended up, as reported in `--json`'s `delivery` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Every byte was accepted into the pane's tty input queue.
    Acked,
    /// Indeterminate: some, all, or none of the bytes may have reached the
    /// tty. Terminal — never retried.
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

/// A refusal in which nothing was written and which resubmitting the
/// identical batch cannot fix (exit 2). `RESOURCE_EXHAUSTED` is
/// [`ApplyVerdict::Busy`] instead: it can succeed unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The prompt text is empty; there is nothing to submit.
    EmptyText,
    /// The prompt text carries a raw newline (ADR-0076 point 3): without DEC
    /// 2004, which no client can observe, each would become a submission.
    MultilineText {
        /// How many raw newlines the text carries.
        newlines: usize,
    },
    /// The payload is over a size ceiling; `wire` marks a protocol cap.
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
    /// The target is a federation satellite; `APPLY_INPUT` is local-only and
    /// is never downgraded to `ROUTE_INPUT`.
    SatelliteTarget {
        /// The satellite host token the target named.
        host: String,
    },
    /// The server does not advertise `ACKNOWLEDGED_INPUT`.
    NoAcknowledgedInput,
    /// The pane declares no `phux.agent/v1` record to verify against.
    NoAgentRecord,
    /// The pane hosts a different agent than the caller named; the string
    /// describes the occupant.
    AgentMismatch(String),
    /// Another client holds the pane's input lease (ADR-0033); these verbs
    /// never seize it.
    InputLeaseHeld(String),
    /// A canonical-mode pane would have silently truncated a batch with no
    /// terminator; refused before any byte was written.
    CanonicalLimitExceeded(String),
    /// An untrusted paste failed the pane's policy (unreachable here: these
    /// verbs send `Trusted`).
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyVerdict {
    /// Every byte was accepted into the tty input queue and flushed.
    Acked,
    /// The server-wide lane was busy; nothing written, retried under the
    /// same operation id.
    Busy(String),
    /// Nothing was written for another proven reason (no PTY, writer queue
    /// full or closed). Not auto-retried, but safe to resubmit.
    NotWritten(String),
    /// Nothing was written and the identical batch cannot succeed.
    Refused(Refusal),
    /// The Terminal is gone or was never here.
    NotFound(String),
    /// Delivery is indeterminate. Terminal: a same-id retry replays it and a
    /// new-id retry duplicates.
    Unknown(String),
}

/// Map one `APPLY_INPUT` reply onto its single reading.
///
/// `INTERNAL_ERROR`, `INPUT_DELIVERY_UNKNOWN`, and any code or shape this
/// build cannot name pessimize to [`ApplyVerdict::Unknown`]: an optimistic
/// default is how a duplicate gets written.
#[must_use]
pub fn classify(result: &CommandResult) -> ApplyVerdict {
    let (code, message) = match result {
        CommandResult::Ok | CommandResult::OkWith(_) => return ApplyVerdict::Acked,
        CommandResult::Error { code, message } => (*code, message.clone()),
        _ => {
            return ApplyVerdict::Unknown(
                "the server answered APPLY_INPUT with a result this build cannot read".to_owned(),
            );
        }
    };
    match code {
        ErrorCode::ResourceExhausted => ApplyVerdict::Busy(message),
        ErrorCode::InputNotWritten => ApplyVerdict::NotWritten(message),
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
        _ => ApplyVerdict::Unknown(message),
    }
}

/// The backoff schedule for `operation_id`, jittered by `JITTER_PERMILLE`
/// from the id's own random bytes: colliding callers decorrelate, and a
/// schedule is reproducible from the id.
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
/// One batch with `Enter` last, so a partial write can only drop the
/// submission, never submit a truncated prompt. `Enter` comes from the
/// interactive [`StdinParser`], so it matches `phux send-keys … Enter`.
#[must_use]
pub fn prompt_events(text: &str) -> Vec<InputEvent> {
    let mut events = vec![InputEvent::Paste(PasteEvent {
        trust: PasteTrust::Trusted,
        data: text.as_bytes().to_vec(),
    })];
    events.extend(StdinParser::default().feed(b"\r"));
    events
}

/// Refuse empty, multi-line, or oversized prompt text before connecting.
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

/// Conservative upper bound on the encoded `APPLY_INPUT` command body.
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

/// Refuse a batch over [`MAX_APPLY_INPUT_EVENTS`] or
/// [`MAX_APPLY_INPUT_COMMAND_BODY`] as a local usage error, never splitting
/// it (`docs/spec/input.md`).
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

/// Whether `conn`'s server advertised `ACKNOWLEDGED_INPUT` (`false` when
/// unnegotiated, so callers refuse rather than proceed).
#[must_use]
pub fn supports_acknowledged_input(conn: &Connection) -> bool {
    conn.negotiated_bootstrap().is_some_and(|bootstrap| {
        bootstrap
            .server_features
            .contains(phux_protocol::caps::ServerFeature::AcknowledgedInput)
    })
}

/// Submit `events` to `terminal` once, under `operation_id`, and classify it.
///
/// Interleaved frames are returned, not dropped: a subscribed connection's
/// `METADATA_CHANGED` is never re-sent. Elapsing [`SUBMIT_DEADLINE`] is
/// [`ApplyVerdict::Unknown`].
///
/// # Errors
///
/// Propagates [`AttachError`] from the transport.
pub async fn apply_input_once(
    conn: &mut Connection,
    terminal: &ResourceId,
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

/// Run `attempt` under `operation_id`, retrying only [`ApplyVerdict::Busy`]
/// on `schedule`, always with the same id (nothing here can mint one).
/// Returns the final verdict and the attempt count.
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
    /// Lowercase hex of the operation id, for correlation (ADR-0076 point 2).
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
    /// The lane never freed; nothing written, safe to re-run. Exit 1.
    #[error(
        "the pane's acknowledged input stayed busy across {attempts} attempts \
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
    /// Nothing was written for a non-lane reason; safe to resubmit. Exit 1.
    #[error("nothing was written (operation {operation_id}): {message}")]
    NotWritten {
        /// The operation id, in hex.
        operation_id: String,
        /// The server's diagnostic.
        message: String,
    },
    /// The Terminal is gone. Exit 1.
    #[error("terminal not found: {0}")]
    NotFound(String),
    /// Terminal: some, all, or none of the bytes reached the tty; any retry
    /// replays or duplicates. Read the pane instead. Exit 1.
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

/// Read the pane's record over `conn`: the last one observed at or before
/// the answer, interleaved pushes included (latest arrival wins).
async fn read_pre_submit_record(
    conn: &mut Connection,
    terminal: &ResourceId,
) -> Result<Option<AgentRecord>, AttachError> {
    let (answer, interleaved) = conn
        .request_metadata(
            OWNERSHIP_REQUEST_ID,
            Scope::Resource(terminal.clone()),
            RESOURCE_AGENT_KEY.to_owned(),
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

/// Re-verify the pane's occupant from the published record (client-side, so
/// a manifest can never make a pane refuse keystrokes).
#[allow(clippy::future_not_send, reason = "the verifier is a bare `&dyn Fn`")]
async fn verified_occupant(
    conn: &mut Connection,
    terminal: &ResourceId,
    verify: &dyn Fn(&AgentRecord) -> Option<String>,
) -> Result<AgentRecord, PromptError> {
    let Some(record) = read_pre_submit_record(conn, terminal).await? else {
        return Err(PromptError::Refused(Refusal::NoAgentRecord));
    };
    if let Some(mismatch) = verify(&record) {
        return Err(PromptError::Refused(Refusal::AgentMismatch(mismatch)));
    }
    Ok(record)
}

/// What one `APPLY_INPUT` submit produced, with everything the report needs
/// from it.
struct Submitted {
    /// The verdict the last attempt read.
    verdict: ApplyVerdict,
    /// How many attempts the batch took.
    attempts: u32,
    /// The retry budget the schedule allowed, in milliseconds.
    budget_ms: u64,
    /// Wall time the whole submit took, in milliseconds.
    submit_ms: u64,
    /// Frames the server pushed ahead of the result.
    interleaved: Vec<FrameKind>,
}

/// Submit the batch under `operation_id`, retried only on
/// `RESOURCE_EXHAUSTED` and only under that same id.
async fn submit_batch(
    conn: &mut Connection,
    terminal: &ResourceId,
    operation_id: InputOperationId,
    events: Vec<InputEvent>,
) -> Result<Submitted, AttachError> {
    let schedule = backoff_schedule(&operation_id);
    let budget_ms = schedule
        .iter()
        .map(|step| u64::try_from(step.as_millis()).unwrap_or(u64::MAX))
        .sum();
    let started = Instant::now();
    let mut interleaved: Vec<FrameKind> = Vec::new();
    let (verdict, attempts) = submit_with_backoff(operation_id, &schedule, async |id| {
        let (verdict, frames) =
            apply_input_once(&mut *conn, terminal, id, events.clone(), SUBMIT_REQUEST_ID).await?;
        interleaved.extend(frames);
        Ok::<_, AttachError>(verdict)
    })
    .await?;
    let submit_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(Submitted {
        verdict,
        attempts,
        budget_ms,
        submit_ms,
        interleaved,
    })
}

/// Read the submit's verdict as a delivery, or as the failure it reports.
fn delivery_from_verdict(
    verdict: ApplyVerdict,
    attempts: u32,
    budget_ms: u64,
    hex: &str,
) -> Result<Delivery, PromptError> {
    match verdict {
        ApplyVerdict::Acked => Ok(Delivery::Acked),
        ApplyVerdict::Busy(message) => Err(PromptError::LaneBusy {
            attempts,
            budget_ms,
            message,
            operation_id: hex.to_owned(),
        }),
        ApplyVerdict::NotWritten(message) => Err(PromptError::NotWritten {
            operation_id: hex.to_owned(),
            message,
        }),
        ApplyVerdict::Refused(refusal) => Err(PromptError::Refused(refusal)),
        ApplyVerdict::NotFound(message) => Err(PromptError::NotFound(message)),
        ApplyVerdict::Unknown(message) => Err(PromptError::DeliveryUnknown {
            operation_id: hex.to_owned(),
            message,
        }),
    }
}

/// The bytes went somewhere, but not provably to the occupant the ownership
/// check passed on.
fn occupant_changed(detail: String, hex: &str, delivery: Delivery) -> PromptError {
    PromptError::OccupantChanged {
        detail,
        operation_id: hex.to_owned(),
        delivery,
    }
}

/// The last record observed before the result, having checked every frame
/// pushed ahead of it for an occupant change (evidence of who got the bytes,
/// never of completion).
fn confirm_occupant(
    interleaved: &[FrameKind],
    terminal: &ResourceId,
    record: &AgentRecord,
    hex: &str,
    delivery: Delivery,
) -> Result<AgentRecord, PromptError> {
    let mut level = record.clone();
    for frame in interleaved {
        let Some(observed) = record_from_frame(frame, terminal) else {
            continue;
        };
        let Some(observed) = observed else {
            return Err(occupant_changed(
                "the phux.agent/v1 record was deleted".to_owned(),
                hex,
                delivery,
            ));
        };
        if !observed.name.eq_ignore_ascii_case(&record.name) {
            return Err(occupant_changed(
                format!("'{}' replaced '{}'", observed.name, record.name),
                hex,
                delivery,
            ));
        }
        if observed.state == AgentMetaState::Unknown && record.state != AgentMetaState::Unknown {
            return Err(occupant_changed(
                format!("'{}' withdrew its state to unknown", observed.name),
                hex,
                delivery,
            ));
        }
        level = observed;
    }
    Ok(level)
}

/// Deliver a validated batch to `terminal` with a receipt.
///
/// The ordering is the contract: refuse satellites before connecting; subscribe; gate on
/// `ACKNOWLEDGED_INPUT`; `GET_METADATA` (id 1) and `verify`; `APPLY_INPUT`
/// (id 2), retried only on `RESOURCE_EXHAUSTED` under the same id; report an
/// occupant change seen before the result; with `wait`, seed an
/// [`EdgeTracker`] after the result and wait on the same connection.
///
/// # Errors
///
/// See [`PromptError`]. `verify` returning `Some(description)` is
/// [`Refusal::AgentMismatch`].
#[allow(clippy::future_not_send, reason = "current-thread runtime (ADR-0003)")]
pub async fn deliver_acknowledged(
    socket: &Path,
    terminal: &ResourceId,
    operation_id: InputOperationId,
    events: Vec<InputEvent>,
    verify: &dyn Fn(&AgentRecord) -> Option<String>,
    wait: Option<&PromptWait>,
) -> Result<PromptOutcome, PromptError> {
    let hex = operation_id_hex(&operation_id);
    if let Some(host) = terminal.host() {
        return Err(PromptError::Refused(Refusal::SatelliteTarget {
            host: host.as_str().to_owned(),
        }));
    }
    validate_batch(&events).map_err(PromptError::Refused)?;

    let mut conn = subscribe(socket, Some(terminal.clone())).await?;
    if !supports_acknowledged_input(&conn) {
        return Err(PromptError::Refused(Refusal::NoAcknowledgedInput));
    }
    let record = verified_occupant(&mut conn, terminal, verify).await?;
    let pre_submit_state = record.state;
    let submitted = submit_batch(&mut conn, terminal, operation_id, events).await?;
    let delivery = delivery_from_verdict(
        submitted.verdict,
        submitted.attempts,
        submitted.budget_ms,
        &hex,
    )?;

    let level = confirm_occupant(&submitted.interleaved, terminal, &record, &hex, delivery)?;

    // The tracker is built after the result: the pre-submit level is
    // unrepresentable, not merely unevaluated.
    let (wait, degraded_to_polling) = match wait {
        Some(wait) => {
            let (result, degraded) =
                drive_wait(&mut conn, socket, terminal, level, wait, &hex).await?;
            (Some(result), degraded)
        }
        None => (None, false),
    };
    drop(conn);
    Ok(PromptOutcome {
        delivery,
        operation_id: hex,
        agent: record,
        pre_submit_state,
        attempts: submitted.attempts,
        submit_ms: submitted.submit_ms,
        wait,
        degraded_to_polling,
    })
}

/// Wait for a post-result transition on the already-subscribed `conn`, with
/// `phux agent wait`'s push and poll halves; the poll floor here never gives
/// up before the deadline.
#[allow(clippy::future_not_send, reason = "current-thread runtime (ADR-0003)")]
async fn drive_wait(
    conn: &mut Connection,
    socket: &Path,
    terminal: &ResourceId,
    seed: AgentRecord,
    wait: &PromptWait,
    hex: &str,
) -> Result<(AgentWaitResult, bool), PromptError> {
    let shared = WaitShared::new(EdgeTracker::new(seed.state, &wait.targets), Some(seed));
    let decision = tokio::select! {
        decided = watch_pushes(conn, &shared) => Some(decided),
        decided = poll_floor(socket, terminal, wait.poll_interval, &shared, None) => Some(decided),
        () = deadline(wait.timeout) => None,
    }
    .transpose();
    let degraded = shared.push_ended.get();
    let result = decision.and_then(|decision| finish(decision, shared));
    match result {
        Ok(result) => Ok((result, degraded)),
        Err(AgentWaitError::Departed { from, reason, .. }) => Err(PromptError::Departed {
            from,
            reason,
            delivery: Delivery::Acked,
            operation_id: hex.to_owned(),
        }),
        Err(AgentWaitError::Transport(err)) => Err(err.into()),
        Err(AgentWaitError::NoRecord) => Err(PromptError::Refused(Refusal::NoAgentRecord)),
    }
}

/// `phux agent prompt`'s client half: validate the text, build the one batch,
/// and deliver it with a receipt.
///
/// # Errors
///
/// See [`PromptError`].
#[allow(clippy::future_not_send, reason = "current-thread runtime (ADR-0003)")]
pub async fn prompt_agent(
    socket: &Path,
    terminal: &ResourceId,
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

    use std::cell::{Cell, RefCell};

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

    /// The batch is `[Paste(trusted), Key(Enter)]`, Enter last; text that
    /// reads like a key spec (`Enter`) is still pasted as text.
    #[test]
    fn a_prompt_is_one_trusted_paste_then_enter() {
        let events = prompt_events("Enter");
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(
            events[0],
            InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Trusted,
                data: b"Enter".to_vec(),
            })
        );
        assert!(
            matches!(events[1], InputEvent::Key(_)),
            "Enter must be last: {events:?}"
        );
    }

    #[test]
    fn multiline_prompt_text_is_refused_before_any_round_trip() {
        assert_eq!(
            validate_prompt_text("do this\nthen that"),
            Err(Refusal::MultilineText { newlines: 1 })
        );
        assert_eq!(validate_prompt_text("").unwrap_err(), Refusal::EmptyText);
        assert_eq!(validate_prompt_text("one line"), Ok(()));
    }

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

    /// Both wire caps are enforced client-side with `wire: true`.
    #[test]
    fn the_wire_caps_are_enforced_client_side_and_never_split() {
        assert_eq!(validate_batch(&prompt_events(&"x".repeat(4096))), Ok(()));

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

    /// Every reply maps onto exactly one reading. Delivery-unknown, internal
    /// errors, and unnameable codes pessimize to `Unknown`; only
    /// `RESOURCE_EXHAUSTED` is `Busy`; `INPUT_NOT_WRITTEN` is neither.
    #[test]
    fn every_reply_has_exactly_one_reading() {
        type Case = (CommandResult, fn(&ApplyVerdict) -> bool);
        let cases: &[Case] = &[
            (CommandResult::Ok, |v| *v == ApplyVerdict::Acked),
            (error(ErrorCode::InputDeliveryUnknown), |v| {
                matches!(v, ApplyVerdict::Unknown(_))
            }),
            (error(ErrorCode::InternalError), |v| {
                matches!(v, ApplyVerdict::Unknown(_))
            }),
            (error(ErrorCode::NotAttached), |v| {
                matches!(v, ApplyVerdict::Unknown(_))
            }),
            (error(ErrorCode::InputNotWritten), |v| {
                matches!(v, ApplyVerdict::NotWritten(_))
            }),
            (error(ErrorCode::ResourceExhausted), |v| {
                matches!(v, ApplyVerdict::Busy(_))
            }),
            (error(ErrorCode::TerminalNotFound), |v| {
                matches!(v, ApplyVerdict::NotFound(_))
            }),
            (error(ErrorCode::InputLeaseHeld), |v| {
                matches!(v, ApplyVerdict::Refused(Refusal::InputLeaseHeld(_)))
            }),
            (error(ErrorCode::CanonicalLimitExceeded), |v| {
                matches!(v, ApplyVerdict::Refused(Refusal::CanonicalLimitExceeded(_)))
            }),
            (error(ErrorCode::UnsafePaste), |v| {
                matches!(v, ApplyVerdict::Refused(Refusal::UnsafePaste(_)))
            }),
            (error(ErrorCode::InvalidCommand), |v| {
                matches!(v, ApplyVerdict::Refused(Refusal::InvalidBatch(_)))
            }),
            (error(ErrorCode::PermissionDenied), |v| {
                matches!(v, ApplyVerdict::Refused(Refusal::PermissionDenied(_)))
            }),
            (error(ErrorCode::UnsupportedSatelliteRoute), |v| {
                matches!(v, ApplyVerdict::Refused(Refusal::SatelliteTarget { .. }))
            }),
        ];
        for (result, check) in cases {
            let verdict = classify(result);
            assert!(check(&verdict), "{result:?} read as {verdict:?}");
        }
    }

    /// A retry reuses the operation id: a fresh one is exactly the duplicate
    /// `APPLY_INPUT` exists to prevent.
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

    /// A non-`Busy` verdict, in particular an unknown, is never retried.
    #[tokio::test(start_paused = true)]
    async fn an_unknown_result_is_submitted_exactly_once() {
        let id = op_id(0x11);
        let calls = Cell::new(0_u32);
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

    /// The backoff budget outlives the server's 5 s admission hold at every
    /// jitter draw, and jitter decorrelates distinct ids.
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
        for fill in [0x01, 0x7f, 0xfe, 0xff] {
            let schedule = backoff_schedule(&op_id(fill));
            let total: Duration = schedule.iter().sum();
            assert!(total >= floor, "{fill:#x} produced {total:?}");
            for (jittered, base) in schedule.iter().zip(BACKOFF_STEPS) {
                assert!(*jittered >= base.mul_f64(0.849), "{jittered:?} vs {base:?}");
                assert!(*jittered <= base.mul_f64(1.151), "{jittered:?} vs {base:?}");
            }
        }
        assert_ne!(
            backoff_schedule(&op_id(0x01)),
            backoff_schedule(&op_id(0xfe))
        );
    }

    /// A satellite target is refused before a socket is opened (the path
    /// cannot exist), never downgraded to `ROUTE_INPUT`.
    #[tokio::test]
    async fn a_satellite_target_is_refused_rather_than_downgraded() {
        let outcome = prompt_agent(
            Path::new("/nonexistent/phux-must-not-connect.sock"),
            &ResourceId::satellite("devbox", 3),
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

    /// A server without `ACKNOWLEDGED_INPUT` (the scripted server advertises
    /// no features) is refused, not downgraded. The rest of the wire contract
    /// lives in `tests/connection/agent_prompt_wire.rs`.
    #[tokio::test]
    async fn a_server_without_acknowledged_input_is_refused_not_downgraded() {
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = UnixListener::bind(&socket).expect("bind scripted server");
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let spec = ScriptSpec::new()
                    .metadata(|_scope, key| {
                        (key == RESOURCE_AGENT_KEY).then(|| {
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
            &ResourceId::local(7),
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
