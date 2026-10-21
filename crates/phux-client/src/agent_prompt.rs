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
use phux_protocol::wire::frame::{Command, FrameKind, Scope};
#[cfg(test)]
use phux_protocol::wire::frame::{CommandResult, ErrorCode};

use crate::agent_meta::{AgentMetaState, AgentRecord, RESOURCE_AGENT_KEY, parse_agent_record};
use crate::agent_wait::{
    AgentWaitError, AgentWaitResult, EdgeTracker, WaitShared, deadline, finish, poll_floor,
    record_from_frame, watch_pushes,
};
use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::attach::input::StdinParser;
use crate::watch::subscribe;

mod outcome;
mod verdict;

pub use outcome::{PromptError, PromptOutcome, PromptWait};
pub use verdict::{ApplyVerdict, Delivery, Refusal, classify};

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

/// Refuse a batch over either `APPLY_INPUT` wire cap as a local usage error,
/// never splitting it (`docs/spec/input.md`).
///
/// The rule is
/// [`phux_client_core::input_replay::validate_apply_input_batch`], the same
/// one the replay journal applies.
///
/// # Errors
///
/// Returns [`Refusal::TooLarge`] with `wire: true` for either cap.
pub fn validate_batch(events: &[InputEvent]) -> Result<(), Refusal> {
    use phux_client_core::input_replay::{BatchTooLarge, validate_apply_input_batch};

    validate_apply_input_batch(events).map_err(|too_large| {
        let (unit, measured, limit) = match too_large {
            BatchTooLarge::Events { measured, limit } => ("events", measured, limit),
            BatchTooLarge::Bytes { measured, limit } => ("bytes", measured, limit),
        };
        Refusal::TooLarge {
            unit,
            measured,
            limit,
            wire: true,
        }
    })
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
mod tests;
