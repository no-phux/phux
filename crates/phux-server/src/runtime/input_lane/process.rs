//! Processing one queued input: attached, headless, and acknowledged batches.

use phux_protocol::ids::InputOperationId;
use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::{CommandResult, ErrorCode};
use phux_protocol::{MAX_APPLY_INPUT_COMMAND_BODY, MAX_APPLY_INPUT_EVENTS};
use tokio::sync::{mpsc, oneshot};

use crate::state::{ClientId, SharedState, TerminalInput};
use crate::terminal_actor::{EncodedInputRequest, InputCredit, LaneAhead, TerminalHandle};

use super::{
    AcknowledgedReservation, CompletionTicket, CompletionWaiterHandle, LaneEncoderSet,
    PendingCompletion, SharedOperationCache, credit, deadline_from, encode_input, encoder_for,
    handoff_encoded, is_live_terminal_input, terminal_input_from_event,
    with_attached_input_destination, with_route_input_destination,
};

/// Hand `bytes` to the destination the gate just re-resolved, unless its
/// actor was replaced since `encoded_for` (the bytes belong to the old one).
fn redeliver(
    encoded_for: &TerminalHandle,
    current: &super::super::InputDestination,
    bytes: Option<Vec<u8>>,
    input: &TerminalInput,
    credit: Option<InputCredit>,
    lane_ahead: Option<LaneAhead>,
) -> Result<bool, CommandResult> {
    if !encoded_for
        .input_snapshot
        .same_channel(&current.handle.input_snapshot)
    {
        drop(lane_ahead);
        return Ok(false);
    }
    handoff_encoded(
        current.pane,
        &current.handle,
        bytes,
        crate::terminal_actor::echo_probe_for(input),
        credit,
        lane_ahead,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the lane guard travels with the key through the existing handoff arguments"
)]
pub(super) fn process_attached(
    state: &SharedState,
    encoders: &mut std::collections::HashMap<phux_core::ids::ResourceId, LaneEncoderSet>,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    input: &TerminalInput,
    frame_label: &'static str,
    credit: Option<InputCredit>,
    lane_ahead: Option<LaneAhead>,
) -> bool {
    let is_focus_gained = matches!(
        input,
        TerminalInput::Focus(phux_protocol::input::focus::FocusEvent::Gained)
    );
    let live = is_live_terminal_input(input);
    let Some(destination) = with_attached_input_destination(
        state,
        client_id,
        terminal_id,
        frame_label,
        std::convert::identity,
    ) else {
        return false;
    };
    let bytes = encode_input(encoders, destination.pane, &destination.handle, input);
    // Re-gate and send under one state lock, so a lease change cannot slip
    // between gate and delivery while encoding happens off-lock.
    let delivered =
        with_attached_input_destination(state, client_id, terminal_id, frame_label, |current| {
            redeliver(
                &destination.handle,
                &current,
                bytes,
                input,
                credit,
                lane_ahead,
            )
        });
    let accepted = match delivered {
        Some(Ok(accepted)) => accepted,
        Some(Err(refusal)) => {
            refuse_attached(state, client_id, refusal);
            false
        }
        None => false,
    };
    if accepted && is_focus_gained {
        crate::hooks::fire_hook(
            state,
            crate::hooks::HookEvent::focus_changed(terminal_id, client_id),
        );
    }
    accepted && live
}

/// `INPUT_*` frames have no reply, so a refusal after the gates is pushed as
/// an uncorrelated `ERROR` (docs/protocol.md#input): input is never lost silently.
fn refuse_attached(state: &SharedState, client_id: ClientId, refusal: CommandResult) {
    let CommandResult::Error { code, message } = refusal else {
        return;
    };
    state.with(|s| {
        if let Some(mailbox) = s.client_mailbox(client_id) {
            let _ = mailbox.try_send(crate::mailbox::Outbound::Frame(
                phux_protocol::wire::frame::FrameKind::Error {
                    request_id: None,
                    code,
                    message,
                },
            ));
        }
    });
}

pub(super) fn process_headless(
    state: &SharedState,
    encoders: &mut std::collections::HashMap<phux_core::ids::ResourceId, LaneEncoderSet>,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    event: InputEvent,
    credit: Option<InputCredit>,
    lane_ahead: Option<LaneAhead>,
) -> CommandResult {
    let destination =
        match with_route_input_destination(state, client_id, terminal_id, std::convert::identity) {
            Ok(destination) => destination,
            Err(result) => return result,
        };
    let input = match terminal_input_from_event(event) {
        Ok(input) => input,
        Err(result) => return result,
    };
    let bytes = encode_input(encoders, destination.pane, &destination.handle, &input);
    match with_route_input_destination(state, client_id, terminal_id, |current| {
        redeliver(
            &destination.handle,
            &current,
            bytes,
            &input,
            credit,
            lane_ahead,
        )
    }) {
        Ok(Ok(_)) => CommandResult::Ok,
        Ok(Err(result)) | Err(result) => result,
    }
}

pub(super) fn unsupported_satellite_route() -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::UnsupportedSatelliteRoute,
        message: "APPLY_INPUT to a satellite is forwarded by a federation hub, not run here"
            .to_owned(),
    }
}

pub(super) fn invalid_command(message: &str) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::InvalidCommand,
        message: message.to_owned(),
    }
}

pub(super) fn acknowledged_resource_exhausted(message: &str) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::ResourceExhausted,
        message: message.to_owned(),
    }
}

/// An `APPLY_INPUT` refused or abandoned before it reached any pane actor's
/// mailbox: provably nothing was written, so any retry is safe.
pub(super) fn acknowledged_not_written(message: &str) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::InputNotWritten,
        message: message.to_owned(),
    }
}

