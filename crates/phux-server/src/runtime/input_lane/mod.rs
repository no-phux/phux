//! Dedicated input lane (ADR-0044).
//!
//! The server's one current-thread runtime also runs every pane actor, so a
//! large output broadcast and a keystroke compete for one core. This lane
//! moves input routing and encoding onto its own OS thread: pane actors
//! publish a `Send` [`InputEncoderSnapshot`] after each terminal mutation,
//! the lane owns one encoder set per pane generation, and hands encoded
//! bytes to the actor's bounded mailbox with `try_send`. Every handoff
//! spends a pane input credit (ADR-0144, the `credit` submodule), so that
//! `try_send` has room: senders wait for credits, the lane never does.
//!
//! Ordering and authority:
//!
//! * A client's `INPUT_*`, `ROUTE_INPUT`, and `APPLY_INPUT` share one FIFO,
//!   so they reach a pane in wire order.
//! * The subscription and input-lease gates (ADR-0033) are re-checked under
//!   the state lock at delivery time. `ACQUIRE_INPUT` / `RELEASE_INPUT` still
//!   run inline, so a key racing its sender's own release is delivered or
//!   dropped; either is a legal fire-and-forget outcome (SPEC §12.2), and
//!   cross-client exclusion never weakens.
//! * `APPLY_INPUT` owes its caller a PTY verdict, but the lane never waits
//!   for it: see the `acknowledged` submodule.
//! * Only local ids reach the lane; satellite ids are hub forwards and stay
//!   on the main thread.

mod acknowledged;
mod credit;

use phux_protocol::ids::InputOperationId;
use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::{CommandResult, ErrorCode};
use phux_protocol::{MAX_APPLY_INPUT_COMMAND_BODY, MAX_APPLY_INPUT_EVENTS};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::{mpsc, oneshot};

pub(crate) use self::acknowledged::AcknowledgedReservation;
use self::acknowledged::{
    ACKNOWLEDGED_COMPLETION_TIMEOUT, AcknowledgedAdmission, CacheClaim, CompletionTicket,
    CompletionWaiter, CompletionWaiterHandle, PendingCompletion, SharedOperationCache,
    TicketSource, deadline_from, delivery_unknown, operation_digest,
};
pub(crate) use self::credit::{InputCredits, InputStalled, acquire_credit};
use super::{
    terminal_input_from_event, with_attached_input_destination, with_route_input_destination,
};
use crate::input::{
    InputEncoderSnapshot, PasteOutcome, PerTerminalFocusEncoder, PerTerminalKeyEncoder,
    PerTerminalMouseEncoder, PerTerminalPasteEncoder,
};
use crate::state::{ClientId, SharedState, TerminalInput};
use crate::terminal_actor::{EncodedInputRequest, InputCredit, TerminalHandle};

/// Bound on the lane's inbound queue. Attached input waits for room
/// ([`InputLaneHandle::route`]); a command refuses with `RESOURCE_EXHAUSTED`.
const INPUT_LANE_CAPACITY: usize = 1024;

/// One local input operation lifted off the main runtime.
#[derive(Debug)]
pub(crate) struct RoutedInput {
    /// Originating client, for the subscription and lease gates.
    pub(crate) client_id: ClientId,
    /// Wire pane id; always local.
    pub(crate) terminal_id: phux_protocol::ids::ResourceId,
    /// The authority policy and reply behavior for this wire surface.
    pub(crate) kind: RoutedInputKind,
    /// The pane input credit the sender took (ADR-0144); `None` makes the
    /// lane take one itself, without waiting.
    pub(crate) credit: Option<InputCredit>,
}

#[derive(Debug)]
pub(crate) enum RoutedInputKind {
    /// Attached data-plane input: enforce subscription plus lease authority.
    Attached {
        input: TerminalInput,
        frame_label: &'static str,
    },
    /// Attach-free control-plane input: enforce the lease and return its
    /// correlated command result after routing.
    Headless {
        event: InputEvent,
        reply: oneshot::Sender<CommandResult>,
    },
    /// Atomic acknowledged input batch.
    Acknowledged {
        operation_id: InputOperationId,
        events: Vec<InputEvent>,
        reservation: AcknowledgedReservation,
        reply: oneshot::Sender<CommandResult>,
    },
}

impl RoutedInput {
    pub(crate) const fn attached(
        client_id: ClientId,
        terminal_id: phux_protocol::ids::ResourceId,
        input: TerminalInput,
        frame_label: &'static str,
    ) -> Self {
        Self {
            client_id,
            terminal_id,
            kind: RoutedInputKind::Attached { input, frame_label },
            credit: None,
        }
    }

    /// This input, spending `credit` at its handoff.
    #[must_use]
    pub(crate) fn with_credit(mut self, credit: Option<InputCredit>) -> Self {
        self.credit = credit;
        self
    }
}

/// Cloneable handle a client task uses to hand input to the lane. Cloning is
/// cheap (`mpsc::Sender` clone); every clone keeps the lane thread alive.
#[derive(Clone, Debug)]
pub(crate) struct InputLaneHandle {
    tx: mpsc::Sender<RoutedInput>,
    admission: Arc<AcknowledgedAdmission>,
    cache: SharedOperationCache,
}

/// Owned completion of an input command whose lane admission already ran.
#[derive(Debug)]
pub(crate) struct InputReceipt {
    state: ReceiptState,
}

#[derive(Debug)]
enum ReceiptState {
    Ready(Option<CommandResult>),
    Waiting {
        result: oneshot::Receiver<CommandResult>,
        failure: ReceiptFailure,
    },
}

#[derive(Debug)]
enum ReceiptFailure {
    Internal(&'static str),
    DeliveryUnknown,
    DeliveryUnknownAndCache {
        cache: SharedOperationCache,
        operation_id: InputOperationId,
    },
}

impl Future for InputReceipt {
    type Output = CommandResult;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &mut self.state {
            ReceiptState::Ready(result) => {
                Poll::Ready(result.take().unwrap_or_else(|| CommandResult::Error {
                    code: ErrorCode::InternalError,
                    message: "input receipt polled after completion".to_owned(),
                }))
            }
            ReceiptState::Waiting { result, failure } => match Pin::new(result).poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Ok(result)) => Poll::Ready(result),
                Poll::Ready(Err(_)) => Poll::Ready(failure.result()),
            },
        }
    }
}

impl ReceiptFailure {
    fn result(&self) -> CommandResult {
        match self {
            Self::Internal(message) => CommandResult::Error {
                code: ErrorCode::InternalError,
                message: (*message).to_owned(),
            },
            Self::DeliveryUnknown => delivery_unknown(),
            Self::DeliveryUnknownAndCache {
                cache,
                operation_id,
            } => {
                let result = delivery_unknown();
                cache.set_final(*operation_id, &result);
                result
            }
        }
    }
}

