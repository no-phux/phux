//! The output pump loop and its fault handling.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;
use phux_terminal::actor::TerminalResource as _;

/// Why a pump gave up on its generation and asked for a resync.
#[derive(Debug, Clone, Copy)]
pub(super) enum GapCause {
    /// The pane's broadcast overwrote this many chunks under the pump.
    Dropped(u64),
    /// The chunk in hand was read from the pane this long ago, past
    /// [`pump::STALE_OUTPUT_BUDGET`].
    Stale(std::time::Duration),
    /// The consumer mailbox was full.
    Backpressure,
}

impl std::fmt::Display for GapCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dropped(n) => write!(f, "broadcast dropped {n} chunks"),
            Self::Stale(age) => write!(f, "chunk {}ms old", age.as_millis()),
            Self::Backpressure => write!(f, "consumer mailbox full"),
        }
    }
}

/// A final drain wins over ready broadcast traffic and expired gap retries.
pub(super) async fn next_wait(
    ctx: &OutputPumpContext,
    generation: &PumpGeneration,
    output_rx: &mut tokio::sync::broadcast::Receiver<PaneOutput>,
) -> Option<pump::PumpWait> {
    async fn cancelled(token: Option<&CancellationToken>) {
        match token {
            Some(token) => token.cancelled().await,
            None => std::future::pending().await,
        }
    }
    tokio::select! {
        biased;
        () = cancelled(ctx.drain.as_ref()) => Some(pump::PumpWait::FinalDrain),
        wait = pump::next_event(generation, output_rx) => Some(wait),
        () = cancelled(ctx.cancel.as_ref()) => None,
    }
}

/// Drive one client's output subscription for a pane: park on the publication
/// gate, then run [`run_started_output_pump`]. Returns the fault that ended
/// it, `None` if orderly.
pub(super) async fn run_output_pump(
    ctx: &OutputPumpContext,
    gate_rx: oneshot::Receiver<OutputPumpStart>,
    output_rx: tokio::sync::broadcast::Receiver<PaneOutput>,
) -> Option<PumpFault> {
    let start = gate_rx.await.ok()?;
    run_started_output_pump(ctx, start, output_rx).await
}

/// Replay the backlog behind the published cut, then forward live output,
/// ordered control, and generation replacements until the pane or the client
/// goes away.
pub(crate) async fn run_started_output_pump(
    ctx: &OutputPumpContext,
    start: OutputPumpStart,
    mut output_rx: tokio::sync::broadcast::Receiver<PaneOutput>,
) -> Option<PumpFault> {
    let mut generation = PumpGeneration::opened_at(start.published_cut, ctx.initial_bootstrap_id);
    if let Some(live) = start.live {
        output_rx = live;
    }
    if let Err(fault) = ctx
        .forward_gated_replay(&mut generation, start.replay)
        .await
    {
        return Some(fault);
    }
    ctx.publish_last_seq(&generation);
    // The first live chunk waited behind the attach bootstrap and its replay.
    generation.restart_staleness_clock();
    loop {
        let received = match next_wait(ctx, &generation, &mut output_rx).await? {
            pump::PumpWait::FinalDrain => {
                return ctx
                    .publish_final_generation(&mut generation, &mut output_rx)
                    .await
                    .err();
            }
            pump::PumpWait::Event(received) => received,
            pump::PumpWait::RetryResync => {
                if let ControlFlow::Break(fault) = ctx.retry_gap_resync(&mut generation).await {
                    return fault;
                }
                continue;
            }
            pump::PumpWait::GapUnrecoverable => {
                if let ControlFlow::Break(fault) = ctx.abandon_unanswered_gap(&generation).await {
                    return fault;
                }
                continue;
            }
        };
        if let ControlFlow::Break(fault) =
            dispatch_output(ctx, &mut generation, &mut output_rx, received).await
        {
            return fault;
        }
    }
}

