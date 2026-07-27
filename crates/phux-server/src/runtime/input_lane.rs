//! Dedicated input lane (phux-51n6.2, ADR-0044).
//!
//! The server runs one current-thread tokio runtime on a
//! [`tokio::task::LocalSet`] (ADR-0003, ADR-0014): every per-client task and
//! every `!Send` [`libghostty_vt::Terminal`]-owning
//! `TerminalActor` time-share a single
//! core. A large PTY-output broadcast tick and a keystroke therefore compete
//! for the same thread. Commit 284fcbd bounded that contention *inside* the
//! actor's `select!` (input arm biased ahead of output, output coalesce capped
//! and yielding), but the **routing** stage — resolving the wire pane id,
//! walking the subscription set, checking the input lease (ADR-0033), and
//! `try_send`ing onto the pane actor's mailbox — still ran on the main thread,
//! behind whatever output work the runtime was mid-poll on.
//!
//! This module lifts routing **and encoding** onto its own OS thread. The pane
//! actor publishes a complete `Send` snapshot after each terminal mutation:
//! libghostty-captured key options, already-resolved mouse tracking/format,
//! focus/paste modes, and grid/cell dimensions. The lane owns one stateful
//! encoder set per generational pane and hands the resulting bytes to a bounded
//! actor mailbox with `try_send`; neither encoding nor backpressure waits for
//! the main runtime to yield. The `!Send` `Terminal` remains actor-owned.
//!
//! ## Ordering and lease correctness
//!
//! * **Per-client input order** is preserved end to end: a client's read loop
//!   enqueues its local `INPUT_*` and `ROUTE_INPUT` frames onto the same lane
//!   channel in wire order, the tokio mpsc channel is FIFO, the single lane
//!   thread drains it FIFO, and the encoded-byte mailbox it forwards into is FIFO.
//!   Mixed data-plane and control-plane input therefore cannot overtake.
//! * **Lease exclusion** (ADR-0033 "take the wheel") is unchanged: the lane
//!   calls the same destination-resolution helpers as the inline test path,
//!   which re-evaluate the subscription and lease gates atomically under the
//!   state `Mutex` at delivery time. Whoever holds the wheel *when the lane
//!   routes* is honored.
//! * The one behavioral shift: a client's own `INPUT_*` frame now routes on the
//!   lane while its `ACQUIRE_INPUT` / `RELEASE_INPUT` (an L2 `COMMAND`) still
//!   runs inline on the main thread, so their relative timing is no longer
//!   strictly wire-ordered. The lease gate re-check makes every outcome safe
//!   within the fire-and-forget input contract (SPEC §12.2): a key that races
//!   just past its sender's own `RELEASE_INPUT` is delivered if the wheel is
//!   free and dropped if another client has since grabbed it — either is a
//!   legal fire-and-forget result. Cross-client exclusion never weakens.
//!
//! Only **local** pane ids are routed through the lane. Satellite-tagged ids
//! (federation-hub relay, phux-v45.4) stay on the main thread: their delivery
//! is a hub-link forward, not a mailbox `try_send`, and keeping it inline
//! avoids widening the lane's contract to the relay registry. Local
//! `ROUTE_INPUT` carries a one-shot reply so its established correlated
//! command result is emitted only after the lane applies the lease/mailbox
//! operation.

use bytes::BytesMut;
use phux_protocol::ids::InputOperationId;
use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind};
use phux_protocol::{MAX_APPLY_INPUT_COMMAND_BODY, MAX_APPLY_INPUT_EVENTS};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{mpsc, oneshot};

use super::{
    terminal_input_from_event, with_attached_input_destination, with_route_input_destination,
};
use crate::input::{
    InputEncoderSnapshot, PasteOutcome, PerTerminalFocusEncoder, PerTerminalKeyEncoder,
    PerTerminalMouseEncoder, PerTerminalPasteEncoder,
};
use crate::state::{ClientId, SharedState, TerminalInput};
use crate::terminal_actor::{EncodedInputRequest, TerminalHandle};

/// Bound on the lane's inbound queue. Input events are tiny and low-rate, so a
/// generous cap absorbs a paste-expanded burst without unbounded growth. On
/// overflow the lane drops with a `warn!`, matching the fire-and-forget
/// backpressure already applied at the pane mailbox (SPEC §9).
const INPUT_LANE_CAPACITY: usize = 1024;
const DEDUPE_MAX_ENTRIES: usize = 65_536;
const DEDUPE_RETENTION: std::time::Duration = std::time::Duration::from_secs(10 * 60);
const ACKNOWLEDGED_COMPLETION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug)]
pub(crate) struct AcknowledgedReservation {
    admitted: Arc<AtomicBool>,
    admitted_at: std::time::Instant,
}

impl AcknowledgedReservation {
    fn try_acquire(admitted: &Arc<AtomicBool>) -> Option<Self> {
        admitted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self {
                admitted: Arc::clone(admitted),
                admitted_at: std::time::Instant::now(),
            })
    }

    const fn admitted_at(&self) -> std::time::Instant {
        self.admitted_at
    }
}

impl Drop for AcknowledgedReservation {
    fn drop(&mut self) {
        self.admitted.store(false, Ordering::Release);
    }
}

/// One local input operation lifted off the main runtime. Both wire input
/// surfaces share this queue so a client's mixed `INPUT_*` / `ROUTE_INPUT`
/// stream reaches a pane in wire order.
#[derive(Debug)]
pub(crate) struct RoutedInput {
    /// Originating client, for the subscription and lease gates.
    pub(crate) client_id: ClientId,
    /// Wire pane id. Always local (`is_local()`); satellite ids never reach
    /// the lane.
    pub(crate) terminal_id: phux_protocol::ids::TerminalId,
    /// The authority policy and reply behavior for this wire surface.
    pub(crate) kind: RoutedInputKind,
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
        terminal_id: phux_protocol::ids::TerminalId,
        input: TerminalInput,
        frame_label: &'static str,
    ) -> Self {
        Self {
            client_id,
            terminal_id,
            kind: RoutedInputKind::Attached { input, frame_label },
        }
    }
}

/// Cloneable handle a client task uses to hand input to the lane. Cloning is
/// cheap (`mpsc::Sender` clone); every clone keeps the lane thread alive.
#[derive(Clone, Debug)]
pub(crate) struct InputLaneHandle {
    tx: mpsc::Sender<RoutedInput>,
    acknowledged_admitted: Arc<AtomicBool>,
}