impl InputLaneHandle {
    /// Enqueue an input event for off-thread routing, waiting while the
    /// lane's queue is full (ADR-0144). The lane never blocks, so the wait
    /// is short and holds no lock. A closed lane (thread gone during
    /// shutdown) drops at `debug!`.
    pub(crate) async fn route(&self, routed: RoutedInput) {
        if let Err(mpsc::error::SendError(routed)) = self.tx.send(routed).await {
            tracing::debug!(
                client_id = ?routed.client_id,
                frame_label = routed.kind.frame_label(),
                "input lane closed; dropping input",
            );
            routed.kind.reply_dropped(CommandResult::Error {
                code: ErrorCode::InternalError,
                message: "input lane unavailable for ROUTE_INPUT".to_owned(),
            });
        }
    }

    /// Synchronously admit attach-free command input to the same FIFO as
    /// `INPUT_*`, returning an owned lease/mailbox completion. `credit` is
    /// the pane input credit the caller waited for, if any.
    pub(crate) fn begin_route(
        &self,
        client_id: ClientId,
        terminal_id: phux_protocol::ids::ResourceId,
        event: InputEvent,
        credit: Option<InputCredit>,
    ) -> InputReceipt {
        let (reply, result) = oneshot::channel();
        let routed = RoutedInput {
            client_id,
            terminal_id,
            kind: RoutedInputKind::Headless { event, reply },
            credit,
        };
        let failure = match self.tx.try_send(routed) {
            Ok(()) => None,
            Err(mpsc::error::TrySendError::Full(_)) => Some(CommandResult::Error {
                code: ErrorCode::ResourceExhausted,
                message: "input lane queue is full for ROUTE_INPUT".to_owned(),
            }),
            Err(mpsc::error::TrySendError::Closed(_)) => Some(CommandResult::Error {
                code: ErrorCode::InternalError,
                message: "input lane unavailable for ROUTE_INPUT".to_owned(),
            }),
        };
        if let Some(failure) = failure {
            return ready_receipt(failure);
        }
        waiting_receipt(
            result,
            ReceiptFailure::Internal("input lane stopped before ROUTE_INPUT completed"),
        )
    }

    /// Synchronously run dedupe/admission and enqueue an idempotent atomic
    /// batch, returning an owned PTY write/flush completion.
    pub(crate) fn begin_apply(
        &self,
        client_id: ClientId,
        operation_id: InputOperationId,
        terminal_id: phux_protocol::ids::ResourceId,
        events: Vec<InputEvent>,
    ) -> InputReceipt {
        // A satellite target is the hub's to forward (`handle_command` routes
        // it before the lane, L1 §9.1). One that reaches the lane anyway is
        // refused before admission, so the refusal binds no id.
        if terminal_id.local_id().is_none() {
            return ready_receipt(unsupported_satellite_route());
        }
        let (digest, events) = operation_digest(operation_id, &terminal_id, events);
        match self
            .cache
            .claim_at(operation_id, digest, std::time::Instant::now())
        {
            CacheClaim::Final(result) => return ready_receipt(result),
            CacheClaim::Conflict => {
                return ready_receipt(invalid_command(
                    "APPLY_INPUT operation id reused with a different payload",
                ));
            }
            CacheClaim::Full => {
                return ready_receipt(acknowledged_resource_exhausted(
                    "acknowledged-input dedupe cache is full",
                ));
            }
            CacheClaim::Pending(result) => {
                return waiting_receipt(result, ReceiptFailure::DeliveryUnknown);
            }
            CacheClaim::PendingUncertain => return ready_receipt(delivery_unknown()),
            CacheClaim::Owner => {}
        }
        let Some(reservation) = AcknowledgedReservation::try_acquire(&self.admission, &terminal_id)
        else {
            let result = acknowledged_resource_exhausted(
                "another APPLY_INPUT operation is in flight for this terminal",
            );
            self.cache.set_retryable(operation_id, &result);
            return ready_receipt(result);
        };
        let (reply, result) = oneshot::channel();
        let routed = RoutedInput {
            client_id,
            terminal_id,
            kind: RoutedInputKind::Acknowledged {
                operation_id,
                events,
                reservation,
                reply,
            },
            credit: None,
        };
        match self.tx.try_send(routed) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(routed)) => {
                drop(routed);
                let result = acknowledged_resource_exhausted("input lane queue is full");
                self.cache.set_retryable(operation_id, &result);
                return ready_receipt(result);
            }
            // The lane thread is gone: provably nothing was written.
            Err(mpsc::error::TrySendError::Closed(routed)) => {
                drop(routed);
                let result = acknowledged_not_written("input lane unavailable for APPLY_INPUT");
                self.cache.set_retryable(operation_id, &result);
                return ready_receipt(result);
            }
        }
        let cache = self.cache.clone();
        waiting_receipt(
            result,
            ReceiptFailure::DeliveryUnknownAndCache {
                cache,
                operation_id,
            },
        )
    }
}

const fn ready_receipt(result: CommandResult) -> InputReceipt {
    InputReceipt {
        state: ReceiptState::Ready(Some(result)),
    }
}

const fn waiting_receipt(
    result: oneshot::Receiver<CommandResult>,
    failure: ReceiptFailure,
) -> InputReceipt {
    InputReceipt {
        state: ReceiptState::Waiting { result, failure },
    }
}

impl RoutedInputKind {
    const fn frame_label(&self) -> &'static str {
        match self {
            Self::Attached { frame_label, .. } => frame_label,
            Self::Headless { .. } => "ROUTE_INPUT",
            Self::Acknowledged { .. } => "APPLY_INPUT",
        }
    }

    fn reply_dropped(self, result: CommandResult) {
        match self {
            Self::Headless { reply, .. } | Self::Acknowledged { reply, .. } => {
                let _ = reply.send(result);
            }
            Self::Attached { .. } => {}
        }
    }
}

/// Owns the lane's OS thread for the server's lifetime; dropping it joins the
/// thread once every handle clone is gone.
#[derive(Debug)]
pub(crate) struct InputLane {
    handle: InputLaneHandle,
    join: Option<std::thread::JoinHandle<()>>,
    /// Dropped after the lane thread is joined, so no registration can race
    /// the waiter's shutdown.
    #[allow(dead_code, reason = "held for its Drop: stops and joins the waiter")]
    waiter: CompletionWaiter,
}

impl InputLane {
    /// A cloneable routing handle for client tasks.
    pub(crate) fn handle(&self) -> InputLaneHandle {
        self.handle.clone()
    }
}

impl Drop for InputLane {
    fn drop(&mut self) {
        // Swap in a dead sender so the channel closes once every client
        // clone is gone, then join. Callers must drop every handle clone
        // first (the runtime drops its `LocalSet` before the lane), or this
        // join hangs.
        let (dead_tx, _dead_rx) = mpsc::channel(1);
        self.handle.tx = dead_tx;
        if let Some(join) = self.join.take()
            && let Err(err) = join.join()
        {
            tracing::warn!(?err, "input lane thread panicked on shutdown");
        }
    }
}

/// One pane generation's encoders, fed by its actor's published snapshot.
#[derive(Debug)]
struct LaneEncoderSet {
    key: PerTerminalKeyEncoder,
    mouse: PerTerminalMouseEncoder,
    focus: PerTerminalFocusEncoder,
    paste: PerTerminalPasteEncoder,
    snapshot: tokio::sync::watch::Receiver<InputEncoderSnapshot>,
}

