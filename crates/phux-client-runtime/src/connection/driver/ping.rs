//! Settle a liveness probe from the inbound traffic that follows it.

use bytes::Bytes;
use phux_protocol::wire::frame::{FrameKind, TYPE_PONG};
use phux_protocol::wire::framing::LENGTH_PREFIX_LEN;
use tokio::time::Instant;

use super::{Pump, lock};
use crate::connection::io::PROBE_NONCE;

impl Pump<'_> {
    /// Only the probe's PONG answers it, or an empty frame (a WebSocket
    /// control frame, the lane's pong). Frames the peer sent before the
    /// probe can still land after it on a link that just died, so other
    /// traffic only pushes the deadline out: a live link draining a backlog
    /// ahead of the PONG stays up, and a dead one drops a timeout after its
    /// last stale frame (phux-kjoap).
    pub(super) fn settle_probe(&mut self, frames: &[Bytes]) {
        let answered = frames
            .iter()
            .any(|frame| frame.is_empty() || is_probe_pong(frame));
        if answered {
            self.answer_probe();
        } else if let Some(deadline) = self.probe_deadline.as_mut() {
            deadline
                .as_mut()
                .reset(Instant::now() + self.options.probe_timeout);
        }
    }

    /// Clear the probe's deadline and publish the round trip, QUIC's own
    /// estimate or else the time since the probe went out.
    fn answer_probe(&mut self) {
        if self.probe_deadline.take().is_none() {
            return;
        }
        let measured = self
            .io
            .round_trip()
            .unwrap_or_else(|| self.probe_sent.elapsed());
        lock(self.shared).note_round_trip(measured);
    }
}

/// Whether `frame` is the probe's `PONG`; only a `PONG`-typed frame is decoded.
fn is_probe_pong(frame: &[u8]) -> bool {
    frame.get(LENGTH_PREFIX_LEN) == Some(&TYPE_PONG)
        && matches!(
            FrameKind::decode(frame),
            Ok((FrameKind::Pong { nonce: PROBE_NONCE }, []))
        )
}
