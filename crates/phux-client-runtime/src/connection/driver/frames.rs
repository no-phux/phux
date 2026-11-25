//! Native lock handoff and engine execution for one ordered transport batch.

use bytes::Bytes;
use phux_protocol::wire::frame::{CommandResult, CommandValue, FrameKind};

use super::super::{Shared, lock, trace};
use crate::control::frames::stepped::{BatchStep, FrameBatch};
use crate::control::{ControlError, ControlPlane, Status};

/// Execute each handed-out engine batch once, with no control lock held.
/// Borrowed socket payloads and contiguous engine batching stay unchanged.
pub(crate) fn feed_shared_unlocked(
    shared: &Shared,
    frames: Vec<Bytes>,
) -> Result<(), ControlError> {
    drive(shared, FrameBatch::new(frames.into_iter(), |plane, bytes| {
        plane.decode_shared_frame(&bytes)
    }))
}

/// The embedded byte lane uses the same native lock-release protocol.
pub(crate) fn feed_bytes_unlocked(shared: &Shared, bytes: &[u8]) -> Result<(), ControlError> {
    drive(shared, FrameBatch::new(std::iter::once(bytes), ControlPlane::decode_frame))
}

/// Already-decoded embedded frames keep their owned payload without copying.
pub(crate) fn feed_unlocked(shared: &Shared, frame: FrameKind) -> Result<(), ControlError> {
    drive(shared, FrameBatch::new(std::iter::once(frame), |_, frame| Ok(Some(frame))))
}

fn drive(shared: &Shared, mut batch: FrameBatch<impl Iterator>) -> Result<(), ControlError> {
    let mut step = {
        let mut plane = lock(shared);
        plane.observe_time(std::time::Instant::now());
        let step = plane.step_frame_batch(&mut batch);
        advance_frames(&mut plane, &mut batch, step)
    };
    loop {
        match step {
            BatchStep::Done(result) => return result,
            BatchStep::Apply(apply) => {
                let outcomes = apply.engine.apply_batch(apply.events);
                let mut plane = lock(shared);
                plane.observe_time(std::time::Instant::now());
                let completed = plane.complete_frame_apply(&mut batch, apply.geometry, outcomes);
                step = advance_frames(&mut plane, &mut batch, completed);
            }
            BatchStep::Frame(_) => unreachable!("advance_frames consumes semantic frames"),
        }
    }
}

fn advance_frames(
    plane: &mut ControlPlane,
    batch: &mut FrameBatch<impl Iterator>,
    mut step: BatchStep,
) -> BatchStep {
    while let BatchStep::Frame(frame) = step {
        let hello = matches!(frame, FrameKind::HelloOk { .. });
        let topology = matches!(
            &frame,
            FrameKind::CommandResult {
                request_id, result: CommandResult::OkWith(CommandValue::State(_)),
            } if plane.expects_topology_reply(*request_id)
        );
        let attach = matches!(frame, FrameKind::AttachReady { .. });
        if hello {
            trace::phase("hello_received");
        }
        if topology {
            trace::phase("topology_received");
        }
        let result = plane.feed(frame);
        if result.is_ok() {
            if hello && plane.server().is_some() {
                trace::phase("hello_accepted");
            }
            if topology && plane.topology().is_some() {
                trace::phase("topology_applied");
            }
            if attach && plane.status() == Status::Attached {
                trace::phase("attach_ready");
            }
        }
        step = plane.complete_frame_feed(batch, result);
    }
    step
}

#[cfg(test)]
mod tests;
