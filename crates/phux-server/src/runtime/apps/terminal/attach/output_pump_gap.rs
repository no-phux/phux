//! Output pump gap resync: fence, request, retry, and give up.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::output_pump_resync::LOG_TARGET;
use super::*;

impl OutputPumpContext {
    /// The actor invalidated this pump's capture and the generation is
    /// already tombstoned. Stay fenced and ask for the resync, under the gap
    /// retry budget, so a lost broadcast cannot freeze the client; a closing
    /// pane ends the pump quietly instead.
    pub(super) async fn await_superseded_resync(
        &self,
        generation: &mut PumpGeneration,
    ) -> ControlFlow<Option<PumpFault>> {
        if generation.fence_for_gap() {
            return ControlFlow::Continue(());
        }
        self.send_gap_resync(generation).await
    }

    /// A dropped broadcast window leaves the client's mirror stale: fence the
    /// generation (before the request, even if it fails, so gapped frames are
    /// never forwarded), ask for an in-band resync, and lose the generation if
    /// none can be delivered.
    pub(super) async fn request_gap_resync(
        &self,
        generation: &mut PumpGeneration,
        cause: GapCause,
    ) -> ControlFlow<Option<PumpFault>> {
        crate::perf::PUMP_LAGGED.incr();
        if generation.fence_for_gap() {
            // Under load the answer can be lapped too; that is not an
            // actor that never answers.
            if matches!(cause, GapCause::Dropped(_)) {
                generation.refund_lapped_gap();
            }
            debug!(
                terminal_id = ?self.wire_terminal_id,
                %cause,
                "{} lagged again while a resync was already in flight; waiting",
                self.lag_label,
            );
            return ControlFlow::Continue(());
        } else if matches!(cause, GapCause::Stale(_)) {
            // A slow link goes stale every cycle by design; `pump.lagged`
            // counts it.
            debug!(
                target: LOG_TARGET,
                terminal_id = ?self.wire_terminal_id,
                %cause,
                "{} fell behind; skipping to a fresh checkpoint",
                self.lag_label,
            );
        } else {
            warn!(
                terminal_id = ?self.wire_terminal_id,
                %cause,
                "{} lagged; requesting in-band resync",
                self.lag_label,
            );
        }
        crate::perf::PUMP_GAP_RESYNC.incr();
        self.send_gap_resync(generation).await
    }

    /// Ask the actor for this pump's resync. A gone actor ends the pump
    /// quietly (`RESOURCE_CLOSED` follows); a stuck one loses the generation.
    async fn send_gap_resync(
        &self,
        generation: &mut PumpGeneration,
    ) -> ControlFlow<Option<PumpFault>> {
        generation.note_resync_requested();
        if enqueue_output_resync(&self.resize, self.resync_target(generation)).await {
            return ControlFlow::Continue(());
        }
        if self.resize.is_closed() {
            return ControlFlow::Break(None);
        }
        self.fail_unrecoverable_gap().await
    }

    /// The resync asked for at the last gap has not arrived within
    /// [`pump::GAP_RESYNC_RETRY`]: ask again, or the fenced client would sit
    /// on a screen that never changes.
    pub(super) async fn retry_gap_resync(
        &self,
        generation: &mut PumpGeneration,
    ) -> ControlFlow<Option<PumpFault>> {
        debug!(
            terminal_id = ?self.wire_terminal_id,
            attempt = generation.gap_attempts(),
            "{} is still waiting on its in-band resync; re-requesting",
            self.lag_label,
        );
        self.send_gap_resync(generation).await
    }

    /// The gap spent its whole request budget without the actor ever
    /// broadcasting a replacement generation.
    pub(super) async fn abandon_unanswered_gap(
        &self,
        generation: &PumpGeneration,
    ) -> ControlFlow<Option<PumpFault>> {
        warn!(
            terminal_id = ?self.wire_terminal_id,
            attempts = generation.gap_attempts(),
            "{} never received the in-band resync it asked for; failing the generation",
            self.lag_label,
        );
        self.fail_unrecoverable_gap().await
    }

    /// Tell the client the gap is unrecoverable and end the pump.
    async fn fail_unrecoverable_gap(&self) -> ControlFlow<Option<PumpFault>> {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            self.out_tx.send(Outbound::Frame(FrameKind::Error {
                request_id: None,
                code: ErrorCode::InternalError,
                message: "terminal output gap could not be resynchronized".to_owned(),
            })),
        )
        .await;
        ControlFlow::Break(Some(PumpFault::GenerationLost))
    }
}