impl InputLaneHandle {
    /// Enqueue an input event for off-thread routing. Non-blocking: a full
    /// queue drops the event with a `warn!` (fire-and-forget, SPEC §9), a
    /// closed lane (thread gone during shutdown) drops at `debug!`.
    pub(crate) fn route(&self, routed: RoutedInput) {
        match self.tx.try_send(routed) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(routed)) => {
                tracing::warn!(
                    client_id = ?routed.client_id,
                    terminal_id = ?routed.terminal_id,
                    frame_label = routed.kind.frame_label(),
                    "input lane queue full; dropping (fire-and-forget per SPEC §9)",
                );
                routed.kind.reply_dropped(CommandResult::Ok);
            }
            Err(mpsc::error::TrySendError::Closed(routed)) => {
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
    }

    /// Route attach-free command input through the same FIFO as `INPUT_*` and
    /// wait for the lane's lease/mailbox result.
    pub(crate) async fn route_command(
        &self,
        client_id: ClientId,
        terminal_id: phux_protocol::ids::TerminalId,
        event: InputEvent,
    ) -> CommandResult {
        let (reply, result) = oneshot::channel();
        let routed = RoutedInput {
            client_id,
            terminal_id,
            kind: RoutedInputKind::Headless { event, reply },
        };
        // A command has a correlated result (including TerminalNotFound and
        // InputLeaseHeld), so unlike fire-and-forget INPUT_* it waits for lane
        // capacity rather than losing the authority check on queue overflow.
        if self.tx.send(routed).await.is_err() {
            return CommandResult::Error {
                code: ErrorCode::InternalError,
                message: "input lane unavailable for ROUTE_INPUT".to_owned(),
            };
        }
        result.await.unwrap_or_else(|_| CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "input lane stopped before ROUTE_INPUT completed".to_owned(),
        })
    }

    /// Route an idempotent atomic input batch and wait for PTY write/flush.
    pub(crate) async fn apply_input(
        &self,
        client_id: ClientId,
        operation_id: InputOperationId,
        terminal_id: phux_protocol::ids::TerminalId,
        events: Vec<InputEvent>,
    ) -> CommandResult {
        let Some(reservation) = AcknowledgedReservation::try_acquire(&self.acknowledged_admitted)
        else {
            return acknowledged_resource_exhausted("another APPLY_INPUT operation is in flight");
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
        };
        match self.tx.try_send(routed) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                return acknowledged_resource_exhausted("input lane queue is full");
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return CommandResult::Error {
                    code: ErrorCode::InternalError,
                    message: "input lane unavailable for APPLY_INPUT".to_owned(),
                };
            }
        }
        result.await.unwrap_or_else(|_| CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "input lane stopped before APPLY_INPUT completed".to_owned(),
        })
    }

    #[cfg(test)]
    fn enqueue_command(
        &self,
        client_id: ClientId,
        terminal_id: phux_protocol::ids::TerminalId,
        event: InputEvent,
    ) -> oneshot::Receiver<CommandResult> {
        let (reply, result) = oneshot::channel();
        self.route(RoutedInput {
            client_id,
            terminal_id,
            kind: RoutedInputKind::Headless { event, reply },
        });
        result
    }

    #[cfg(test)]
    fn enqueue_apply_input(
        &self,
        client_id: ClientId,
        operation_id: InputOperationId,
        terminal_id: phux_protocol::ids::TerminalId,
        events: Vec<InputEvent>,
    ) -> Result<oneshot::Receiver<CommandResult>, CommandResult> {
        let reservation = AcknowledgedReservation::try_acquire(&self.acknowledged_admitted)
            .ok_or_else(|| acknowledged_resource_exhausted("APPLY_INPUT already admitted"))?;
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
        };
        self.tx.try_send(routed).map_err(|err| match err {
            mpsc::error::TrySendError::Full(_) => {
                acknowledged_resource_exhausted("input lane queue is full")
            }
            mpsc::error::TrySendError::Closed(_) => CommandResult::Error {
                code: ErrorCode::InternalError,
                message: "input lane unavailable for APPLY_INPUT".to_owned(),
            },
        })?;
        Ok(result)
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

