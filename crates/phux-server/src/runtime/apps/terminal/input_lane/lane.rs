//! The lane thread: spawning it, running its loop, and stopping it on drop.
#![allow(
    clippy::redundant_pub_crate,
    reason = "the connection read loop and command handlers use these through the lane's re-export"
)]

use phux_protocol::wire::frame::CommandResult;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::state::SharedState;

use super::{
    ACKNOWLEDGED_COMPLETION_TIMEOUT, AcknowledgedAdmission, ActivityGate, CompletionWaiter,
    CompletionWaiterHandle, INPUT_LANE_CAPACITY, InputLaneHandle, RoutedInput, RoutedInputKind,
    SharedOperationCache, TicketSource, is_live_input, note_input_activity, process_apply_input,
    process_attached, process_headless, prune_closed_encoders,
};

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

/// Spawn the input-lane thread. It exits once every sender is dropped.
///
/// # Errors
///
/// The OS error if the thread cannot be spawned.
pub(crate) fn spawn_input_lane(state: SharedState) -> std::io::Result<InputLane> {
    spawn_input_lane_with_completion_timeout(state, ACKNOWLEDGED_COMPLETION_TIMEOUT)
}

pub(super) fn spawn_input_lane_with_completion_timeout(
    state: SharedState,
    completion_timeout: std::time::Duration,
) -> std::io::Result<InputLane> {
    let (tx, rx) = mpsc::channel::<RoutedInput>(INPUT_LANE_CAPACITY);
    let admission = Arc::new(AcknowledgedAdmission::default());
    // The server's one dedupe record (ADR-0126).
    let cache = SharedOperationCache::new(state.with(|s| s.operation_dedupe().clone()));
    let lane_cache = cache.clone();
    let waiter = CompletionWaiter::spawn(cache.clone())?;
    let waiter_handle = waiter.handle();
    let join = std::thread::Builder::new()
        .name("phux-input-lane".to_owned())
        .spawn(move || {
            run_input_lane(&state, rx, &lane_cache, &waiter_handle, completion_timeout);
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

/// Encode and hand off one lane queue. Synchronous and non-blocking, so no
/// pane can stall another pane's input.
fn run_input_lane(
    state: &SharedState,
    mut rx: mpsc::Receiver<RoutedInput>,
    lane_cache: &SharedOperationCache,
    waiter_handle: &CompletionWaiterHandle,
    completion_timeout: std::time::Duration,
) {
    crate::perf::promote_helper_thread("phux-input-lane");
    let mut encoders = std::collections::HashMap::new();
    let mut tickets = TicketSource::default();
    let mut activity_gate = ActivityGate::default();
    while let Some(routed) = rx.blocking_recv() {
        phux_perf::echo_hop::record("lane");
        prune_closed_encoders(&mut encoders);
        match routed.kind {
            RoutedInputKind::Attached { input, frame_label } => {
                if process_attached(
                    state,
                    &mut encoders,
                    routed.client_id,
                    &routed.terminal_id,
                    &input,
                    frame_label,
                    routed.credit,
                    routed.lane_ahead,
                ) {
                    note_input_activity(
                        &mut activity_gate,
                        state,
                        routed.client_id,
                        &routed.terminal_id,
                    );
                }
            }
            RoutedInputKind::Headless { event, reply } => {
                let live = is_live_input(&event);
                let result = process_headless(
                    state,
                    &mut encoders,
                    routed.client_id,
                    &routed.terminal_id,
                    event,
                    routed.credit,
                    routed.lane_ahead,
                );
                if live && matches!(result, CommandResult::Ok) {
                    note_input_activity(
                        &mut activity_gate,
                        state,
                        routed.client_id,
                        &routed.terminal_id,
                    );
                }
                let _ = reply.send(result);
            }
            RoutedInputKind::Acknowledged {
                operation_id,
                events,
                reservation,
                reply,
            } => {
                let live = events.iter().any(is_live_input);
                if process_apply_input(
                    state,
                    &mut encoders,
                    lane_cache,
                    waiter_handle,
                    tickets.next_ticket(),
                    completion_timeout,
                    routed.client_id,
                    operation_id,
                    &routed.terminal_id,
                    events,
                    reservation,
                    reply,
                    routed.lane_ahead,
                ) && live
                {
                    note_input_activity(
                        &mut activity_gate,
                        state,
                        routed.client_id,
                        &routed.terminal_id,
                    );
                }
            }
        }
    }
    tracing::debug!("input lane thread exiting (channel closed)");
}