impl LaneEncoderSet {
    fn new(handle: &TerminalHandle) -> Result<Self, libghostty_vt::Error> {
        Ok(Self {
            key: PerTerminalKeyEncoder::new()?,
            mouse: PerTerminalMouseEncoder::new()?,
            focus: PerTerminalFocusEncoder::new(),
            paste: PerTerminalPasteEncoder::new(),
            snapshot: handle.input_snapshot.clone(),
        })
    }

    fn encode(&mut self, input: &TerminalInput) -> Result<Option<Vec<u8>>, libghostty_vt::Error> {
        let snapshot = *self.snapshot.borrow();
        self.encode_with_snapshot(input, snapshot)
    }

    fn encode_with_snapshot(
        &mut self,
        input: &TerminalInput,
        snapshot: InputEncoderSnapshot,
    ) -> Result<Option<Vec<u8>>, libghostty_vt::Error> {
        match input {
            TerminalInput::Key(event) => Ok(Some(
                self.key.encode_with_options(event, snapshot.key)?.to_vec(),
            )),
            TerminalInput::Mouse(event) => Ok(Some(
                self.mouse
                    .encode_with_options(
                        event,
                        snapshot.mouse,
                        snapshot.cols,
                        snapshot.rows,
                        snapshot.cell_px,
                    )?
                    .to_vec(),
            )),
            TerminalInput::Focus(event) => Ok(self
                .focus
                .encode_with_mode(*event, snapshot.focus_reporting)?
                .map(<[u8]>::to_vec)),
            TerminalInput::Paste(event) => match self
                .paste
                .encode_with_mode(event, snapshot.bracketed_paste)?
            {
                PasteOutcome::Encoded(bytes) => Ok(Some(bytes.to_vec())),
                PasteOutcome::Rejected => Ok(None),
            },
        }
    }
}

fn prune_closed_encoders(
    encoders: &mut std::collections::HashMap<phux_core::ids::ResourceId, LaneEncoderSet>,
) {
    encoders.retain(|_, encoder| encoder.snapshot.has_changed().is_ok());
}

fn encoder_for<'a>(
    encoders: &'a mut std::collections::HashMap<phux_core::ids::ResourceId, LaneEncoderSet>,
    pane: phux_core::ids::ResourceId,
    handle: &TerminalHandle,
) -> Result<&'a mut LaneEncoderSet, libghostty_vt::Error> {
    let replace = encoders
        .get(&pane)
        .is_some_and(|encoder| !encoder.snapshot.same_channel(&handle.input_snapshot));
    if replace {
        encoders.remove(&pane);
    }
    match encoders.entry(pane) {
        std::collections::hash_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
        std::collections::hash_map::Entry::Vacant(entry) => {
            Ok(entry.insert(LaneEncoderSet::new(handle)?))
        }
    }
}

fn encode_input(
    encoders: &mut std::collections::HashMap<phux_core::ids::ResourceId, LaneEncoderSet>,
    pane: phux_core::ids::ResourceId,
    handle: &TerminalHandle,
    input: &TerminalInput,
) -> Option<Vec<u8>> {
    match encoder_for(encoders, pane, handle).and_then(|encoder| encoder.encode(input)) {
        Ok(Some(bytes)) if !bytes.is_empty() => Some(bytes),
        Ok(_) => None,
        Err(err) => {
            tracing::warn!(error = %err, ?pane, "input lane encode failed; dropping event");
            None
        }
    }
}

/// The refusal for input that found its pane saturated at the handoff.
fn input_saturated() -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::ResourceExhausted,
        message: "terminal input is saturated; nothing was written".to_owned(),
    }
}

fn handoff_encoded(
    pane: phux_core::ids::ResourceId,
    handle: &TerminalHandle,
    bytes: Option<Vec<u8>>,
    echo_probe: bool,
    credit: Option<InputCredit>,
) -> Result<bool, CommandResult> {
    let Some(bytes) = bytes else {
        return Ok(true);
    };
    // A sender that waited brings its credit; only input that raced a pane
    // replacement, or came from a sender that did not wait, can find none.
    let Some(credit) = credit::credit_for(handle, credit) else {
        tracing::warn!(?pane, "pane input credits exhausted; refusing input");
        return Err(input_saturated());
    };
    let request = EncodedInputRequest::legacy_probe(bytes, echo_probe).with_credit(credit);
    match handle.encoded_input.try_send(request) {
        Ok(()) => Ok(true),
        // Unreachable while every sender holds a credit (ADR-0144).
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::error!(
                ?pane,
                "credited input found the pane mailbox full; refusing"
            );
            Err(input_saturated())
        }
        Err(mpsc::error::TrySendError::Closed(_)) => Err(CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor unavailable for ROUTE_INPUT".to_owned(),
        }),
    }
}

/// Hand `bytes` to the destination the gate just re-resolved, unless its
/// actor was replaced since `encoded_for` (the bytes belong to the old one).
fn redeliver(
    encoded_for: &TerminalHandle,
    current: &super::InputDestination,
    bytes: Option<Vec<u8>>,
    input: &TerminalInput,
    credit: Option<InputCredit>,
) -> Result<bool, CommandResult> {
    if !encoded_for
        .input_snapshot
        .same_channel(&current.handle.input_snapshot)
    {
        return Ok(false);
    }
    handoff_encoded(
        current.pane,
        &current.handle,
        bytes,
        crate::terminal_actor::echo_probe_for(input),
        credit,
    )
}

fn process_attached(
    state: &SharedState,
    encoders: &mut std::collections::HashMap<phux_core::ids::ResourceId, LaneEncoderSet>,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    input: &TerminalInput,
    frame_label: &'static str,
    credit: Option<InputCredit>,
) {
    let is_focus_gained = matches!(
        input,
        TerminalInput::Focus(phux_protocol::input::focus::FocusEvent::Gained)
    );
    let Some(destination) = with_attached_input_destination(
        state,
        client_id,
        terminal_id,
        frame_label,
        std::convert::identity,
    ) else {
        return;
    };
    let bytes = encode_input(encoders, destination.pane, &destination.handle, input);
    // Re-gate and send under one state lock, so a lease change cannot slip
    // between gate and delivery while encoding happens off-lock.
    let delivered =
        with_attached_input_destination(state, client_id, terminal_id, frame_label, |current| {
            redeliver(&destination.handle, &current, bytes, input, credit)
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
}

/// `INPUT_*` frames have no reply, so a refusal after the gates is pushed as
/// an uncorrelated `ERROR` (input.md §5.1): input is never lost silently.
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

fn process_headless(
    state: &SharedState,
    encoders: &mut std::collections::HashMap<phux_core::ids::ResourceId, LaneEncoderSet>,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    event: InputEvent,
    credit: Option<InputCredit>,
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
        redeliver(&destination.handle, &current, bytes, &input, credit)
    }) {
        Ok(Ok(_)) => CommandResult::Ok,
        Ok(Err(result)) | Err(result) => result,
    }
}

