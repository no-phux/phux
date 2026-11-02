//! The sending side of the lane: arming panes and routing input onto the queue.
#![allow(
    clippy::redundant_pub_crate,
    reason = "the connection read loop and command handlers use these through the lane's re-export"
)]

use phux_protocol::ids::InputOperationId;
use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::{CommandResult, ErrorCode};
use phux_terminal::actor::TerminalResource as _;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use crate::state::{ClientId, SharedState};
use phux_terminal::actor::{InputCredit, LaneAhead};

use super::{
    AcknowledgedAdmission, AcknowledgedReservation, CacheClaim, InputReceipt, ReceiptFailure,
    RoutedInput, RoutedInputKind, SharedOperationCache, acknowledged_not_written,
    acknowledged_resource_exhausted, delivery_unknown, invalid_command, operation_digest,
    ready_receipt, unsupported_satellite_route, waiting_receipt,
};

/// Cloneable handle a client task uses to hand input to the lane. Cloning is
/// cheap (`mpsc::Sender` clone); every clone keeps the lane thread alive.
#[derive(Clone, Debug)]
pub struct InputLaneHandle {
    pub(super) tx: mpsc::Sender<RoutedInput>,
    pub(super) admission: Arc<AcknowledgedAdmission>,
    pub(super) cache: SharedOperationCache,
}

/// Count one pane key that is about to enter the lane, whatever admitted
/// it (`INPUT_*`, `ROUTE_INPUT`, or `APPLY_INPUT`). `None` when the fast
/// path is off or this id has no inline writer: nothing can jump the queue.
pub(crate) fn arm_pane_lane(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
) -> Option<LaneAhead> {
    if !phux_perf::echo_hop::fast_path_enabled() {
        return None;
    }
    state.with(|server| {
        let crate::state::Resolved::Local(local) = server.resolve_resource(terminal_id) else {
            return None;
        };
        local
            .handle
            .terminal()
            .ok()?
            .fast_pty
            .as_ref()?
            .upgrade()
            .map(|fast| LaneAhead::arm(&fast))
    })
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
        state: &SharedState,
        client_id: ClientId,
        terminal_id: phux_protocol::ids::ResourceId,
        event: InputEvent,
        credit: Option<InputCredit>,
    ) -> InputReceipt {
        let (reply, result) = oneshot::channel();
        // Arm before the send. A later `INPUT_KEY` on this pane must see the
        // count before this call returns, or it can write ahead of `event`.
        let lane_ahead = arm_pane_lane(state, &terminal_id);
        let routed = RoutedInput {
            client_id,
            terminal_id,
            kind: RoutedInputKind::Headless { event, reply },
            credit,
            lane_ahead,
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
        state: &SharedState,
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
        let lane_ahead = arm_pane_lane(state, &terminal_id);
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
            lane_ahead,
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
