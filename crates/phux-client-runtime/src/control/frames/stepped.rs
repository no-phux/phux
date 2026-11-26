//! Inbound batches as resumable steps.
//!
//! Applying a run of engine events is a blocking round trip to the engine
//! owner thread, and a reconnect's snapshot replay makes that run as large
//! as it ever gets. A caller that holds the control lock across it stalls
//! every other reader of the plane, a consumer's UI thread draining
//! observations included. Stepping hands each run back to the caller, which
//! releases the lock for the round trip and re-takes it to complete the run.

#![allow(
    clippy::redundant_pub_crate,
    reason = "crate-wide items in a private module: `pub` trips `unreachable_pub` instead"
)]

use phux_client_core::session::KernelInput;

pub(crate) use super::super::kernel::EngineApply;
use super::super::{ControlError, ControlPlane, EngineEvent, EngineOutcome, FrameKind, ResourceId};
use super::allowed_before_handshake;

/// One transport read's frames, part way through being fed.
pub(crate) struct FrameBatch<I: Iterator> {
    frames: I,
    decode: DecodeFrame<I::Item>,
    engine_events: Vec<EngineEvent>,
    deferred_error: Option<ControlError>,
    /// What the in-flight run was flushed for.
    after: Option<AfterApply>,
    /// The connection the in-flight run was handed out on.
    epoch: Option<u64>,
}

type DecodeFrame<F> = fn(&ControlPlane, F) -> Result<Option<FrameKind>, ControlError>;

impl<I: Iterator> FrameBatch<I> {
    pub(crate) const fn new(frames: I, decode: DecodeFrame<I::Item>) -> Self {
        Self {
            frames,
            decode,
            engine_events: Vec::new(),
            deferred_error: None,
            after: None,
            epoch: None,
        }
    }
}

/// Why a run of engine events was flushed: the frame that ends it, a decode
/// failure, or the end of the read.
enum AfterApply {
    Feed(FrameKind),
    Fail(ControlError),
    Finish,
}

#[allow(
    clippy::large_enum_variant,
    reason = "owned frames stay allocation-free between steps"
)]
pub(crate) enum BatchStep {
    /// Let the driver execute one semantic frame, with native tracing outside it.
    Frame(FrameKind),
    /// Apply this run on the engine, without the control lock, then hand
    /// the outcomes to [`ControlPlane::complete_frame_apply`].
    Apply(EngineApply),
    Done(Result<(), ControlError>),
}

impl ControlPlane {
    /// Finish a non-engine frame before resuming the same ordered batch.
    pub(crate) fn complete_frame_feed(
        &mut self,
        batch: &mut FrameBatch<impl Iterator>,
        result: Result<(), ControlError>,
    ) -> BatchStep {
        match Self::continue_after_nonfatal(&mut batch.deferred_error, result) {
            Ok(()) => self.step_frame_batch(batch),
            Err(error) => BatchStep::Done(Err(error)),
        }
    }

    /// Feed frames until a run of engine events must be applied, or the
    /// batch is done.
    pub(crate) fn step_frame_batch(&mut self, batch: &mut FrameBatch<impl Iterator>) -> BatchStep {
        loop {
            let Some(raw) = batch.frames.next() else {
                return self.flush_then(batch, AfterApply::Finish);
            };
            let frame = match (batch.decode)(self, raw) {
                Ok(Some(frame)) => frame,
                Ok(None) => continue,
                Err(error) => return self.flush_then(batch, AfterApply::Fail(error)),
            };
            if !self.handshake_ready && !allowed_before_handshake(&frame) {
                let error =
                    ControlError::Protocol("server frame arrived before HELLO_OK".to_owned());
                return self.flush_then(batch, AfterApply::Fail(error));
            }
            if KernelInput::from_stream_frame(&frame).is_none() {
                return self.flush_then(batch, AfterApply::Feed(frame));
            }
            batch.engine_events.push(EngineEvent::Stream(frame));
        }
    }

    /// Fold an applied run's outcomes in and keep feeding. A connection that
    /// ended while the engine worked (the consumer closed it) keeps none of
    /// what the run built.
    pub(crate) fn complete_frame_apply(
        &mut self,
        batch: &mut FrameBatch<impl Iterator>,
        geometry: Vec<(usize, ResourceId, bool)>,
        outcomes: Result<Vec<EngineOutcome>, crate::engine::EngineError>,
    ) -> BatchStep {
        let after = batch.after.take().unwrap_or(AfterApply::Finish);
        if self.live_connection_epoch != batch.epoch {
            // Retirement/reset is ordered by the native owner. Resetting here
            // could erase a replacement connection that opened during the apply.
            return BatchStep::Done(Ok(()));
        }
        let flushed = self.complete_engine_apply(geometry, outcomes);
        Self::resolve_flush(batch, after, flushed)
    }

    /// Hand out the pending run, or resolve `after` at once when there is
    /// none.
    fn flush_then(
        &mut self,
        batch: &mut FrameBatch<impl Iterator>,
        after: AfterApply,
    ) -> BatchStep {
        if batch.engine_events.is_empty() {
            return Self::resolve_flush(batch, after, Ok(()));
        }
        match self.prepare_engine_apply(std::mem::take(&mut batch.engine_events)) {
            Ok(apply) => {
                batch.after = Some(after);
                batch.epoch = self.live_connection_epoch;
                BatchStep::Apply(apply)
            }
            Err(error) => Self::resolve_flush(batch, after, Err(error)),
        }
    }

    /// The flushed run's result, then whatever it was flushed for, with the
    /// non-fatal deferral a batch feed applies.
    fn resolve_flush(
        batch: &mut FrameBatch<impl Iterator>,
        after: AfterApply,
        flushed: Result<(), ControlError>,
    ) -> BatchStep {
        let deferred = &mut batch.deferred_error;
        let done = match after {
            AfterApply::Feed(frame) => match Self::continue_after_nonfatal(deferred, flushed) {
                Ok(()) => return BatchStep::Frame(frame),
                Err(error) => Err(error),
            },
            AfterApply::Fail(error) => match Self::continue_after_nonfatal(deferred, flushed) {
                Err(pending) => Err(ControlError::prefer(Some(pending), error)),
                Ok(()) => Err(ControlError::prefer(deferred.take(), error)),
            },
            AfterApply::Finish => Self::continue_after_nonfatal(deferred, flushed)
                .and_then(|()| deferred.take().map_or(Ok(()), Err)),
        };
        BatchStep::Done(done)
    }
}
