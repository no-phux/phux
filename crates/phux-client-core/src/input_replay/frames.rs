//! Building `APPLY_INPUT` attempts and rolling back unsent ones.

use std::collections::HashSet;

use phux_protocol::wire::frame::{Command, FrameKind};

#[cfg(not(target_arch = "wasm32"))]
use super::monotonic_now_ms;
use super::{InFlightAttempt, InputReplayJournal, ReplayReport};

impl InputReplayJournal {
    /// Build the next `APPLY_INPUT` attempt, if one is due.
    ///
    /// Serialized: nothing is built while an attempt is outstanding. Expired
    /// operations encountered at the front strand (reported) rather than
    /// being sent past the server's dedupe retention. The returned frame has
    /// already been recorded as in flight under a request id drawn from
    /// `next_request_id` — the caller's only obligation is to put it on the
    /// wire (a send failure ends in [`Self::connection_lost`] anyway).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn next_frame(
        &mut self,
        next_request_id: &mut u32,
    ) -> (Vec<ReplayReport>, Option<FrameKind>) {
        self.next_frame_at(next_request_id, monotonic_now_ms())
    }

    /// Caller-clock single-frame entry point, available on every target.
    pub fn next_frame_at(
        &mut self,
        next_request_id: &mut u32,
        now_ms: u64,
    ) -> (Vec<ReplayReport>, Option<FrameKind>) {
        let (reports, mut frames) = self.build_frames(next_request_id, 1, now_ms);
        (reports, frames.pop())
    }

    /// Build one due attempt per Terminal. Independent terminals can therefore
    /// progress while an earlier operation waits for its PTY outcome.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn next_frames(
        &mut self,
        next_request_id: &mut u32,
    ) -> (Vec<ReplayReport>, Vec<FrameKind>) {
        self.next_frames_at(next_request_id, monotonic_now_ms())
    }

    /// Caller-clock multi-frame entry point, available on every target.
    pub fn next_frames_at(
        &mut self,
        next_request_id: &mut u32,
        now_ms: u64,
    ) -> (Vec<ReplayReport>, Vec<FrameKind>) {
        self.build_frames(next_request_id, usize::MAX, now_ms)
    }

    /// Roll back a suffix returned by [`Self::next_frames_at`] that the caller
    /// never handed to its connection.
    ///
    /// A sequential sender must exclude the frame whose `send` returned an
    /// error: that handoff is uncertain. Only later frames are provably
    /// unsent. Rolling those back removes their connection-local request ids
    /// and reverses this build's attempt, while preserving any earlier attempt
    /// (and therefore its unknown-delivery risk) across a replay.
    pub fn rollback_unsent(&mut self, frames: &[FrameKind]) {
        for frame in frames {
            let FrameKind::Command { request_id, .. } = frame else {
                continue;
            };
            let Some(attempt) = self
                .connection
                .as_mut()
                .and_then(|ctx| ctx.in_flight.remove(request_id))
            else {
                continue;
            };
            let Some(op) = self
                .ops
                .iter_mut()
                .find(|op| op.operation_id == attempt.operation_id)
            else {
                continue;
            };
            op.attempts = op.attempts.saturating_sub(1);
            if op.attempts == 0 {
                op.expected_server_id = None;
            }
        }
    }

    pub(super) fn build_frames(
        &mut self,
        next_request_id: &mut u32,
        limit: usize,
        now_ms: u64,
    ) -> (Vec<ReplayReport>, Vec<FrameKind>) {
        let mut reports = self.take_reports();
        reports.extend(self.sweep_expired(now_ms));
        let Some(ctx) = self.connection.as_mut() else {
            return (reports, Vec::new());
        };
        let server_id = ctx.server_id.clone();
        let mut busy: HashSet<_> = ctx
            .in_flight
            .values()
            .map(|attempt| attempt.terminal_id.clone())
            .collect();
        let mut frames = Vec::new();
        for op in &mut self.ops {
            if frames.len() == limit || !busy.insert(op.terminal_id.clone()) {
                continue;
            }
            let request_id = *next_request_id;
            *next_request_id = next_request_id.wrapping_add(1);
            op.attempts = op.attempts.saturating_add(1);
            op.expected_server_id
                .get_or_insert_with(|| server_id.clone());
            frames.push(FrameKind::Command {
                request_id,
                command: Command::ApplyInput {
                    operation_id: op.operation_id,
                    terminal_id: op.terminal_id.clone(),
                    events: op.events.clone(),
                },
            });
            ctx.in_flight.insert(
                request_id,
                InFlightAttempt {
                    operation_id: op.operation_id,
                    terminal_id: op.terminal_id.clone(),
                },
            );
        }
        (reports, frames)
    }
}
