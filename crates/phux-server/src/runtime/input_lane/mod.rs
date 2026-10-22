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
//! * Accepted key, mouse, and paste input emits an unjournaled
//!   `input_activity` to the pane's other subscribers, at most once per
//!   250 ms per pane (leading edge). Focus does not. The sender is excluded.

mod acknowledged;
mod credit;
mod encode;
mod handle;
mod lane;
mod process;
mod receipt;

use phux_protocol::ids::InputOperationId;
use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::CommandResult;
use tokio::sync::oneshot;

pub(crate) use self::acknowledged::AcknowledgedReservation;
use self::acknowledged::{
    ACKNOWLEDGED_COMPLETION_TIMEOUT, AcknowledgedAdmission, CacheClaim, CompletionTicket,
    CompletionWaiter, CompletionWaiterHandle, PendingCompletion, SharedOperationCache,
    TicketSource, deadline_from, delivery_unknown, operation_digest,
};
pub(crate) use self::credit::{InputCredits, InputStalled, acquire_credit};
use self::encode::{
    LaneEncoderSet, encode_input, encoder_for, handoff_encoded, prune_closed_encoders,
};
pub use self::handle::InputLaneHandle;
pub(crate) use self::handle::arm_pane_lane;
pub(crate) use self::lane::spawn_input_lane;
use self::process::{
    acknowledged_not_written, acknowledged_resource_exhausted, invalid_command,
    process_apply_input, process_attached, process_headless, unsupported_satellite_route,
};
pub(crate) use self::receipt::InputReceipt;
use self::receipt::{ReceiptFailure, ready_receipt, waiting_receipt};
use super::{
    terminal_input_from_event, with_attached_input_destination, with_route_input_destination,
};
#[cfg(doc)]
use crate::input::InputEncoderSnapshot;
use crate::state::{ClientId, SharedState, TerminalInput};
use crate::terminal_actor::{InputCredit, LaneAhead};

/// Bound on the lane's inbound queue. Attached input waits for room
/// ([`InputLaneHandle::route`]); a command refuses with `RESOURCE_EXHAUSTED`.
const INPUT_LANE_CAPACITY: usize = 1024;

/// Leading-edge gap between `input_activity` emissions for one pane.
/// 250 ms is 4 Hz, so a one-second burst emits at most five events.
const INPUT_ACTIVITY_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// Per-pane leading-edge limiter for `input_activity`.
#[derive(Default)]
struct ActivityGate {
    last: std::collections::HashMap<phux_protocol::ids::ResourceId, std::time::Instant>,
}

impl ActivityGate {
    fn admit(&mut self, pane: &phux_protocol::ids::ResourceId, now: std::time::Instant) -> bool {
        if let Some(prev) = self.last.get(pane)
            && now.saturating_duration_since(*prev) < INPUT_ACTIVITY_MIN_INTERVAL
        {
            return false;
        }
        self.last.insert(pane.clone(), now);
        true
    }
}

const fn is_live_input(event: &InputEvent) -> bool {
    !matches!(event, InputEvent::Focus(_))
}

const fn is_live_terminal_input(input: &TerminalInput) -> bool {
    !matches!(input, TerminalInput::Focus(_))
}

fn note_input_activity(
    gate: &mut ActivityGate,
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
) {
    if gate.admit(terminal_id, std::time::Instant::now()) {
        state.with_mut(|server| server.offer_input_activity(terminal_id, client_id));
    }
}

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
    /// Keeps the receive-task PTY write closed until this key is in the
    /// writer queue or has been dropped. `None` for input that never raced
    /// the inline path.
    pub(crate) lane_ahead: Option<LaneAhead>,
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
            lane_ahead: None,
        }
    }

    /// This input, spending `credit` at its handoff.
    #[must_use]
    pub(crate) fn with_credit(mut self, credit: Option<InputCredit>) -> Self {
        self.credit = credit;
        self
    }

    /// This input blocks an inline PTY write until the lane finishes with it.
    #[must_use]
    pub(crate) fn with_lane_ahead(mut self, lane_ahead: Option<LaneAhead>) -> Self {
        self.lane_ahead = lane_ahead;
        self
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

#[cfg(test)]
mod tests;