/// Owns the lane's OS thread. Held for the server's lifetime; on drop it closes
/// the channel (the last non-clone sender) and joins the thread so it releases
/// its `SharedState` clone. Hand out routing capability via [`Self::handle`].
#[derive(Debug)]
pub(crate) struct InputLane {
    handle: InputLaneHandle,
    acknowledged_admitted: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl InputLane {
    /// A cloneable routing handle for client tasks.
    pub(crate) fn handle(&self) -> InputLaneHandle {
        debug_assert!(Arc::ptr_eq(
            &self.acknowledged_admitted,
            &self.handle.acknowledged_admitted
        ));
        self.handle.clone()
    }
}

impl Drop for InputLane {
    fn drop(&mut self) {
        // Replace our retained sender with a fresh closed one so the lane sees
        // the channel close once every client-held clone is also dropped, then
        // join. `run_async` drops the `LocalSet` before dropping the lane, which
        // *destroys* (not merely aborts) every per-client task future and so
        // releases every `InputLaneHandle` clone; only then does this join
        // observe the channel close. Dropping the lane while an aborted-but-not-
        // yet-dropped client future still holds a clone would hang the join.
        let (dead_tx, _dead_rx) = mpsc::channel(1);
        self.handle.tx = dead_tx;
        if let Some(join) = self.join.take()
            && let Err(err) = join.join()
        {
            tracing::warn!(?err, "input lane thread panicked on shutdown");
        }
    }
}

/// Spawn the dedicated input-lane thread and return its owner.
///
/// The thread blocks on the channel and, for each [`RoutedInput`], runs the
/// existing attached-input or headless-input gates plus snapshot-driven encode
/// off the main runtime. The thread exits when
/// the channel closes (all senders dropped), releasing its `SharedState`
/// clone.
///
/// # Errors
///
/// Returns the OS error if the lane thread cannot be spawned (a fatal
/// server-startup condition; the caller propagates it).
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

#[derive(Debug)]
struct CachedOperation {
    digest: [u8; 32],
    result: Option<CommandResult>,
    inserted_at: std::time::Instant,
}

#[derive(Debug, PartialEq)]
enum CacheBinding {
    New,
    Pending,
    Final(CommandResult),
    Conflict,
    Full,
}

#[derive(Debug, Default)]
struct OperationCache {
    entries: std::collections::HashMap<InputOperationId, CachedOperation>,
    expiry: std::collections::VecDeque<(std::time::Instant, InputOperationId)>,
}

impl OperationCache {
    fn prune_expired(&mut self, now: std::time::Instant) {
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
        admitted_at: std::time::Instant,
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

fn operation_digest(
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

fn prune_closed_encoders(
    encoders: &mut std::collections::HashMap<phux_core::ids::TerminalId, LaneEncoderSet>,
) {
    encoders.retain(|_, encoder| encoder.snapshot.has_changed().is_ok());
}

fn encoder_for<'a>(
    encoders: &'a mut std::collections::HashMap<phux_core::ids::TerminalId, LaneEncoderSet>,
    pane: phux_core::ids::TerminalId,
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
    encoders: &mut std::collections::HashMap<phux_core::ids::TerminalId, LaneEncoderSet>,
    pane: phux_core::ids::TerminalId,
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

fn handoff_encoded(
    pane: phux_core::ids::TerminalId,
    handle: &TerminalHandle,
    bytes: Option<Vec<u8>>,
) -> Result<bool, CommandResult> {
    let Some(bytes) = bytes else {
        return Ok(true);
    };
    match handle
        .encoded_input
        .try_send(EncodedInputRequest::legacy(bytes))
    {
        Ok(()) => Ok(true),
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::warn!(
                ?pane,
                "encoded-input actor mailbox full; dropping (fire-and-forget per SPEC §9)"
            );
            Ok(false)
        }
        Err(mpsc::error::TrySendError::Closed(_)) => Err(CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor unavailable for ROUTE_INPUT".to_owned(),
        }),
    }
}

fn process_attached(
    state: &SharedState,
    encoders: &mut std::collections::HashMap<phux_core::ids::TerminalId, LaneEncoderSet>,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::TerminalId,
    input: &TerminalInput,
    frame_label: &'static str,
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
    // Re-run the authority gate and perform the non-blocking send under the
    // same state lock, so a lease change cannot slip between gate and delivery
    // while encoding happens off-lock.
    let accepted =
        with_attached_input_destination(state, client_id, terminal_id, frame_label, |current| {
            if !destination
                .handle
                .input_snapshot
                .same_channel(&current.handle.input_snapshot)
            {
                return Ok(false);
            }
            handoff_encoded(current.pane, &current.handle, bytes)
        })
        .and_then(Result::ok)
        .unwrap_or(false);
    if accepted && is_focus_gained {
        crate::hooks::fire_hook(
            state,
            crate::hooks::HookEvent::focus_changed(terminal_id, client_id),
        );
    }
}

fn process_headless(
    state: &SharedState,
    encoders: &mut std::collections::HashMap<phux_core::ids::TerminalId, LaneEncoderSet>,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::TerminalId,
    event: InputEvent,
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
        if !destination
            .handle
            .input_snapshot
            .same_channel(&current.handle.input_snapshot)
        {
            return Ok(false);
        }
        handoff_encoded(current.pane, &current.handle, bytes)
    }) {
        Ok(Ok(_)) => CommandResult::Ok,
        Ok(Err(result)) | Err(result) => result,
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

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the atomic batch path keeps validation, handoff, completion, and caching in one ordered transaction"
)]
fn process_apply_input(
    state: &SharedState,
    encoders: &mut std::collections::HashMap<phux_core::ids::TerminalId, LaneEncoderSet>,
    cache: &mut OperationCache,
    client_id: ClientId,
    operation_id: InputOperationId,
    terminal_id: &phux_protocol::ids::TerminalId,
    events: Vec<InputEvent>,
    admitted_at: std::time::Instant,
    completion_timeout: std::time::Duration,
) -> CommandResult {
    if events.is_empty() || events.len() > MAX_APPLY_INPUT_EVENTS {
        return invalid_command("APPLY_INPUT requires 1..=256 events");
    }

    let digest = operation_digest(operation_id, terminal_id, events.clone());
    let mut inputs = Vec::with_capacity(events.len());
    for event in events {
        match terminal_input_from_event(event) {
            Ok(input) => inputs.push(input),
            Err(result) => return result,
        }
    }
    match cache.bind_at(operation_id, digest, admitted_at) {
        CacheBinding::Final(result) => return result,
        CacheBinding::Conflict => {
            return invalid_command("APPLY_INPUT operation id reused with a different payload");
        }
        CacheBinding::Full => {
            return acknowledged_resource_exhausted("acknowledged-input dedupe cache is full");
        }
        CacheBinding::New | CacheBinding::Pending => {}
    }

    let destination =
        match with_route_input_destination(state, client_id, terminal_id, std::convert::identity) {
            Ok(destination) => destination,
            Err(result) => return result,
        };
    let encoder = match encoder_for(encoders, destination.pane, &destination.handle) {
        Ok(encoder) => encoder,
        Err(err) => {
            tracing::warn!(error = %err, pane = ?destination.pane, "APPLY_INPUT encoder unavailable");
            return invalid_command("APPLY_INPUT encoder unavailable");
        }
    };
    if inputs.iter().any(
        |input| matches!(input, TerminalInput::Paste(event) if encoder.paste.would_reject(event)),
    ) {
        return CommandResult::Error {
            code: ErrorCode::UnsafePaste,
            message: "APPLY_INPUT batch contains an unsafe paste".to_owned(),
        };
    }

    let snapshot = *encoder.snapshot.borrow();
    let mut bytes = Vec::new();
    for input in &inputs {
        match encoder.encode_with_snapshot(input, snapshot) {
            Ok(Some(event_bytes)) => {
                if bytes.len().saturating_add(event_bytes.len()) > MAX_APPLY_INPUT_COMMAND_BODY {
                    return invalid_command("APPLY_INPUT encoded PTY bytes exceed 64 KiB");
                }
                bytes.extend_from_slice(&event_bytes);
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(error = %err, pane = ?destination.pane, "APPLY_INPUT encode failed");
                return invalid_command("APPLY_INPUT event encoding failed");
            }
        }
    }

    let (completion, completed) = std::sync::mpsc::channel();
    let request = EncodedInputRequest::acknowledged(bytes, completion);
    let handed_off = match with_route_input_destination(state, client_id, terminal_id, |current| {
        if !destination
            .handle
            .input_snapshot
            .same_channel(&current.handle.input_snapshot)
        {
            return Err(CommandResult::Error {
                code: ErrorCode::InternalError,
                message: "pane actor changed before APPLY_INPUT handoff".to_owned(),
            });
        }
        match current.handle.encoded_input.try_send(request) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => Err(CommandResult::Error {
                code: ErrorCode::ResourceExhausted,
                message: "pane actor input mailbox is full".to_owned(),
            }),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(CommandResult::Error {
                code: ErrorCode::InternalError,
                message: "pane actor unavailable for APPLY_INPUT".to_owned(),
            }),
        }
    }) {
        Ok(result) => result,
        Err(result) => Err(result),
    };
    if let Err(result) = handed_off {
        return result;
    }

    let result = match completed.recv_timeout(completion_timeout) {
        Ok(true) => CommandResult::Ok,
        Ok(false) | Err(_) => CommandResult::Error {
            code: ErrorCode::InputDeliveryUnknown,
            message: "PTY input delivery could not be confirmed".to_owned(),
        },
    };
    cache.set_final(operation_id, result.clone());
    result
}