/// Handle one broadcast result for this pump's generation.
async fn dispatch_output(
    ctx: &OutputPumpContext,
    generation: &mut PumpGeneration,
    output_rx: &mut tokio::sync::broadcast::Receiver<PaneOutput>,
    received: Result<PaneOutput, tokio::sync::broadcast::error::RecvError>,
) -> ControlFlow<Option<PumpFault>> {
    match received {
        Ok(PaneOutput::Live { seq, bytes, at }) => {
            let age = generation.chunk_age(at);
            if ctx.stale_skip && pump::is_stale(age) && generation.forwards(seq) {
                // Skip a slow consumer to a fresh checkpoint rather than
                // replay a backlog that only grows.
                ctx.request_gap_resync(generation, GapCause::Stale(age))
                    .await
            } else {
                ctx.forward_live(generation, seq, &bytes).await
            }
        }
        Ok(PaneOutput::Control { owner, frame }) => {
            ctx.forward_control(generation, owner, frame).await
        }
        Ok(PaneOutput::Resync {
            cols,
            rows,
            bytes,
            reason,
            audience,
            base_seq,
        }) => {
            ctx.apply_resync(
                generation,
                output_rx,
                &audience,
                &PaneResync {
                    cols,
                    rows,
                    bytes,
                    reason,
                    base_seq,
                },
            )
            .await
        }
        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
            ctx.request_gap_resync(generation, GapCause::Dropped(n))
                .await
        }
        Err(tokio::sync::broadcast::error::RecvError::Closed) => ControlFlow::Break(None),
    }
}

/// Release a shared pane's consumer after its pump failed. The pane itself
/// belongs to others: an unrecoverable generation detaches only this client
/// and closes its connection.
pub(crate) fn release_after_pump_fault(
    fault: PumpFault,
    state: &SharedState,
    client_id: ClientId,
    connection_token: &CancellationToken,
) {
    match fault {
        PumpFault::OutboundClosed | PumpFault::ReplayAbandoned | PumpFault::PaneGone => {}
        PumpFault::TombstoneNotQueued | PumpFault::GenerationLost => {
            warn!(?client_id, ?fault, "attach pump failed; closing the client");
            crate::runtime::client::detach_and_release_consumer_state(state, client_id);
            connection_token.cancel();
        }
        PumpFault::PublicationNotActivated => {
            warn!(
                ?client_id,
                "attach pump publication not activated; closing the client"
            );
            connection_token.cancel();
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "single rollback boundary deliberately receives every staged and committed resource so cancellation, producer detach, pump abortion, and the fatal sentinel remain strictly ordered"
)]
pub(super) async fn fail_aggregate_attach_prepublication(
    state: &SharedState,
    client_id: ClientId,
    attach_id: u32,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    connection_token: &CancellationToken,
    staged_handles: &[ResourceHandle],
    staged_pumps: &mut JoinSet<()>,
    committed_pumps: &mut JoinSet<()>,
    reason: &str,
) {
    staged_pumps.abort_all();
    while staged_pumps.join_next().await.is_some() {}
    crate::runtime::client::abort_output_pumps(committed_pumps, client_id, "failed ATTACH").await;

    let wire_client_id = crate::runtime::wire_client(client_id);
    let producer_deadline = std::time::Duration::from_secs(1);
    for handle in staged_handles {
        // Native history cuts are a Terminal facet lease.
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if let Ok(terminal) = handle.terminal() {
            let _ = tokio::time::timeout(
                producer_deadline,
                terminal
                    .native_release
                    .send(phux_terminal::actor::NativeReleaseRequest { owner: client_id.0 }),
            )
            .await;
        }
        let Ok(terminal) = handle.terminal() else {
            continue;
        };
        let (reply, done) = oneshot::channel();
        if matches!(
            tokio::time::timeout(
                producer_deadline,
                terminal.consumer_detach.send(ConsumerDetachRequest {
                    client_id: wire_client_id,
                    reply,
                }),
            )
            .await,
            Ok(Ok(()))
        ) {
            let _ = tokio::time::timeout(producer_deadline, done).await;
        }
    }
    crate::runtime::client::detach_and_release_consumer_state(state, client_id);

    // The writer closes right after this terminal ERROR, discarding anything
    // a surviving producer races in behind it.
    if !matches!(
        tokio::time::timeout(
            producer_deadline,
            out_tx.send(Outbound::TerminalError {
                request_id: None,
                code: ErrorCode::CodecUnavailable,
                message: format!("ATTACH {attach_id} failed before publication: {reason}"),
            }),
        )
        .await,
        Ok(Ok(()))
    ) {
        warn!(client = ?client_id, attach_id, "failed to enqueue terminal ATTACH error");
    }
    connection_token.cancel();
}
