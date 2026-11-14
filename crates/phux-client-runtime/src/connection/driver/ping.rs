//! Answer transport liveness probes before handing queued frames to a UI.

use super::{Pump, lock};
use crate::control::encode;
use bytes::Bytes;
use phux_protocol::wire::frame::{FrameKind, TYPE_PING};
use phux_protocol::wire::framing::LENGTH_PREFIX_LEN;
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

/// Pull Ping frames out of a queued batch and encode matching Pongs.
///
/// Non-Ping frames (and undecodable bytes) stay for the consumer. Ping is
/// session liveness, not UI state: leaving it behind a stalled drain makes
/// stall depend on the UI thread for replies.
///
/// Only a frame whose type byte says `PING` is decoded: the consumer decodes
/// everything else itself, and a full decode here would copy every output
/// payload once more just to discard it.
pub(super) fn peel_queued_pings(frames: Vec<Bytes>) -> (Vec<Bytes>, Vec<Vec<u8>>) {
    let mut keep = Vec::with_capacity(frames.len());
    let mut pongs = Vec::new();
    for frame in frames {
        if !is_ping_frame(&frame) {
            keep.push(frame);
            continue;
        }
        match FrameKind::decode(&frame) {
            Ok((FrameKind::Ping { nonce }, [])) => {
                pongs.push(encode(&FrameKind::Pong { nonce }));
            }
            _ => keep.push(frame),
        }
    }
    (keep, pongs)
}

/// Whether a length-prefixed frame's type byte is `PING`.
pub(super) fn is_ping_frame(frame: &[u8]) -> bool {
    frame.get(LENGTH_PREFIX_LEN) == Some(&TYPE_PING)
}