pub(crate) fn spawn_input_lane(state: SharedState) -> std::io::Result<InputLane> {
    spawn_input_lane_with_completion_timeout(state, ACKNOWLEDGED_COMPLETION_TIMEOUT)
}

fn spawn_input_lane_with_completion_timeout(
    state: SharedState,
    completion_timeout: std::time::Duration,
) -> std::io::Result<InputLane> {
    let (tx, mut rx) = mpsc::channel::<RoutedInput>(INPUT_LANE_CAPACITY);
    let acknowledged_admitted = Arc::new(AtomicBool::new(false));
    let join = std::thread::Builder::new()
        .name("phux-input-lane".to_owned())
        .spawn(move || {
            let mut encoders = std::collections::HashMap::new();
            let mut cache = OperationCache::default();
            // `blocking_recv` parks the thread with no tokio runtime on it.
            // Gating, snapshot reads, encoding, and the bounded actor handoff
            // are all synchronous and non-blocking.
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
                    ),
                    RoutedInputKind::Headless { event, reply } => {
                        let result = process_headless(
                            &state,
                            &mut encoders,
                            routed.client_id,
                            &routed.terminal_id,
                            event,
                        );
                        let _ = reply.send(result);
                    }
                    RoutedInputKind::Acknowledged {
                        operation_id,
                        events,
                        reservation,
                        reply,
                    } => {
                        let admitted_at = reservation.admitted_at();
                        let result = process_apply_input(
                            &state,
                            &mut encoders,
                            &mut cache,
                            routed.client_id,
                            operation_id,
                            &routed.terminal_id,
                            events,
                            admitted_at,
                            completion_timeout,
                        );
                        drop(reservation);
                        let _ = reply.send(result);
                    }
                }
            }
            tracing::debug!("input lane thread exiting (channel closed)");
        })?;
    Ok(InputLane {
        handle: InputLaneHandle {
            tx,
            acknowledged_admitted: Arc::clone(&acknowledged_admitted),
        },
        acknowledged_admitted,
        join: Some(join),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use phux_protocol::input::paste::{PasteEvent, PasteTrust};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::terminal_actor::TerminalActor;

    /// Ceiling for "the lane was handed this work, so it should already be on
    /// the other side" waits.
    ///
    /// Not load-bearing, and deliberately far larger than anything these
    /// tests measure: every one of them resolves in single-digit
    /// milliseconds, and none asserts a latency. The bound exists so a lane
    /// that genuinely wedges fails the run instead of hanging the binary.
    ///
    /// Sized this way on purpose (phux-br1f). The previous 1-2s bounds were
    /// generous on an idle laptop and not generous on a saturated one, where
    /// the current-thread runtime these tests share can lose its core for
    /// seconds at a stretch — and a suite that cries wolf gets ignored, which
    /// is how a real regression ships.
    ///
    /// The one bound in this module that is NOT this constant is the fast
    /// path in `apply_input_full_writer_queue_is_bounded_and_returns_delivery_unknown`,
    /// where the deadline IS the assertion; see the comment there.
    const LANE_DELIVERY_DEADLINE: Duration = Duration::from_secs(30);

    /// Ceiling for "and nothing else arrives" checks.
    ///
    /// The opposite polarity, so it must stay short: expiry is the PASS, and
    /// load can only make it pass harder. Enlarging it would just slow the
    /// suite down without proving anything more.
    const NOTHING_FURTHER_WINDOW: Duration = Duration::from_millis(100);

    /// A trusted paste of `bytes`. With DEC 2004 bracketed-paste off (a fresh
    /// `Terminal`'s default) the pane's paste encoder emits exactly `bytes` on
    /// the PTY writer channel, so the routed input is byte-identifiable.
    fn paste_event(bytes: &[u8]) -> PasteEvent {
        PasteEvent {
            trust: PasteTrust::Trusted,
            data: bytes.to_vec(),
        }
    }

    fn paste(bytes: &[u8]) -> TerminalInput {
        TerminalInput::Paste(paste_event(bytes))
    }

    fn operation_id(byte: u8) -> InputOperationId {
        InputOperationId::new([byte; 16]).expect("non-zero operation id")
    }

    fn operation_id_from_u64(value: u64) -> InputOperationId {
        let mut bytes = [0; 16];
        bytes[8..].copy_from_slice(&value.to_be_bytes());
        InputOperationId::new(bytes).expect("non-zero operation id")
    }

    /// A registered pane actor plus two subscribed clients, wired into a fresh
    /// [`SharedState`] and spawned on the caller's `LocalSet`. Input routed
    /// through the lane for `wire` and gated on `pane`'s subscription/lease is
    /// encoded by the actor and observable on `writer_rx`.
    struct Fixture {
        state: SharedState,
        wire: phux_protocol::ids::TerminalId,
        pane: phux_core::ids::TerminalId,
        client_a: ClientId,
        client_b: ClientId,
        encoded_input: mpsc::Sender<EncodedInputRequest>,
        writer_rx: mpsc::Receiver<EncodedInputRequest>,
        token: CancellationToken,
    }

    /// Build the fixture and spawn the actor. Must run inside a `LocalSet`
    /// (the pane actor owns a `!Send` `Terminal`, per ADR-0014).
    fn spawn_fixture() -> Fixture {
        spawn_fixture_with_seed(b"")
    }

    fn spawn_fixture_with_seed(seed: &[u8]) -> Fixture {
        let bundle = TerminalActor::new_with_seed(80, 24, seed).expect("actor");
        let handle = bundle.handle.clone();
        let encoded_input = handle.encoded_input.clone();
        let token = bundle.token.clone();
        let mut actor = bundle.actor;
        let (_pty_evt_tx, writer_rx) = actor.install_test_pty_channels();

        let state = SharedState::new();
        let (wire, pane, client_a, client_b) = state.with_mut(|s| {
            let (_sid, _wid, pane) = s.seed_session("s");
            let wire = s.register_terminal_handle(pane, handle.clone(), token.clone());
            let a = s.new_client_id();
            let b = s.new_client_id();
            let (tx_a, _rx_a) = mpsc::channel(16);
            let (tx_b, _rx_b) = mpsc::channel(16);
            s.attach_default_caps(a, "s", tx_a).expect("attach a");
            s.attach_default_caps(b, "s", tx_b).expect("attach b");
            (wire, pane, a, b)
        });

        tokio::task::spawn_local(actor.run());

        Fixture {
            state,
            wire,
            pane,
            client_a,
            client_b,
            encoded_input,
            writer_rx,
            token,
        }
    }

    /// The lane delivers routed input to the pane's PTY writer off the main
    /// runtime, preserving per-client FIFO order: three distinct pastes routed
    /// in order arrive on the writer channel in the same order.
    #[tokio::test(flavor = "current_thread")]
    async fn lane_routes_input_to_pane_in_order() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut fx = spawn_fixture();
                let lane = spawn_input_lane(fx.state.clone()).expect("spawn lane");

                for byte in [&b"a"[..], b"b", b"c"] {
                    lane.handle().route(RoutedInput::attached(
                        fx.client_a,
                        fx.wire.clone(),
                        paste(byte),
                        "INPUT_PASTE",
                    ));
                }

                for expected in [&b"a"[..], b"b", b"c"] {
                    let got = tokio::time::timeout(LANE_DELIVERY_DEADLINE, fx.writer_rx.recv())
                        .await
                        .expect("lane must deliver routed input to the PTY writer")
                        .expect("writer channel open");
                    assert_eq!(
                        got.bytes, expected,
                        "routed input must arrive in FIFO order"
                    );
                }

                fx.token.cancel();
            })
            .await;
    }

    /// Lease exclusion (ADR-0033) holds across the lane: with client B holding
    /// the wheel, client A's routed input is dropped by the lease gate on the
    /// lane thread while B's is delivered — the first (and only) byte the PTY
    /// writer sees is B's.
    #[tokio::test(flavor = "current_thread")]
    async fn lane_honors_input_lease() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut fx = spawn_fixture();
                // B takes the wheel.
                fx.state
                    .with_mut(|s| s.set_input_lease(fx.pane, fx.client_b));

                let lane = spawn_input_lane(fx.state.clone()).expect("spawn lane");
                // A is blocked on both input surfaces. Attached input is
                // silently dropped; ROUTE_INPUT keeps its typed lease error.
                lane.handle().route(RoutedInput::attached(
                    fx.client_a,
                    fx.wire.clone(),
                    paste(b"a"),
                    "INPUT_PASTE",
                ));
                let route_result = lane
                    .handle()
                    .route_command(
                        fx.client_a,
                        fx.wire.clone(),
                        InputEvent::Paste(paste_event(b"r")),
                    )
                    .await;
                assert!(
                    matches!(
                        route_result,
                        CommandResult::Error {
                            code: ErrorCode::InputLeaseHeld,
                            ..
                        }
                    ),
                    "ROUTE_INPUT must retain its lease error on the lane",
                );
                // B holds the wheel; its input must be delivered.
                lane.handle().route(RoutedInput::attached(
                    fx.client_b,
                    fx.wire.clone(),
                    paste(b"B"),
                    "INPUT_PASTE",
                ));

                let got = tokio::time::timeout(LANE_DELIVERY_DEADLINE, fx.writer_rx.recv())
                    .await
                    .expect("B's leased input must reach the PTY writer")
                    .expect("writer channel open");
                assert_eq!(
                    got.bytes, b"B",
                    "the lease holder's input is delivered; the blocked client's is dropped",
                );
                // A's input must never arrive: nothing else is queued.
                let extra = tokio::time::timeout(NOTHING_FURTHER_WINDOW, fx.writer_rx.recv()).await;
                assert!(
                    extra.is_err(),
                    "blocked client's input must not reach the PTY writer",
                );

                fx.token.cancel();
            })
            .await;
    }

    /// A `ROUTE_INPUT` completes while the current-thread runtime is
    /// synchronously occupied. The result receiver is inspected without an
    /// `.await`, so only the dedicated OS thread can have run the lease check
    /// and mailbox send during the sleep; an inline/main-thread route cannot
    /// satisfy this test.
    #[tokio::test(flavor = "current_thread")]
    async fn route_input_routes_while_main_runtime_is_not_polling() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let fx = spawn_fixture();
                let lane = spawn_input_lane(fx.state.clone()).expect("spawn lane");
                // Hold the authority mutex while enqueueing so the lane cannot
                // finish before this task stops polling. Once the closure
                // releases the lock, the synchronous loop below occupies the
                // runtime thread; only the lane thread can produce the reply.
                let handle = lane.handle();
                let mut result = fx.state.with(|_| {
                    let mut result = handle.enqueue_command(
                        fx.client_a,
                        fx.wire.clone(),
                        InputEvent::Paste(paste_event(b"k")),
                    );
                    assert!(
                        matches!(result.try_recv(), Err(oneshot::error::TryRecvError::Empty)),
                        "lane must be blocked on the held state mutex",
                    );
                    result
                });

                // Blocking spin, so this is the one place the deadline costs
                // real thread time on the failure path — but the fact under
                // test is "the lane routes at all while the runtime thread is
                // occupied", never how fast, so the bound is the shared
                // generous one.
                let deadline = std::time::Instant::now() + LANE_DELIVERY_DEADLINE;
                let routed = loop {
                    match result.try_recv() {
                        Ok(result) => break result,
                        Err(oneshot::error::TryRecvError::Empty)
                            if std::time::Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        other => {
                            panic!("lane did not route while main runtime was blocked: {other:?}")
                        }
                    }
                };
                assert_eq!(routed, CommandResult::Ok);
                fx.token.cancel();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lane_encodes_from_published_bracketed_paste_snapshot() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut fx = spawn_fixture_with_seed(b"\x1b[?2004h");
                let lane = spawn_input_lane(fx.state.clone()).expect("spawn lane");
                lane.handle().route(RoutedInput::attached(
                    fx.client_a,
                    fx.wire.clone(),
                    paste(b"lane"),
                    "INPUT_PASTE",
                ));
                let got = tokio::time::timeout(LANE_DELIVERY_DEADLINE, fx.writer_rx.recv())
                    .await
                    .expect("lane delivery")
                    .expect("writer open");
                assert_eq!(got.bytes, b"\x1b[200~lane\x1b[201~");
                fx.token.cancel();
            })
            .await;
    }

    #[test]
    fn encoded_actor_handoff_is_bounded_and_nonblocking() {
        let bundle = TerminalActor::new(80, 24).expect("actor");
        for _ in 0..crate::terminal_actor::DEFAULT_INPUT_MAILBOX {
            bundle
                .handle
                .encoded_input
                .try_send(EncodedInputRequest::legacy(vec![b'x']))
                .expect("capacity available");
        }
        assert!(matches!(
            bundle
                .handle
                .encoded_input
                .try_send(EncodedInputRequest::legacy(vec![b'y'])),
            Err(mpsc::error::TrySendError::Full(_))
        ));
    }

    #[test]
    fn encoder_cache_drops_closed_actor_generations() {
        let bundle = TerminalActor::new(80, 24).expect("actor");
        let state = SharedState::new();
        let pane = state.with_mut(|s| s.seed_session("s").2);
        let mut encoders = std::collections::HashMap::new();
        encoder_for(&mut encoders, pane, &bundle.handle).expect("encoder");
        assert_eq!(encoders.len(), 1);
        drop(bundle);
        prune_closed_encoders(&mut encoders);
        assert!(encoders.is_empty());
    }

    #[test]
    fn encoder_cache_replaces_state_when_actor_generation_changes() {
        let first = TerminalActor::new(80, 24).expect("first actor");
        let second = TerminalActor::new(80, 24).expect("second actor");
        let state = SharedState::new();
        let pane = state.with_mut(|s| s.seed_session("s").2);
        let mut encoders = std::collections::HashMap::new();
        encoder_for(&mut encoders, pane, &first.handle).expect("first encoder");
        assert!(
            encoders[&pane]
                .snapshot
                .same_channel(&first.handle.input_snapshot)
        );
        encoder_for(&mut encoders, pane, &second.handle).expect("replacement encoder");
        assert!(
            encoders[&pane]
                .snapshot
                .same_channel(&second.handle.input_snapshot)
        );
        assert_eq!(encoders.len(), 1, "one encoder set per pane generation");
    }

    /// `INPUT_*` and `ROUTE_INPUT` use one lane FIFO. Distinct paste payloads
    /// make the pane writer's observed order unambiguous.
    #[tokio::test(flavor = "current_thread")]
    async fn mixed_attached_and_route_input_preserve_fifo() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut fx = spawn_fixture();
                let lane = spawn_input_lane(fx.state.clone()).expect("spawn lane");
                let handle = lane.handle();

                handle.route(RoutedInput::attached(
                    fx.client_a,
                    fx.wire.clone(),
                    paste(b"a"),
                    "INPUT_PASTE",
                ));
                let route_result = handle.enqueue_command(
                    fx.client_a,
                    fx.wire.clone(),
                    InputEvent::Paste(paste_event(b"b")),
                );
                handle.route(RoutedInput::attached(
                    fx.client_a,
                    fx.wire.clone(),
                    paste(b"c"),
                    "INPUT_PASTE",
                ));

                assert_eq!(route_result.await.expect("lane reply"), CommandResult::Ok);
                for expected in [&b"a"[..], b"b", b"c"] {
                    let got = tokio::time::timeout(LANE_DELIVERY_DEADLINE, fx.writer_rx.recv())
                        .await
                        .expect("mixed input must reach PTY")
                        .expect("writer channel open");
                    assert_eq!(got.bytes, expected, "mixed lane routing must stay FIFO");
                }
                fx.token.cancel();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_input_writes_one_atomic_batch_then_acks_and_deduplicates() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut fx = spawn_fixture();
                let lane = spawn_input_lane(fx.state.clone()).expect("spawn lane");
                let handle = lane.handle();
                let id = operation_id(1);
                let events = vec![
                    InputEvent::Paste(paste_event(b"one")),
                    InputEvent::Paste(paste_event(b"two")),
                ];

                let first_handle = handle.clone();
                let first_wire = fx.wire.clone();
                let first_events = events.clone();
                let first = tokio::task::spawn_local(async move {
                    first_handle
                        .apply_input(fx.client_a, id, first_wire, first_events)
                        .await
                });
                let request = tokio::time::timeout(LANE_DELIVERY_DEADLINE, fx.writer_rx.recv())
                    .await
                    .expect("batch handed to writer")
                    .expect("writer open");
                assert_eq!(request.bytes, b"onetwo");
                request
                    .completion
                    .expect("acknowledged completion")
                    .send(true)
                    .expect("lane waiting");
                assert_eq!(first.await.expect("apply task"), CommandResult::Ok);

                assert_eq!(
                    handle
                        .apply_input(fx.client_a, id, fx.wire.clone(), events)
                        .await,
                    CommandResult::Ok
                );
                assert!(
                    tokio::time::timeout(NOTHING_FURTHER_WINDOW, fx.writer_rx.recv())
                        .await
                        .is_err(),
                    "same operation must not write twice"
                );
                fx.token.cancel();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_input_rejects_conflicting_reuse_empty_oversized_and_unsafe_batches() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut fx = spawn_fixture();
                let lane = spawn_input_lane(fx.state.clone()).expect("spawn lane");
                let handle = lane.handle();
                let id = operation_id(2);
                let original = vec![InputEvent::Paste(paste_event(b"original"))];
                let first_handle = handle.clone();
                let first_wire = fx.wire.clone();
                let first_events = original.clone();
                let first = tokio::task::spawn_local(async move {
                    first_handle
                        .apply_input(fx.client_a, id, first_wire, first_events)
                        .await
                });
                let request = fx.writer_rx.recv().await.expect("first write");
                request.completion.expect("completion").send(true).unwrap();
                assert_eq!(first.await.unwrap(), CommandResult::Ok);

                let conflict = handle
                    .apply_input(
                        fx.client_a,
                        id,
                        fx.wire.clone(),
                        vec![InputEvent::Paste(paste_event(b"different"))],
                    )
                    .await;
                assert!(matches!(
                    conflict,
                    CommandResult::Error {
                        code: ErrorCode::InvalidCommand,
                        ..
                    }
                ));

                for (id, events) in [
                    (operation_id(3), vec![]),
                    (
                        operation_id(4),
                        std::iter::repeat_with(|| InputEvent::Paste(paste_event(b"x")))
                            .take(MAX_APPLY_INPUT_EVENTS + 1)
                            .collect(),
                    ),
                ] {
                    assert!(matches!(
                        handle
                            .apply_input(fx.client_a, id, fx.wire.clone(), events)
                            .await,
                        CommandResult::Error {
                            code: ErrorCode::InvalidCommand,
                            ..
                        }
                    ));
                }

                let unsafe_result = handle
                    .apply_input(
                        fx.client_a,
                        operation_id(5),
                        fx.wire.clone(),
                        vec![
                            InputEvent::Paste(paste_event(b"prefix")),
                            InputEvent::Paste(PasteEvent {
                                trust: PasteTrust::Untrusted,
                                data: b"danger\n".to_vec(),
                            }),
                        ],
                    )
                    .await;
                assert!(matches!(
                    unsafe_result,
                    CommandResult::Error {
                        code: ErrorCode::UnsafePaste,
                        ..
                    }
                ));
                assert!(
                    tokio::time::timeout(NOTHING_FURTHER_WINDOW, fx.writer_rx.recv())
                        .await
                        .is_err(),
                    "every refusal must happen before a write"
                );
                fx.token.cancel();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_input_reports_mailbox_full_without_handoff() {
        let bundle = TerminalActor::new(80, 24).expect("actor");
        for _ in 0..crate::terminal_actor::DEFAULT_INPUT_MAILBOX {
            bundle
                .handle
                .encoded_input
                .try_send(EncodedInputRequest::legacy(vec![b'x']))
                .expect("fill mailbox");
        }
        let state = SharedState::new();
        let (wire, client) = state.with_mut(|s| {
            let pane = s.seed_session("s").2;
            let wire =
                s.register_terminal_handle(pane, bundle.handle.clone(), bundle.token.clone());
            (wire, s.new_client_id())
        });
        let lane = spawn_input_lane(state).expect("spawn lane");
        let result = lane
            .handle()
            .apply_input(
                client,
                operation_id(6),
                wire,
                vec![InputEvent::Paste(paste_event(b"x"))],
            )
            .await;
        assert!(matches!(
            result,
            CommandResult::Error {
                code: ErrorCode::ResourceExhausted,
                ..
            }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_input_full_writer_queue_is_bounded_and_returns_delivery_unknown() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let fx = spawn_fixture();
                assert_eq!(
                    fx.writer_rx.max_capacity(),
                    crate::terminal_actor::DEFAULT_INPUT_MAILBOX
                );
                for _ in 0..crate::terminal_actor::DEFAULT_INPUT_MAILBOX {
                    fx.encoded_input
                        .try_send(EncodedInputRequest::legacy(vec![b'x']))
                        .expect("actor input mailbox open");
                }
                tokio::time::timeout(LANE_DELIVERY_DEADLINE, async {
                    while fx.writer_rx.len() < crate::terminal_actor::DEFAULT_INPUT_MAILBOX {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("actor must fill the writer queue");

                let lane = spawn_input_lane(fx.state.clone()).expect("spawn lane");
                let handle = lane.handle();
                // The ONLY deadline in this module that is itself the
                // assertion, so it cannot join `LANE_DELIVERY_DEADLINE`: a
                // full writer queue must short-circuit to
                // `InputDeliveryUnknown` rather than wait out the product's
                // `ACKNOWLEDGED_COMPLETION_TIMEOUT`, and both paths end in
                // the same `CommandResult`. Strictly-under is the whole
                // claim, so the bound is derived from the product constant
                // instead of hand-picked — half of it is 5x the 500ms this
                // used to be (headroom against a loaded runner) while still
                // failing if the slow path were ever taken.
                let fast_path_bound = ACKNOWLEDGED_COMPLETION_TIMEOUT / 2;
                for (id, payload) in [(18, b"first".as_slice()), (19, b"second".as_slice())] {
                    let result = tokio::time::timeout(
                        fast_path_bound,
                        handle.apply_input(
                            fx.client_a,
                            operation_id(id),
                            fx.wire.clone(),
                            vec![InputEvent::Paste(paste_event(payload))],
                        ),
                    )
                    .await
                    .expect("full writer queue must resolve without the completion timeout");
                    assert!(matches!(
                        result,
                        CommandResult::Error {
                            code: ErrorCode::InputDeliveryUnknown,
                            ..
                        }
                    ));
                    assert_eq!(
                        fx.writer_rx.len(),
                        crate::terminal_actor::DEFAULT_INPUT_MAILBOX,
                        "dropping acknowledged input must not grow the full writer queue"
                    );
                }
                fx.token.cancel();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_input_caches_writer_failure_as_delivery_unknown() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut fx = spawn_fixture();
                let lane = spawn_input_lane(fx.state.clone()).expect("spawn lane");
                let handle = lane.handle();
                let id = operation_id(7);
                let events = vec![InputEvent::Paste(paste_event(b"uncertain"))];
                let first_handle = handle.clone();
                let first_wire = fx.wire.clone();
                let first_events = events.clone();
                let first = tokio::task::spawn_local(async move {
                    first_handle
                        .apply_input(fx.client_a, id, first_wire, first_events)
                        .await
                });
                let request = fx.writer_rx.recv().await.expect("writer request");
                request.completion.expect("completion").send(false).unwrap();
                let first_result = first.await.unwrap();
                assert!(matches!(
                    first_result,
                    CommandResult::Error {
                        code: ErrorCode::InputDeliveryUnknown,
                        ..
                    }
                ));

                let retry = handle
                    .apply_input(fx.client_a, id, fx.wire.clone(), events)
                    .await;
                assert_eq!(retry, first_result);
                assert!(
                    tokio::time::timeout(NOTHING_FURTHER_WINDOW, fx.writer_rx.recv())
                        .await
                        .is_err(),
                    "unknown result must be deduplicated without another write"
                );
                fx.token.cancel();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_input_honors_lease_and_retains_id_binding_after_refusal() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut fx = spawn_fixture();
                fx.state
                    .with_mut(|state| state.set_input_lease(fx.pane, fx.client_b));
                let lane = spawn_input_lane(fx.state.clone()).expect("spawn lane");
                let handle = lane.handle();
                let id = operation_id(9);

                let refused = handle
                    .apply_input(
                        fx.client_a,
                        id,
                        fx.wire.clone(),
                        vec![InputEvent::Paste(paste_event(b"blocked"))],
                    )
                    .await;
                assert!(matches!(
                    refused,
                    CommandResult::Error {
                        code: ErrorCode::InputLeaseHeld,
                        ..
                    }
                ));

                let conflicting = handle
                    .apply_input(
                        fx.client_a,
                        id,
                        fx.wire.clone(),
                        vec![InputEvent::Paste(paste_event(b"different"))],
                    )
                    .await;
                assert!(matches!(
                    conflicting,
                    CommandResult::Error {
                        code: ErrorCode::InvalidCommand,
                        ..
                    }
                ));

                fx.state
                    .with_mut(|state| state.release_input_lease(fx.pane, fx.client_b));
                let retry_handle = handle.clone();
                let retry_wire = fx.wire.clone();
                let retry = tokio::task::spawn_local(async move {
                    retry_handle
                        .apply_input(
                            fx.client_a,
                            id,
                            retry_wire,
                            vec![InputEvent::Paste(paste_event(b"blocked"))],
                        )
                        .await
                });
                let request = fx.writer_rx.recv().await.expect("retry write");
                assert_eq!(request.bytes, b"blocked");
                request.completion.expect("completion").send(true).unwrap();
                assert_eq!(retry.await.unwrap(), CommandResult::Ok);
                fx.token.cancel();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_input_waiter_finishes_if_writer_channel_is_closed() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let fx = spawn_fixture();
                drop(fx.writer_rx);
                let lane = spawn_input_lane(fx.state.clone()).expect("spawn lane");
                let result = tokio::time::timeout(
                    LANE_DELIVERY_DEADLINE,
                    lane.handle().apply_input(
                        fx.client_a,
                        operation_id(8),
                        fx.wire,
                        vec![InputEvent::Paste(paste_event(b"x"))],
                    ),
                )
                .await
                .expect("waiter must not hang");
                assert!(matches!(
                    result,
                    CommandResult::Error {
                        code: ErrorCode::InputDeliveryUnknown,
                        ..
                    }
                ));
                fx.token.cancel();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_input_completion_timeout_is_cached_and_late_completion_is_harmless() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut fx = spawn_fixture();
                let lane = spawn_input_lane_with_completion_timeout(
                    fx.state.clone(),
                    Duration::from_millis(10),
                )
                .expect("spawn lane");
                let handle = lane.handle();
                let id = operation_id(10);
                let events = vec![InputEvent::Paste(paste_event(b"timeout"))];
                let first_handle = handle.clone();
                let first_wire = fx.wire.clone();
                let first_events = events.clone();
                let first = tokio::task::spawn_local(async move {
                    first_handle
                        .apply_input(fx.client_a, id, first_wire, first_events)
                        .await
                });
                let request = fx.writer_rx.recv().await.expect("writer request");
                let late_completion = request.completion.expect("completion");
                let result = tokio::time::timeout(LANE_DELIVERY_DEADLINE, first)
                    .await
                    .expect("bounded completion wait")
                    .expect("apply task");
                assert!(matches!(
                    result,
                    CommandResult::Error {
                        code: ErrorCode::InputDeliveryUnknown,
                        ..
                    }
                ));
                assert!(late_completion.send(true).is_err());
                assert_eq!(
                    handle
                        .apply_input(fx.client_a, id, fx.wire.clone(), events)
                        .await,
                    result
                );
                assert!(
                    tokio::time::timeout(NOTHING_FURTHER_WINDOW, fx.writer_rx.recv())
                        .await
                        .is_err()
                );
                fx.token.cancel();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_input_caches_success_after_result_waiter_is_dropped() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut fx = spawn_fixture();
                let lane = spawn_input_lane(fx.state.clone()).expect("spawn lane");
                let handle = lane.handle();
                let id = operation_id(11);
                let events = vec![InputEvent::Paste(paste_event(b"detached-caller"))];
                let waiter = handle
                    .enqueue_apply_input(fx.client_a, id, fx.wire.clone(), events.clone())
                    .expect("admit operation");
                let request = fx.writer_rx.recv().await.expect("writer request");
                drop(waiter);
                request.completion.expect("completion").send(true).unwrap();
                while handle.acknowledged_admitted.load(Ordering::Acquire) {
                    tokio::task::yield_now().await;
                }

                assert_eq!(
                    handle
                        .apply_input(fx.client_a, id, fx.wire.clone(), events)
                        .await,
                    CommandResult::Ok
                );
                assert!(
                    tokio::time::timeout(NOTHING_FURTHER_WINDOW, fx.writer_rx.recv())
                        .await
                        .is_err(),
                    "retry after lost result must use the cached success"
                );
                fx.token.cancel();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_input_rejects_satellite_and_oversized_encoded_bytes_before_handoff() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut fx = spawn_fixture();
                let lane = spawn_input_lane(fx.state.clone()).expect("spawn lane");
                let satellite = lane
                    .handle()
                    .apply_input(
                        fx.client_a,
                        operation_id(12),
                        phux_protocol::TerminalId::satellite("peer", 1),
                        vec![InputEvent::Paste(paste_event(b"x"))],
                    )
                    .await;
                assert!(matches!(
                    satellite,
                    CommandResult::Error {
                        code: ErrorCode::UnsupportedSatelliteRoute,
                        ..
                    }
                ));

                let oversized_payload = vec![b'x'; MAX_APPLY_INPUT_COMMAND_BODY + 1];
                let oversized = lane
                    .handle()
                    .apply_input(
                        fx.client_a,
                        operation_id(13),
                        fx.wire.clone(),
                        vec![InputEvent::Paste(paste_event(&oversized_payload))],
                    )
                    .await;
                assert!(matches!(
                    oversized,
                    CommandResult::Error {
                        code: ErrorCode::InvalidCommand,
                        ..
                    }
                ));
                assert!(
                    tokio::time::timeout(NOTHING_FURTHER_WINDOW, fx.writer_rx.recv())
                        .await
                        .is_err()
                );
                fx.token.cancel();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acknowledged_admission_allows_only_one_operation_until_completion() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut fx = spawn_fixture();
                let lane = spawn_input_lane(fx.state.clone()).expect("spawn lane");
                let handle = lane.handle();
                let first_handle = handle.clone();
                let first_wire = fx.wire.clone();
                let first = tokio::task::spawn_local(async move {
                    first_handle
                        .apply_input(
                            fx.client_a,
                            operation_id(14),
                            first_wire,
                            vec![InputEvent::Paste(paste_event(b"first"))],
                        )
                        .await
                });
                let first_request = fx.writer_rx.recv().await.expect("first writer request");

                let second = tokio::time::timeout(
                    LANE_DELIVERY_DEADLINE,
                    handle.apply_input(
                        fx.client_a,
                        operation_id(15),
                        fx.wire.clone(),
                        vec![InputEvent::Paste(paste_event(b"second"))],
                    ),
                )
                .await
                .expect("second admission must return immediately");
                assert!(matches!(
                    second,
                    CommandResult::Error {
                        code: ErrorCode::ResourceExhausted,
                        ..
                    }
                ));

                first_request
                    .completion
                    .expect("completion")
                    .send(true)
                    .unwrap();
                assert_eq!(first.await.unwrap(), CommandResult::Ok);

                let third_handle = handle.clone();
                let third_wire = fx.wire.clone();
                let third = tokio::task::spawn_local(async move {
                    third_handle
                        .apply_input(
                            fx.client_a,
                            operation_id(16),
                            third_wire,
                            vec![InputEvent::Paste(paste_event(b"third"))],
                        )
                        .await
                });
                let third_request = fx.writer_rx.recv().await.expect("third writer request");
                assert_eq!(third_request.bytes, b"third");
                third_request
                    .completion
                    .expect("completion")
                    .send(true)
                    .unwrap();
                assert_eq!(third.await.unwrap(), CommandResult::Ok);
                fx.token.cancel();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acknowledged_queue_full_rejects_without_leaking_reservation() {
        let (tx, _rx) = mpsc::channel(1);
        let admitted = Arc::new(AtomicBool::new(false));
        let handle = InputLaneHandle {
            tx,
            acknowledged_admitted: Arc::clone(&admitted),
        };
        handle.route(RoutedInput::attached(
            ClientId(1),
            phux_protocol::TerminalId::local(1),
            paste(b"legacy"),
            "INPUT_PASTE",
        ));
        let result = handle
            .apply_input(
                ClientId(1),
                operation_id(17),
                phux_protocol::TerminalId::local(1),
                vec![InputEvent::Paste(paste_event(b"ack"))],
            )
            .await;
        assert!(matches!(
            result,
            CommandResult::Error {
                code: ErrorCode::ResourceExhausted,
                ..
            }
        ));
        assert!(!admitted.load(Ordering::Acquire));
    }

    #[test]
    fn operation_cache_prunes_only_expired_prefix_and_treats_over_capacity_as_full() {
        let start = std::time::Instant::now();
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
        let start = std::time::Instant::now();
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
}