fn unsupported_satellite_route() -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::UnsupportedSatelliteRoute,
        message: "APPLY_INPUT to a satellite is forwarded by a federation hub, not run here"
            .to_owned(),
    }
}

fn invalid_command(message: &str) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::InvalidCommand,
        message: message.to_owned(),
    }
}

fn acknowledged_resource_exhausted(message: &str) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::ResourceExhausted,
        message: message.to_owned(),
    }
}

/// An `APPLY_INPUT` refused or abandoned before it reached any pane actor's
/// mailbox: provably nothing was written, so any retry is safe.
fn acknowledged_not_written(message: &str) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::InputNotWritten,
        message: message.to_owned(),
    }
}

/// An acknowledged batch that passed every pre-handoff gate: authority, dedupe
/// binding, paste safety, and encoding.
struct PreparedBatch {
    destination: super::InputDestination,
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
fn process_apply_input(
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
) {
    let prepared = match prepare_acknowledged_batch(state, encoders, client_id, terminal_id, events)
    {
        Ok(prepared) => prepared,
        Err(result) => {
            // A pre-handoff refusal leaves the id retryable (SPEC L1 6.2.1).
            drop(reservation);
            cache.set_retryable(operation_id, &result);
            let _ = reply.send(result);
            return;
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
        return;
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
            .with_credit(credit);
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
        Ok(Ok(())) => {}
        Ok(Err(result)) | Err(result) => waiter.abandon(ticket, result),
    }
}

/// Spawn the input-lane thread. It exits once every sender is dropped.
///
/// # Errors
///
/// The OS error if the thread cannot be spawned.
pub(crate) fn spawn_input_lane(state: SharedState) -> std::io::Result<InputLane> {
    spawn_input_lane_with_completion_timeout(state, ACKNOWLEDGED_COMPLETION_TIMEOUT)
}

fn spawn_input_lane_with_completion_timeout(
    state: SharedState,
    completion_timeout: std::time::Duration,
) -> std::io::Result<InputLane> {
    let (tx, mut rx) = mpsc::channel::<RoutedInput>(INPUT_LANE_CAPACITY);
    let admission = Arc::new(AcknowledgedAdmission::default());
    // The server's one dedupe record (ADR-0126).
    let cache = SharedOperationCache::new(state.with(|s| s.operation_dedupe().clone()));
    let lane_cache = cache.clone();
    let waiter = CompletionWaiter::spawn(cache.clone())?;
    let waiter_handle = waiter.handle();
    let join = std::thread::Builder::new()
        .name("phux-input-lane".to_owned())
        .spawn(move || {
            crate::perf::promote_helper_thread("phux-input-lane");
            let mut encoders = std::collections::HashMap::new();
            let mut tickets = TicketSource::default();
            // Everything below is synchronous and non-blocking, so no pane
            // can stall another pane's input.
            while let Some(routed) = rx.blocking_recv() {
                prune_closed_encoders(&mut encoders);
                match routed.kind {
                    RoutedInputKind::Attached { input, frame_label } => process_attached(
                        &state,
                        &mut encoders,
                        routed.client_id,
                        &routed.terminal_id,
                        &input,
                        frame_label,
                        routed.credit,
                    ),
                    RoutedInputKind::Headless { event, reply } => {
                        let result = process_headless(
                            &state,
                            &mut encoders,
                            routed.client_id,
                            &routed.terminal_id,
                            event,
                            routed.credit,
                        );
                        let _ = reply.send(result);
                    }
                    RoutedInputKind::Acknowledged {
                        operation_id,
                        events,
                        reservation,
                        reply,
                    } => process_apply_input(
                        &state,
                        &mut encoders,
                        &lane_cache,
                        &waiter_handle,
                        tickets.next_ticket(),
                        completion_timeout,
                        routed.client_id,
                        operation_id,
                        &routed.terminal_id,
                        events,
                        reservation,
                        reply,
                    ),
                }
            }
            tracing::debug!("input lane thread exiting (channel closed)");
        })?;
    Ok(InputLane {
        handle: InputLaneHandle {
            tx,
            admission,
            cache,
        },
        join: Some(join),
        waiter,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::FutureExt as _;
    use phux_protocol::input::paste::{PasteEvent, PasteTrust};
    use tokio::task::LocalSet;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::terminal_actor::{
        DEFAULT_INPUT_MAILBOX, INPUT_CREDITS, InputCreditPool, TerminalActor, WriteCompletion,
    };

    /// Bound for "this work was handed off" waits. Never the assertion
    /// itself; generous so a loaded machine does not fail a correct lane.
    const LANE_DELIVERY_DEADLINE: Duration = Duration::from_secs(30);

    /// "Nothing else arrives" window: expiry is the pass.
    const NOTHING_FURTHER_WINDOW: Duration = Duration::from_millis(100);

    fn paste_event(bytes: &[u8]) -> PasteEvent {
        PasteEvent {
            trust: PasteTrust::Trusted,
            data: bytes.to_vec(),
        }
    }

    /// A trusted paste; with DEC 2004 off it encodes to exactly `bytes`.
    fn paste(bytes: &[u8]) -> TerminalInput {
        TerminalInput::Paste(paste_event(bytes))
    }

    fn ev(bytes: &[u8]) -> InputEvent {
        InputEvent::Paste(paste_event(bytes))
    }

    fn operation_id(byte: u8) -> InputOperationId {
        InputOperationId::new([byte; 16]).expect("non-zero operation id")
    }

    fn code(result: &CommandResult) -> Option<ErrorCode> {
        match result {
            CommandResult::Error { code, .. } => Some(*code),
            _ => None,
        }
    }

    async fn next_write(rx: &mut mpsc::Receiver<EncodedInputRequest>) -> EncodedInputRequest {
        tokio::time::timeout(LANE_DELIVERY_DEADLINE, rx.recv())
            .await
            .expect("input reaches the PTY writer")
            .expect("writer channel open")
    }

    fn complete(request: EncodedInputRequest, outcome: WriteCompletion) {
        request
            .completion
            .expect("acknowledged completion")
            .complete(outcome);
    }

    async fn assert_no_write(rx: &mut mpsc::Receiver<EncodedInputRequest>) {
        assert!(
            tokio::time::timeout(NOTHING_FURTHER_WINDOW, rx.recv())
                .await
                .is_err(),
            "nothing further may reach the PTY writer"
        );
    }

    /// A live pane actor with test PTY channels, registered in its own
    /// session. Must run inside a `LocalSet`.
    struct Pane {
        wire: phux_protocol::ids::ResourceId,
        core: phux_core::ids::ResourceId,
        credits: InputCreditPool,
        writer_rx: mpsc::Receiver<EncodedInputRequest>,
        token: CancellationToken,
    }

    fn spawn_pane(state: &SharedState, session: &str, seed: &[u8]) -> Pane {
        let bundle = TerminalActor::new_with_seed(80, 24, seed).expect("actor");
        let handle = bundle.handle.clone();
        let credits = handle.terminal().expect("facet").input_credits.clone();
        let token = bundle.token.clone();
        let mut actor = bundle.actor;
        let (_pty_evt_tx, writer_rx) = actor.install_test_pty_channels();
        let (wire, core) = state.with_mut(|s| {
            let (_sid, _wid, core) = s.seed_session(session);
            (
                s.register_resource_handle(core, handle, token.clone()),
                core,
            )
        });
        tokio::task::spawn_local(actor.run());
        Pane {
            wire,
            core,
            credits,
            writer_rx,
            token,
        }
    }

    /// A pane plus two attached clients and a lane.
    struct Fixture {
        state: SharedState,
        pane: Pane,
        client_a: ClientId,
        client_b: ClientId,
        lane: InputLane,
    }

    fn fixture_with_seed(seed: &[u8]) -> Fixture {
        let state = SharedState::new();
        let pane = spawn_pane(&state, "s", seed);
        let (client_a, client_b) = state.with_mut(|s| {
            let a = s.new_client_id();
            let b = s.new_client_id();
            s.attach_default_caps(a, "s", mpsc::channel(16).0)
                .expect("attach a");
            s.attach_default_caps(b, "s", mpsc::channel(16).0)
                .expect("attach b");
            (a, b)
        });
        let lane = spawn_input_lane(state.clone()).expect("spawn lane");
        Fixture {
            state,
            pane,
            client_a,
            client_b,
            lane,
        }
    }

    fn fixture() -> Fixture {
        fixture_with_seed(b"")
    }

    impl Fixture {
        fn handle(&self) -> InputLaneHandle {
            self.lane.handle()
        }

        fn attached(&self, client: ClientId, bytes: &[u8]) {
            self.handle()
                .route(RoutedInput::attached(
                    client,
                    self.pane.wire.clone(),
                    paste(bytes),
                    "INPUT_PASTE",
                ))
                .now_or_never()
                .expect("the lane queue has room");
        }

        /// Client A's `APPLY_INPUT` of one paste per payload to this pane.
        fn apply(&self, id: u8, payloads: &[&[u8]]) -> InputReceipt {
            let events = payloads.iter().map(|bytes| ev(bytes)).collect();
            self.handle().begin_apply(
                self.client_a,
                operation_id(id),
                self.pane.wire.clone(),
                events,
            )
        }

        fn route(&self, client: ClientId, bytes: &[u8]) -> InputReceipt {
            self.handle()
                .begin_route(client, self.pane.wire.clone(), ev(bytes), None)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.pane.token.cancel();
        }
    }

    /// `INPUT_*` and `ROUTE_INPUT` share one lane FIFO, so a mixed stream
    /// reaches the PTY writer in the order routed.
    #[tokio::test(flavor = "current_thread")]
    async fn attached_and_route_input_share_one_fifo() {
        LocalSet::new()
            .run_until(async {
                let mut fx = fixture();
                fx.attached(fx.client_a, b"a");
                let routed = fx.route(fx.client_a, b"b");
                fx.attached(fx.client_a, b"c");
                assert_eq!(routed.await, CommandResult::Ok);
                for expected in [&b"a"[..], b"b", b"c"] {
                    assert_eq!(next_write(&mut fx.pane.writer_rx).await.bytes, expected);
                }
            })
            .await;
    }

    /// Lease exclusion (ADR-0033) holds on the lane: with B holding the
    /// wheel, A's attached input is dropped and A's `ROUTE_INPUT` keeps its
    /// typed lease error, while B's input is delivered.
    #[tokio::test(flavor = "current_thread")]
    async fn lane_honors_input_lease() {
        LocalSet::new()
            .run_until(async {
                let mut fx = fixture();
                fx.state
                    .with_mut(|s| s.set_input_lease(fx.pane.core, fx.client_b));
                fx.attached(fx.client_a, b"a");
                let refused = fx.route(fx.client_a, b"r").await;
                assert_eq!(code(&refused), Some(ErrorCode::InputLeaseHeld));
                fx.attached(fx.client_b, b"B");
                assert_eq!(
                    next_write(&mut fx.pane.writer_rx).await.bytes.as_ref(),
                    b"B"
                );
                assert_no_write(&mut fx.pane.writer_rx).await;
            })
            .await;
    }

    /// `ROUTE_INPUT` completes while the runtime thread is synchronously
    /// busy: only the dedicated lane thread can have routed it.
    #[tokio::test(flavor = "current_thread")]
    async fn route_input_routes_while_main_runtime_is_not_polling() {
        LocalSet::new()
            .run_until(async {
                let fx = fixture();
                // Enqueue under the state lock so the lane cannot finish
                // before this task stops polling.
                let mut receipt = fx.state.with(|_| {
                    let mut receipt = fx.route(fx.client_a, b"k");
                    assert!((&mut receipt).now_or_never().is_none());
                    receipt
                });
                let deadline = std::time::Instant::now() + LANE_DELIVERY_DEADLINE;
                let routed = loop {
                    if let Some(result) = (&mut receipt).now_or_never() {
                        break result;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "lane did not route while the runtime was blocked"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                };
                assert_eq!(routed, CommandResult::Ok);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lane_encodes_from_published_bracketed_paste_snapshot() {
        LocalSet::new()
            .run_until(async {
                let mut fx = fixture_with_seed(b"\x1b[?2004h");
                fx.attached(fx.client_a, b"lane");
                assert_eq!(
                    next_write(&mut fx.pane.writer_rx).await.bytes.as_ref(),
                    b"\x1b[200~lane\x1b[201~"
                );
            })
            .await;
    }

    /// One encoder set per pane generation: a respawned actor replaces it,
    /// and a closed actor's set is pruned.
    #[test]
    fn encoder_cache_tracks_actor_generations() {
        let first = TerminalActor::new(80, 24).expect("first actor");
        let second = TerminalActor::new(80, 24).expect("second actor");
        let state = SharedState::new();
        let pane = state.with_mut(|s| s.seed_session("s").2);
        let mut encoders = std::collections::HashMap::new();
        let facet = |bundle: &crate::terminal_actor::TerminalActorBundle| {
            bundle.handle.terminal().expect("facet").clone()
        };
        encoder_for(&mut encoders, pane, &facet(&first)).expect("first encoder");
        encoder_for(&mut encoders, pane, &facet(&second)).expect("replacement");
        assert_eq!(encoders.len(), 1);
        assert!(
            encoders[&pane]
                .snapshot
                .same_channel(&facet(&second).input_snapshot)
        );
        drop(second);
        prune_closed_encoders(&mut encoders);
        assert!(encoders.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_input_writes_one_atomic_batch_then_acks_and_deduplicates() {
        LocalSet::new()
            .run_until(async {
                let mut fx = fixture();
                let first = fx.apply(1, &[b"one", b"two"]);
                let request = next_write(&mut fx.pane.writer_rx).await;
                assert_eq!(request.bytes.as_ref(), b"onetwo");
                complete(request, WriteCompletion::Delivered);
                assert_eq!(first.await, CommandResult::Ok);
                assert_eq!(fx.apply(1, &[b"one", b"two"]).await, CommandResult::Ok);
                assert_no_write(&mut fx.pane.writer_rx).await;
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn same_id_retry_joins_the_unresolved_operation() {
        LocalSet::new()
            .run_until(async {
                let mut fx = fixture();
                let first = fx.apply(24, &[b"once"]);
                let request = next_write(&mut fx.pane.writer_rx).await;
                let mut retry = fx.apply(24, &[b"once"]);
                assert!(
                    tokio::time::timeout(NOTHING_FURTHER_WINDOW, &mut retry)
                        .await
                        .is_err(),
                    "the retry waits for the operation's outcome"
                );
                complete(request, WriteCompletion::Delivered);
                assert_eq!(first.await, CommandResult::Ok);
                assert_eq!(retry.await, CommandResult::Ok);
                assert_no_write(&mut fx.pane.writer_rx).await;
            })
            .await;
    }

    /// The dedupe lookup precedes per-Terminal admission.
    #[tokio::test(flavor = "current_thread")]
    async fn cached_operation_is_readable_while_another_id_holds_admission() {
        LocalSet::new()
            .run_until(async {
                let mut fx = fixture();
                let first = fx.apply(25, &[b"cached"]);
                complete(
                    next_write(&mut fx.pane.writer_rx).await,
                    WriteCompletion::Delivered,
                );
                assert_eq!(first.await, CommandResult::Ok);

                let active = fx.apply(26, &[b"active"]);
                let active_write = next_write(&mut fx.pane.writer_rx).await;
                assert_eq!(fx.apply(25, &[b"cached"]).await, CommandResult::Ok);
                complete(active_write, WriteCompletion::Delivered);
                assert_eq!(active.await, CommandResult::Ok);
                assert_no_write(&mut fx.pane.writer_rx).await;
            })
            .await;
    }

    /// A writer that fails, or drops the request, after the handoff leaves
    /// the delivery unknown; the retry replays that answer and never writes
    /// again.
    #[tokio::test(flavor = "current_thread")]
    async fn writer_failure_after_handoff_is_unknown_and_deduplicated() {
        LocalSet::new()
            .run_until(async {
                let mut fx = fixture();
                for (id, fail) in [(7_u8, true), (27, false)] {
                    let first = fx.apply(id, &[b"uncertain"]);
                    let request = next_write(&mut fx.pane.writer_rx).await;
                    if fail {
                        complete(request, WriteCompletion::Failed);
                    } else {
                        drop(request);
                    }
                    let result = first.await;
                    assert_eq!(code(&result), Some(ErrorCode::InputDeliveryUnknown));
                    assert_eq!(fx.apply(id, &[b"uncertain"]).await, result);
                    assert_no_write(&mut fx.pane.writer_rx).await;
                }
            })
            .await;
    }

    /// Every pre-handoff refusal happens before a write: a reused id with a
    /// new payload, an empty or oversized batch, an unsafe paste, encoded
    /// bytes past the body limit, and a satellite target, which also binds
    /// no id (L1 §9.1).
    #[tokio::test(flavor = "current_thread")]
    async fn apply_input_refuses_before_handoff() {
        LocalSet::new()
            .run_until(async {
                let mut fx = fixture();
                let first = fx.apply(2, &[b"original"]);
                complete(
                    next_write(&mut fx.pane.writer_rx).await,
                    WriteCompletion::Delivered,
                );
                assert_eq!(first.await, CommandResult::Ok);

                let handle = fx.handle();
                let wire = fx.pane.wire.clone();
                let unsafe_paste = InputEvent::Paste(PasteEvent {
                    trust: PasteTrust::Untrusted,
                    data: b"danger\n".to_vec(),
                });
                let too_many = std::iter::repeat_with(|| ev(b"x"))
                    .take(MAX_APPLY_INPUT_EVENTS + 1)
                    .collect();
                let oversized = vec![ev(&vec![b'x'; MAX_APPLY_INPUT_COMMAND_BODY + 1])];
                let satellite = phux_protocol::ResourceId::satellite("peer", 1);
                let cases = [
                    (
                        2,
                        wire.clone(),
                        vec![ev(b"different")],
                        ErrorCode::InvalidCommand,
                    ),
                    (3, wire.clone(), vec![], ErrorCode::InvalidCommand),
                    (4, wire.clone(), too_many, ErrorCode::InvalidCommand),
                    (
                        5,
                        wire.clone(),
                        vec![ev(b"prefix"), unsafe_paste],
                        ErrorCode::UnsafePaste,
                    ),
                    (13, wire.clone(), oversized, ErrorCode::InvalidCommand),
                    (
                        12,
                        satellite,
                        vec![ev(b"x")],
                        ErrorCode::UnsupportedSatelliteRoute,
                    ),
                ];
                for (id, target, events, want) in cases {
                    let result = handle
                        .begin_apply(fx.client_a, operation_id(id), target, events)
                        .await;
                    assert_eq!(code(&result), Some(want), "operation {id}");
                }
                assert!(
                    matches!(
                        handle.cache.claim_at(
                            operation_id(12),
                            [0xee; 32],
                            std::time::Instant::now()
                        ),
                        CacheClaim::Owner
                    ),
                    "a refused satellite batch leaves its id unbound"
                );
                assert_no_write(&mut fx.pane.writer_rx).await;
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_input_reports_mailbox_full_without_handoff() {
        let bundle = TerminalActor::new(80, 24).expect("actor");
        let facet = bundle.handle.terminal().expect("facet");
        for _ in 0..DEFAULT_INPUT_MAILBOX {
            facet
                .encoded_input
                .try_send(EncodedInputRequest::legacy(vec![b'x']))
                .expect("fill mailbox");
        }
        let state = SharedState::new();
        let (wire, client) = state.with_mut(|s| {
            let pane = s.seed_session("s").2;
            let wire =
                s.register_resource_handle(pane, bundle.handle.clone(), bundle.token.clone());
            (wire, s.new_client_id())
        });
        let lane = spawn_input_lane(state).expect("spawn lane");
        let result = lane
            .handle()
            .begin_apply(client, operation_id(6), wire, vec![ev(b"x")])
            .await;
        assert_eq!(code(&result), Some(ErrorCode::ResourceExhausted));
    }

    /// phux-w7z2.60: a batch that provably never reached a PTY writer is
    /// `INPUT_NOT_WRITTEN`, not unknown: a pane with no PTY, an actor gone
    /// before handoff, a closed writer channel.
    #[tokio::test(flavor = "current_thread")]
    async fn apply_input_reports_not_written_when_no_writer_can_receive() {
        LocalSet::new()
            .run_until(async {
                for case in ["no-pty", "actor-gone", "writer-closed"] {
                    let state = SharedState::new();
                    let (token, wire) = if case == "writer-closed" {
                        let pane = spawn_pane(&state, "s", b"");
                        drop(pane.writer_rx);
                        (pane.token, pane.wire)
                    } else {
                        let bundle = TerminalActor::new(80, 24).expect("actor");
                        let handle = bundle.handle.clone();
                        let token = bundle.token.clone();
                        if case == "no-pty" {
                            tokio::task::spawn_local(bundle.actor.run());
                        } else {
                            drop(bundle);
                        }
                        let wire = state.with_mut(|s| {
                            let pane = s.seed_session("s").2;
                            s.register_resource_handle(pane, handle, token.clone())
                        });
                        (token, wire)
                    };
                    let client = state.with_mut(crate::state::ServerState::new_client_id);
                    let lane = spawn_input_lane(state).expect("spawn lane");
                    let result = tokio::time::timeout(
                        LANE_DELIVERY_DEADLINE,
                        lane.handle()
                            .begin_apply(client, operation_id(8), wire, vec![ev(b"x")]),
                    )
                    .await
                    .expect("must not hang");
                    assert_eq!(code(&result), Some(ErrorCode::InputNotWritten), "{case}");
                    token.cancel();
                }
            })
            .await;
    }

    /// A pane with no free input credit (ADR-0144) refuses `APPLY_INPUT`
    /// before handoff, without waiting out the completion timeout and
    /// without growing the writer queue; the id stays retryable.
    #[tokio::test(flavor = "current_thread")]
    async fn apply_input_to_a_saturated_pane_is_refused_before_handoff() {
        LocalSet::new()
            .run_until(async {
                let mut fx = fixture();
                let mut held: Vec<_> = std::iter::from_fn(|| fx.pane.credits.try_take()).collect();
                assert_eq!(held.len(), INPUT_CREDITS);
                // The deadline is the assertion: strictly under the timeout.
                let result = tokio::time::timeout(
                    ACKNOWLEDGED_COMPLETION_TIMEOUT / 2,
                    fx.apply(18, &[b"first"]),
                )
                .await
                .expect("resolves without the completion timeout");
                assert_eq!(code(&result), Some(ErrorCode::ResourceExhausted));
                assert_no_write(&mut fx.pane.writer_rx).await;

                held.pop();
                let retry = fx.apply(18, &[b"first"]);
                let request = next_write(&mut fx.pane.writer_rx).await;
                assert_eq!(request.bytes.as_ref(), b"first");
                complete(request, WriteCompletion::Delivered);
                assert_eq!(retry.await, CommandResult::Ok);
            })
            .await;
    }

    /// What one connection's read loop does per `INPUT_PASTE` (ADR-0144):
    /// take a credit, waiting while the pane is saturated, then route.
    async fn type_into(
        state: SharedState,
        lane: InputLaneHandle,
        client: ClientId,
        pane: phux_protocol::ids::ResourceId,
        tag: char,
        count: usize,
    ) {
        let mut credits = InputCredits::default();
        for i in 0..count {
            let credit = credits
                .acquire(&state, &pane)
                .await
                .expect("the pane drains within the stall limit");
            let bytes = format!("{tag}{i:04};");
            lane.route(
                RoutedInput::attached(client, pane.clone(), paste(bytes.as_bytes()), "INPUT_PASTE")
                    .with_credit(credit),
            )
            .await;
        }
    }

    /// Drain `count` writes, returning their concatenated bytes. Dropping
    /// each request returns its credit, as the PTY writer does.
    async fn drain(rx: &mut mpsc::Receiver<EncodedInputRequest>, count: usize) -> String {
        let mut out = String::new();
        for _ in 0..count {
            let request = next_write(rx).await;
            out.push_str(std::str::from_utf8(&request.bytes).expect("utf-8 payload"));
        }
        out
    }

    /// Every `{tag}{i};` a client typed, in the order it typed them.
    fn typed_by(tag: char, count: usize) -> String {
        use std::fmt::Write as _;
        (0..count).fold(String::new(), |mut out, i| {
            let _ = write!(out, "{tag}{i:04};");
            out
        })
    }

    fn only(tag: char, written: &str) -> String {
        written
            .split_inclusive(';')
            .filter(|chunk| chunk.starts_with(tag))
            .collect()
    }

    /// ADR-0144: a pane whose writer has stalled saturates, and the clients
    /// typing into it wait instead of losing input, while other panes and
    /// clients keep flowing. Once the stalled writer drains, every event
    /// arrives, in each client's order, and nothing deadlocks.
    #[tokio::test(flavor = "current_thread")]
    async fn a_saturated_pane_loses_no_input_and_blocks_no_other_pane() {
        const PER_CLIENT: usize = 3 * INPUT_CREDITS;
        LocalSet::new()
            .run_until(async {
                let state = SharedState::new();
                let mut stalled = spawn_pane(&state, "a", b"");
                let mut others = [spawn_pane(&state, "b", b""), spawn_pane(&state, "c", b"")];
                let attach = |session: &str| {
                    state.with_mut(|s| {
                        let id = s.new_client_id();
                        s.attach_default_caps(id, session, mpsc::channel(16).0)
                            .expect("attach");
                        id
                    })
                };
                let (a1, a2, b, c) = (attach("a"), attach("a"), attach("b"), attach("c"));
                let lane = spawn_input_lane(state.clone()).expect("spawn lane");

                let mut typists = tokio::task::JoinSet::new();
                for (client, pane, tag) in [
                    (a1, stalled.wire.clone(), 'x'),
                    (a2, stalled.wire.clone(), 'y'),
                    (b, others[0].wire.clone(), 'b'),
                    (c, others[1].wire.clone(), 'c'),
                ] {
                    typists.spawn_local(type_into(
                        state.clone(),
                        lane.handle(),
                        client,
                        pane,
                        tag,
                        PER_CLIENT,
                    ));
                }

                // The other panes take everything while `a` is stuck.
                for (pane, tag) in others.iter_mut().zip(['b', 'c']) {
                    let written = drain(&mut pane.writer_rx, PER_CLIENT).await;
                    assert_eq!(written, typed_by(tag, PER_CLIENT), "pane {tag}");
                }

                // `a` holds exactly its credits, and its typists are waiting.
                tokio::time::timeout(LANE_DELIVERY_DEADLINE, async {
                    while stalled.writer_rx.len() < INPUT_CREDITS {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("the stalled pane fills its credits");
                assert_eq!(stalled.credits.available(), 0);
                assert_eq!(stalled.writer_rx.len(), INPUT_CREDITS);
                tokio::time::timeout(LANE_DELIVERY_DEADLINE, async {
                    while typists.len() > 2 {
                        if typists.try_join_next().is_none() {
                            tokio::task::yield_now().await;
                        }
                    }
                })
                .await
                .expect("the typists into `b` and `c` finish");
                assert!(
                    typists.try_join_next().is_none(),
                    "the typists into `a` wait for credits"
                );

                let written = drain(&mut stalled.writer_rx, 2 * PER_CLIENT).await;
                for tag in ['x', 'y'] {
                    assert_eq!(
                        only(tag, &written),
                        typed_by(tag, PER_CLIENT),
                        "client {tag}"
                    );
                }
                tokio::time::timeout(LANE_DELIVERY_DEADLINE, async {
                    while typists.join_next().await.is_some() {}
                })
                .await
                .expect("every typist finishes");
                assert_no_write(&mut stalled.writer_rx).await;
                assert_eq!(stalled.credits.available(), INPUT_CREDITS);

                for pane in std::iter::once(&stalled).chain(&others) {
                    pane.token.cancel();
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_input_completion_timeout_is_cached_and_late_completion_is_harmless() {
        LocalSet::new()
            .run_until(async {
                let mut fx = fixture();
                fx.lane = spawn_input_lane_with_completion_timeout(
                    fx.state.clone(),
                    Duration::from_millis(10),
                )
                .expect("spawn lane");
                let first = fx.apply(10, &[b"timeout"]);
                let late = next_write(&mut fx.pane.writer_rx).await;
                let result = tokio::time::timeout(LANE_DELIVERY_DEADLINE, first)
                    .await
                    .expect("bounded completion wait");
                assert_eq!(code(&result), Some(ErrorCode::InputDeliveryUnknown));
                complete(late, WriteCompletion::Delivered);
                assert_eq!(fx.apply(10, &[b"timeout"]).await, result);
                assert_no_write(&mut fx.pane.writer_rx).await;
            })
            .await;
    }

    /// A caller that stops waiting still gets its success cached.
    #[tokio::test(flavor = "current_thread")]
    async fn apply_input_caches_success_after_result_waiter_is_dropped() {
        LocalSet::new()
            .run_until(async {
                let mut fx = fixture();
                drop(fx.apply(11, &[b"detached-caller"]));
                complete(
                    next_write(&mut fx.pane.writer_rx).await,
                    WriteCompletion::Delivered,
                );
                while fx.handle().admission.is_in_flight(&fx.pane.wire) {
                    tokio::task::yield_now().await;
                }
                assert_eq!(fx.apply(11, &[b"detached-caller"]).await, CommandResult::Ok);
                assert_no_write(&mut fx.pane.writer_rx).await;
            })
            .await;
    }

    /// A lease refusal keeps the id bound to its payload but retryable.
    #[tokio::test(flavor = "current_thread")]
    async fn apply_input_honors_lease_and_retains_id_binding_after_refusal() {
        LocalSet::new()
            .run_until(async {
                let mut fx = fixture();
                fx.state
                    .with_mut(|s| s.set_input_lease(fx.pane.core, fx.client_b));
                assert_eq!(
                    code(&fx.apply(9, &[b"blocked"]).await),
                    Some(ErrorCode::InputLeaseHeld)
                );
                assert_eq!(
                    code(&fx.apply(9, &[b"different"]).await),
                    Some(ErrorCode::InvalidCommand)
                );
                fx.state
                    .with_mut(|s| s.release_input_lease(fx.pane.core, fx.client_b));
                let retry = fx.apply(9, &[b"blocked"]);
                let request = next_write(&mut fx.pane.writer_rx).await;
                assert_eq!(request.bytes.as_ref(), b"blocked");
                complete(request, WriteCompletion::Delivered);
                assert_eq!(retry.await, CommandResult::Ok);
            })
            .await;
    }

    /// phux-w7z2.58: admission is per Terminal and the completion wait is
    /// off the lane thread. With A's batch unresolved, another pane still
    /// admits `APPLY_INPUT` and `ROUTE_INPUT` (fleet fan-out used to collide,
    /// and one stalled writer used to freeze every pane's input).
    #[tokio::test(flavor = "current_thread")]
    async fn owned_receipts_admit_other_input_before_first_completion() {
        LocalSet::new()
            .run_until(async {
                let mut fx = fixture();
                let mut other = spawn_pane(&fx.state, "t", b"");
                let handle = fx.handle();

                let first_receipt = fx.apply(24, &[b"held"]);
                let first = next_write(&mut fx.pane.writer_rx).await;

                let second_receipt = handle.begin_apply(
                    fx.client_a,
                    operation_id(25),
                    other.wire.clone(),
                    vec![ev(b"other")],
                );
                complete(
                    next_write(&mut other.writer_rx).await,
                    WriteCompletion::Delivered,
                );
                assert_eq!(second_receipt.await, CommandResult::Ok);

                let routed = tokio::time::timeout(
                    ACKNOWLEDGED_COMPLETION_TIMEOUT / 2,
                    handle.begin_route(fx.client_a, other.wire.clone(), ev(b"control"), None),
                )
                .await
                .expect("ROUTE_INPUT must not wait on another pane's completion");
                assert_eq!(routed, CommandResult::Ok);
                assert_eq!(
                    next_write(&mut other.writer_rx).await.bytes.as_ref(),
                    b"control"
                );

                complete(first, WriteCompletion::Delivered);
                assert_eq!(first_receipt.await, CommandResult::Ok);
                other.token.cancel();
            })
            .await;
    }

    /// Same-pane ordering comes from the pane's FIFO mailbox: input routed
    /// after an unresolved batch still reaches the writer behind it.
    #[tokio::test(flavor = "current_thread")]
    async fn later_input_to_the_same_pane_stays_behind_an_unresolved_batch() {
        LocalSet::new()
            .run_until(async {
                let mut fx = fixture();
                let batch = fx.apply(23, &[b"batch"]);
                let first = next_write(&mut fx.pane.writer_rx).await;
                assert_eq!(first.bytes.as_ref(), b"batch");
                assert_eq!(fx.route(fx.client_a, b"after").await, CommandResult::Ok);
                assert_eq!(
                    next_write(&mut fx.pane.writer_rx).await.bytes.as_ref(),
                    b"after"
                );
                complete(first, WriteCompletion::Delivered);
                assert_eq!(batch.await, CommandResult::Ok);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acknowledged_admission_allows_only_one_operation_per_terminal_until_completion() {
        LocalSet::new()
            .run_until(async {
                let mut fx = fixture();
                let first = fx.apply(14, &[b"first"]);
                let first_request = next_write(&mut fx.pane.writer_rx).await;
                let second =
                    tokio::time::timeout(LANE_DELIVERY_DEADLINE, fx.apply(15, &[b"second"]))
                        .await
                        .expect("second admission returns immediately");
                assert_eq!(code(&second), Some(ErrorCode::ResourceExhausted));
                complete(first_request, WriteCompletion::Delivered);
                assert_eq!(first.await, CommandResult::Ok);

                let third = fx.apply(16, &[b"third"]);
                let third_request = next_write(&mut fx.pane.writer_rx).await;
                assert_eq!(third_request.bytes.as_ref(), b"third");
                complete(third_request, WriteCompletion::Delivered);
                assert_eq!(third.await, CommandResult::Ok);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acknowledged_queue_full_rejects_without_leaking_reservation() {
        let (tx, _rx) = mpsc::channel(1);
        let admission = Arc::new(AcknowledgedAdmission::default());
        let handle = InputLaneHandle {
            tx,
            admission: Arc::clone(&admission),
            cache: SharedOperationCache::default(),
        };
        let terminal = phux_protocol::ResourceId::local(1);
        handle
            .route(RoutedInput::attached(
                ClientId(1),
                terminal.clone(),
                paste(b"legacy"),
                "INPUT_PASTE",
            ))
            .await;
        let result = handle
            .begin_apply(
                ClientId(1),
                operation_id(17),
                terminal.clone(),
                vec![ev(b"ack")],
            )
            .await;
        assert_eq!(code(&result), Some(ErrorCode::ResourceExhausted));
        assert!(!admission.is_in_flight(&terminal));
    }
}
