//! [`ControlPlane::feed_shared_batch`] as resumable steps.
//!
//! Applying a run of engine events is a blocking round trip to the engine
//! owner thread, and a reconnect's snapshot replay makes that run as large
//! as it ever gets. A caller that holds the control lock across it stalls
//! every other reader of the plane, a consumer's UI thread draining
//! observations included. Stepping hands each run back to the caller, which
//! releases the lock for the round trip and re-takes it to complete the run.

use bytes::Bytes;

use crate::connection::{Shared, lock};

use super::super::kernel::EngineApply;
use super::super::{ControlError, ControlPlane, EngineEvent, EngineOutcome, FrameKind, ResourceId};
use super::allowed_before_handshake;
use super::classify::{ClassifiedFrame, classify_engine_frame};

/// One transport read's frames, part way through being fed.
pub(super) struct SharedBatch {
    frames: std::vec::IntoIter<Bytes>,
    engine_events: Vec<EngineEvent>,
    deferred_error: Option<ControlError>,
    /// What the in-flight run was flushed for.
    after: Option<AfterApply>,
    /// The connection the in-flight run was handed out on.
    epoch: Option<u64>,
}

impl SharedBatch {
    pub(super) fn new(frames: Vec<Bytes>) -> Self {
        Self {
            frames: frames.into_iter(),
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

pub(super) enum BatchStep {
    /// Apply this run on the engine, without the control lock, then hand
    /// the outcomes to [`ControlPlane::complete_shared_apply`].
    Apply(EngineApply),
    Done(Result<(), ControlError>),
}

impl ControlPlane {
    /// [`Self::feed_shared_batch`] for the connection driver: the control
    /// lock is released across each engine round trip and re-taken to fold
    /// its outcomes in.
    pub(crate) fn feed_shared_unlocked(
        shared: &Shared,
        frames: Vec<Bytes>,
    ) -> Result<(), ControlError> {
        let mut batch = SharedBatch::new(frames);
        let mut step = lock(shared).step_shared_batch(&mut batch);
        loop {
            match step {
                BatchStep::Done(result) => return result,
                BatchStep::Apply(apply) => {
                    let outcomes = apply.engine.apply_batch(apply.events);
                    step = lock(shared).complete_shared_apply(&mut batch, apply.geometry, outcomes);
                }
            }
        }
    }

    /// Feed frames until a run of engine events must be applied, or the
    /// batch is done.
    pub(super) fn step_shared_batch(&mut self, batch: &mut SharedBatch) -> BatchStep {
        loop {
            let Some(bytes) = batch.frames.next() else {
                return self
                    .flush_then(batch, AfterApply::Finish)
                    .unwrap_or(BatchStep::Done(Ok(())));
            };
            let frame = match self.decode_shared_frame(&bytes) {
                Ok(Some(frame)) => frame,
                Ok(None) => continue,
                Err(error) => {
                    return self
                        .flush_then(batch, AfterApply::Fail(error))
                        .unwrap_or(BatchStep::Done(Ok(())));
                }
            };
            if !self.handshake_ready && !allowed_before_handshake(&frame) {
                let error =
                    ControlError::Protocol("server frame arrived before HELLO_OK".to_owned());
                return self
                    .flush_then(batch, AfterApply::Fail(error))
                    .unwrap_or(BatchStep::Done(Ok(())));
            }
            match classify_engine_frame(frame) {
                ClassifiedFrame::Engine(event) => batch.engine_events.push(event),
                ClassifiedFrame::Other(frame) => {
                    if let Some(step) = self.flush_then(batch, AfterApply::Feed(frame)) {
                        return step;
                    }
                }
            }
        }
    }

    /// Fold an applied run's outcomes in and keep feeding. A connection that
    /// ended while the engine worked (the consumer closed it) keeps none of
    /// what the run built.
    pub(super) fn complete_shared_apply(
        &mut self,
        batch: &mut SharedBatch,
        geometry: Vec<(usize, ResourceId, bool)>,
        outcomes: Result<Vec<EngineOutcome>, crate::engine::EngineError>,
    ) -> BatchStep {
        let after = batch.after.take().unwrap_or(AfterApply::Finish);
        if self.live_connection_epoch != batch.epoch {
            if let Some(engine) = &self.engine {
                engine.reset_connection();
            }
            return BatchStep::Done(Ok(()));
        }
        let flushed = self.complete_engine_apply(geometry, outcomes);
        self.resolve_flush(batch, after, flushed)
            .unwrap_or_else(|| self.step_shared_batch(batch))
    }

    /// Hand out the pending run, or resolve `after` at once when there is
    /// none. `None` means keep feeding.
    fn flush_then(&mut self, batch: &mut SharedBatch, after: AfterApply) -> Option<BatchStep> {
        if batch.engine_events.is_empty() {
            return self.resolve_flush(batch, after, Ok(()));
        }
        match self.prepare_engine_apply(std::mem::take(&mut batch.engine_events)) {
            Ok(apply) => {
                batch.after = Some(after);
                batch.epoch = self.live_connection_epoch;
                Some(BatchStep::Apply(apply))
            }
            Err(error) => self.resolve_flush(batch, after, Err(error)),
        }
    }

    /// The flushed run's result, then whatever it was flushed for, with the
    /// non-fatal deferral `feed_batch` applies.
    fn resolve_flush(
        &mut self,
        batch: &mut SharedBatch,
        after: AfterApply,
        flushed: Result<(), ControlError>,
    ) -> Option<BatchStep> {
        let deferred = &mut batch.deferred_error;
        let done = match after {
            AfterApply::Feed(frame) => {
                let fed = Self::continue_after_nonfatal(deferred, flushed)
                    .and_then(|()| Self::continue_after_nonfatal(deferred, self.feed(frame)));
                if fed.is_ok() {
                    return None;
                }
                fed
            }
            AfterApply::Fail(error) => match Self::continue_after_nonfatal(deferred, flushed) {
                Err(pending) => Err(ControlError::prefer(Some(pending), error)),
                Ok(()) => Err(ControlError::prefer(deferred.take(), error)),
            },
            AfterApply::Finish => Self::continue_after_nonfatal(deferred, flushed)
                .and_then(|()| deferred.take().map_or(Ok(()), Err)),
        };
        Some(BatchStep::Done(done))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use phux_protocol::PROTOCOL_VERSION;
    use phux_protocol::caps::{
        BootstrapCapabilities, ServerCapabilities, select_bootstrap_profile,
    };
    use phux_protocol::ids::{BootstrapId, StreamId};

    use super::*;
    use crate::control::{ControlOptions, encode};

    fn negotiated() -> ControlPlane {
        let mut plane = ControlPlane::new(ControlOptions::default());
        plane.connection_opened();
        let hello = plane.take_outbound();
        let (FrameKind::Hello { client_caps, .. }, _) =
            FrameKind::decode(&hello[0]).expect("HELLO decodes")
        else {
            panic!("the opening frame is HELLO");
        };
        let (profile, limits) =
            select_bootstrap_profile(&client_caps, &BootstrapCapabilities::new())
                .expect("intersect");
        plane
            .feed(FrameKind::HelloOk {
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new(),
                server_id: b"incarnation".to_vec(),
                selected_profile: Some(profile),
                bootstrap_limits: limits,
            })
            .expect("HELLO_OK");
        assert!(plane.engine().is_some(), "HELLO_OK starts the engine");
        plane
    }

    /// Output for a terminal this connection never bootstrapped: a run the
    /// engine applies and answers with an error.
    fn stray_output() -> Bytes {
        Bytes::from(encode(&FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(7),
            stream_id: StreamId::new(1).expect("stream"),
            bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
            seq: 1,
            bytes: b"late".to_vec().into(),
        }))
    }

    fn handed_out(shared: &Shared) -> (SharedBatch, EngineApply) {
        let mut batch = SharedBatch::new(vec![stray_output()]);
        let BatchStep::Apply(apply) = lock(shared).step_shared_batch(&mut batch) else {
            panic!("an engine run is handed out");
        };
        (batch, apply)
    }

    /// The point of stepping: while a run is with the engine, the plane is
    /// free for every other reader.
    #[test]
    fn a_run_is_with_the_engine_while_the_plane_is_free() {
        let shared: Shared = Arc::new(Mutex::new(negotiated()));
        let (mut batch, apply) = handed_out(&shared);
        let drained = shared
            .try_lock()
            .map(|mut plane| plane.take_observation())
            .is_ok();
        assert!(drained, "observation drains without waiting on the engine");
        let outcomes = apply.engine.apply_batch(apply.events);
        let BatchStep::Done(_) =
            lock(&shared).complete_shared_apply(&mut batch, apply.geometry, outcomes)
        else {
            panic!("a one-frame read is done after its run");
        };
    }

    /// A consumer that closes while the engine applies a run keeps none of
    /// it: the run belonged to a connection that is gone.
    #[test]
    fn a_close_during_the_apply_keeps_none_of_the_run() {
        let open: Shared = Arc::new(Mutex::new(negotiated()));
        let (mut batch, apply) = handed_out(&open);
        let outcomes = apply.engine.apply_batch(apply.events);
        let BatchStep::Done(still_open) =
            lock(&open).complete_shared_apply(&mut batch, apply.geometry, outcomes)
        else {
            panic!("done");
        };
        assert!(
            still_open.is_err(),
            "an open connection hears the stray run's error"
        );

        let closed: Shared = Arc::new(Mutex::new(negotiated()));
        let (mut batch, apply) = handed_out(&closed);
        lock(&closed).close();
        let _ = lock(&closed).take_events();
        let outcomes = apply.engine.apply_batch(apply.events);
        let BatchStep::Done(result) =
            lock(&closed).complete_shared_apply(&mut batch, apply.geometry, outcomes)
        else {
            panic!("done");
        };
        assert!(
            result.is_ok(),
            "a closed connection's run is discarded: {result:?}"
        );
        assert!(lock(&closed).take_events().is_empty(), "and emits nothing");
    }
}