/// An acknowledged batch that passed every pre-handoff gate: authority, dedupe
/// binding, paste safety, and encoding.
struct PreparedBatch {
    destination: super::super::InputDestination,
    bytes: Vec<u8>,
}

fn prepare_acknowledged_batch(
    state: &SharedState,
    encoders: &mut std::collections::HashMap<phux_core::ids::ResourceId, LaneEncoderSet>,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    events: Vec<InputEvent>,
) -> Result<PreparedBatch, CommandResult> {
    if events.is_empty() || events.len() > MAX_APPLY_INPUT_EVENTS {
        return Err(invalid_command("APPLY_INPUT requires 1..=256 events"));
    }

    let mut inputs = Vec::with_capacity(events.len());
    for event in events {
        inputs.push(terminal_input_from_event(event)?);
    }
    let destination =
        with_route_input_destination(state, client_id, terminal_id, std::convert::identity)?;
    let encoder = match encoder_for(encoders, destination.pane, &destination.handle) {
        Ok(encoder) => encoder,
        Err(err) => {
            tracing::warn!(error = %err, pane = ?destination.pane, "APPLY_INPUT encoder unavailable");
            return Err(invalid_command("APPLY_INPUT encoder unavailable"));
        }
    };
    if inputs.iter().any(
        |input| matches!(input, TerminalInput::Paste(event) if crate::input::paste::PerTerminalPasteEncoder::would_reject(event)),
    ) {
        return Err(CommandResult::Error {
            code: ErrorCode::UnsafePaste,
            message: "APPLY_INPUT batch contains an unsafe paste".to_owned(),
        });
    }

    let snapshot = *encoder.snapshot.borrow();
    let mut bytes = Vec::new();
    for input in &inputs {
        match encoder.encode_with_snapshot(input, snapshot) {
            Ok(Some(event_bytes)) => {
                if bytes.len().saturating_add(event_bytes.len()) > MAX_APPLY_INPUT_COMMAND_BODY {
                    return Err(invalid_command(
                        "APPLY_INPUT encoded PTY bytes exceed 64 KiB",
                    ));
                }
                bytes.extend_from_slice(&event_bytes);
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(error = %err, pane = ?destination.pane, "APPLY_INPUT encode failed");
                return Err(invalid_command("APPLY_INPUT event encoding failed"));
            }
        }
    }

    Ok(PreparedBatch { destination, bytes })
}

/// Validate, encode, register with the completion waiter, and hand off one
/// acknowledged batch without waiting for the PTY. From registration on, the
/// waiter owns the reply and the Terminal's reservation.
#[allow(
    clippy::too_many_arguments,
    reason = "the atomic batch path keeps validation, handoff, and registration in one ordered transaction"
)]
pub(super) fn process_apply_input(
    state: &SharedState,
    encoders: &mut std::collections::HashMap<phux_core::ids::ResourceId, LaneEncoderSet>,
    cache: &SharedOperationCache,
    waiter: &CompletionWaiterHandle,
    ticket: CompletionTicket,
    completion_timeout: std::time::Duration,
    client_id: ClientId,
    operation_id: InputOperationId,
    terminal_id: &phux_protocol::ids::ResourceId,
    events: Vec<InputEvent>,
    reservation: AcknowledgedReservation,
    reply: oneshot::Sender<CommandResult>,
    lane_ahead: Option<LaneAhead>,
) -> bool {
    let prepared = match prepare_acknowledged_batch(state, encoders, client_id, terminal_id, events)
    {
        Ok(prepared) => prepared,
        Err(result) => {
            // A pre-handoff refusal leaves the id retryable (SPEC L1 6.2.1).
            drop(reservation);
            cache.set_retryable(operation_id, &result);
            let _ = reply.send(result);
            return false;
        }
    };

    let deadline = deadline_from(std::time::Instant::now(), completion_timeout);
    if let Err(pending) = waiter.register(PendingCompletion::new(
        ticket,
        operation_id,
        deadline,
        reservation,
        reply,
    )) {
        pending.resolve_without_writer(
            cache,
            acknowledged_not_written("acknowledged completion waiter unavailable for APPLY_INPUT"),
        );
        return false;
    }

    // The waiter now owns the answer. Abandon the ticket *before* dropping a
    // live request: its unfired sink reports `Failed` on drop, and the first
    // message wins.
    let handoff = with_route_input_destination(state, client_id, terminal_id, |current| {
        if !prepared
            .destination
            .handle
            .input_snapshot
            .same_channel(&current.handle.input_snapshot)
        {
            // The actor was replaced since admission; nothing was sent.
            return Err(acknowledged_not_written(
                "pane actor changed before APPLY_INPUT handoff",
            ));
        }
        // No credit, no room: refused before handoff, so retryable.
        let Some(credit) = credit::credit_for(&current.handle, None) else {
            return Err(acknowledged_resource_exhausted(
                "pane actor input mailbox is full",
            ));
        };
        let request = EncodedInputRequest::acknowledged(prepared.bytes, waiter.sink(ticket))
            .with_credit(credit)
            .with_lane_ahead(lane_ahead);
        match current.handle.encoded_input.try_send(request) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(request)) => {
                let refusal = acknowledged_resource_exhausted("pane actor input mailbox is full");
                waiter.abandon(ticket, refusal.clone());
                drop(request);
                Err(refusal)
            }
            // The actor is gone: provably nothing was written.
            Err(mpsc::error::TrySendError::Closed(request)) => {
                let refusal = acknowledged_not_written("pane actor unavailable for APPLY_INPUT");
                waiter.abandon(ticket, refusal.clone());
                drop(request);
                Err(refusal)
            }
        }
    });
    match handoff {
        Ok(Ok(())) => true,
        Ok(Err(result)) | Err(result) => {
            waiter.abandon(ticket, result);
            false
        }
    }
}
