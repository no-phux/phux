//! Publish a liveness probe's round trip when inbound traffic answers it.

use super::{Pump, lock};
use tokio::time::Instant;

impl Pump<'_> {
    /// Inbound traffic answers an in-flight probe: clear its deadline and
    /// publish the round trip, QUIC's own estimate or else the time since
    /// the probe went out.
    // ponytail: off QUIC any inbound frame answers the probe, so a busy
    // session reads low; match the PONG nonce if that matters.
    pub(super) fn answer_probe(&mut self) {
        let Some(deadline) = self.probe_deadline.take() else {
            return;
        };
        let sent = deadline.deadline().checked_sub(self.options.probe_timeout);
        let measured = self
            .io
            .round_trip()
            .or_else(|| Some(Instant::now() - sent?));
        if let Some(round_trip) = measured {
            lock(self.shared).note_round_trip(round_trip);
        }
    }
}
