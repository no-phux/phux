//! Framed send and receive on a [`Session`]: corking, the viewport
//! ordering barrier, and inbound decode.

use bytes::Bytes;
use futures_util::FutureExt;
use phux_protocol::caps::BootstrapLimits;
use phux_protocol::wire::frame::{FrameKind, TYPE_PONG};
use phux_protocol::wire::framing::{self, FramingError, LENGTH_PREFIX_LEN};

use super::multistream::{self, MUX_FRAME_BYTES, MUX_FRAME_CHANNEL, TERMINAL_FRAME_DEADLINE};
use super::{Session, SessionError};
use crate::control::encode;

impl Session {
    /// Encode `frame` and write it to the server: on its Terminal's stream
    /// under QUIC multistream, else on the control stream (into the batch
    /// while corked).
    ///
    /// # Errors
    ///
    /// The write failure; under multistream also a Terminal frame for a
    /// Terminal that was never bound, or a viewport barrier that failed.
    pub async fn send(&mut self, frame: &FrameKind) -> Result<(), SessionError> {
        let sent = self.send_frame(frame).await;
        self.note_write(sent)
    }

    async fn send_frame(&mut self, frame: &FrameKind) -> Result<(), SessionError> {
        if self.multistream_enabled()
            && let Some(terminal_id) = multistream::terminal_target(frame)
            && let Some(mux) = self.multistream.as_mut()
        {
            return mux.send(terminal_id, frame).await;
        }
        self.write_control(&encode(frame)).await?;
        if self.multistream_enabled() && matches!(frame, FrameKind::ViewportResize { .. }) {
            self.settle_viewport_order().await?;
        }
        Ok(())
    }

    async fn write_control(&mut self, bytes: &[u8]) -> Result<(), SessionError> {
        if let Some(batch) = self.corked.as_mut() {
            batch.extend_from_slice(bytes);
            return Ok(());
        }
        self.link
            .write_frame(bytes)
            .await
            .map_err(SessionError::from)
    }

    /// Record a write that failed on a closed peer, then pass the result on.
    pub(super) fn note_write(
        &mut self,
        result: Result<(), SessionError>,
    ) -> Result<(), SessionError> {
        if let Err(SessionError::Io(error)) = &result
            && matches!(
                error.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
            )
        {
            self.write_hit_closed_peer = true;
        }
        result
    }

    /// QUIC orders each stream, not the control stream against Terminal
    /// streams. Wait until the viewport vote has been dispatched before the
    /// caller can send its exact pane layout: a `PING` on control, and every
    /// control frame ahead of its `PONG` kept for [`Self::recv`].
    async fn settle_viewport_order(&mut self) -> Result<(), SessionError> {
        let nonce = uuid::Uuid::new_v4().as_u64_pair().0;
        self.write_control(&encode(&FrameKind::Ping { nonce }))
            .await?;
        let ahead = self.inbound.len();
        tokio::time::timeout(TERMINAL_FRAME_DEADLINE, self.await_pong(nonce, ahead))
            .await
            .map_err(|_| SessionError::Protocol("viewport ordering barrier timed out".to_owned()))?
    }

    /// Read control frames until the `PONG` for `nonce` arrives behind the
    /// `ahead` frames already queued, then drop it from the queue.
    async fn await_pong(&mut self, nonce: u64, ahead: usize) -> Result<(), SessionError> {
        loop {
            let pong = self
                .inbound
                .iter()
                .skip(ahead)
                .position(|frame| is_pong(frame, nonce));
            let before = pong.unwrap_or(self.inbound.len() - ahead);
            check_barrier_bound(self.inbound.iter().skip(ahead).take(before))?;
            if let Some(index) = pong {
                self.inbound.remove(ahead + index);
                return Ok(());
            }
            let batch = self.link.read_frames().await;
            self.queue_inbound(batch)?;
        }
    }

    /// Read the next frame from the server: the control stream, or under
    /// QUIC multistream any bound Terminal stream.
    ///
    /// On WebSocket the read also ping/pong probes an idle peer, so one
    /// that stops answering surfaces as [`SessionError::Disconnected`]. A
    /// SPEC §5 framing violation is answered with `ERROR { FRAME_TOO_LARGE }`
    /// (best-effort) before [`SessionError::Framing`] is returned.
    ///
    /// # Errors
    ///
    /// [`SessionError::Disconnected`] on a clean close; transport, framing,
    /// and decode failures otherwise.
    pub async fn recv(&mut self) -> Result<FrameKind, SessionError> {
        let result = self.recv_frame().await;
        if let Ok(frame) = &result {
            self.retire_closed_terminal(frame);
        }
        if let Err(SessionError::Framing(violation)) = &result {
            self.answer_framing_violation(*violation).await;
        }
        result
    }

    /// A lifecycle close on control retires the independent Terminal stream
    /// before queued output can reach the caller's closed session resource.
    fn retire_closed_terminal(&mut self, frame: &FrameKind) {
        if let FrameKind::ResourceClosed { terminal_id, .. } = frame
            && let Some(mux) = self.multistream.as_mut()
        {
            mux.close_terminal(terminal_id);
        }
    }

    async fn recv_frame(&mut self) -> Result<FrameKind, SessionError> {
        let limits = self.limits();
        loop {
            if let Some(frame) = self.inbound.pop_front() {
                match decode_frame(&frame, limits)? {
                    Some(frame) => return Ok(frame),
                    None => continue,
                }
            }
            let batch = match self.multistream.as_mut() {
                None => self.link.read_frames().await,
                Some(mux) => tokio::select! {
                    batch = self.link.read_frames() => batch,
                    frame = mux.recv(limits) => return frame,
                },
            };
            self.queue_inbound(batch)?;
        }
    }

    /// A frame that is already available without waiting, or `Ok(None)`,
    /// so a loop can drain a burst into one paint. Framing violations are
    /// answered as in [`Self::recv`], without blocking.
    ///
    /// # Errors
    ///
    /// As [`Self::recv`]; a clean close is reported by the next `recv`.
    pub fn try_recv(&mut self) -> Result<Option<FrameKind>, SessionError> {
        let result = self.try_recv_frame();
        if let Ok(Some(frame)) = &result {
            self.retire_closed_terminal(frame);
        }
        if let Err(SessionError::Framing(violation)) = &result {
            let _ = self.answer_framing_violation(*violation).now_or_never();
        }
        result
    }

    fn try_recv_frame(&mut self) -> Result<Option<FrameKind>, SessionError> {
        let limits = self.limits();
        if self.inbound.is_empty() {
            if let Some(mux) = self.multistream.as_mut()
                && let Some(frame) = mux.try_recv(limits)?
            {
                return Ok(Some(frame));
            }
            let frames = self.link.try_read_frames()?;
            self.inbound
                .extend(frames.into_iter().filter(|frame| !frame.is_empty()));
        }
        while let Some(frame) = self.inbound.pop_front() {
            if let Some(frame) = decode_frame(&frame, limits)? {
                return Ok(Some(frame));
            }
        }
        Ok(None)
    }

    /// Queue a read batch's frames; a clean close is a disconnect. Empty
    /// frames are WebSocket proof-of-life and carry nothing.
    fn queue_inbound(
        &mut self,
        batch: Result<Option<Vec<Bytes>>, crate::connection::io::LinkError>,
    ) -> Result<(), SessionError> {
        let frames = batch?.ok_or(SessionError::Disconnected)?;
        self.inbound
            .extend(frames.into_iter().filter(|frame| !frame.is_empty()));
        Ok(())
    }

    /// SPEC §5: send `ERROR { FRAME_TOO_LARGE }` before the caller drops
    /// the connection, ahead of (and so dropping) any corked batch.
    /// Best-effort: a vanished peer cannot fail the close.
    async fn answer_framing_violation(&mut self, violation: FramingError) {
        self.corked = None;
        let goodbye = encode(&framing::frame_too_large_error(violation));
        let _ = self.link.write_frame(&goodbye).await;
    }
}

/// Decode one complete, length-checked frame under `limits`.
///
/// `Ok(None)` for a well-framed frame carrying a value this build does not
/// define (an unknown frame type or enum value from a newer 1.x server): it
/// is logged and skipped, and the connection stays up. Malformed bytes are a
/// protocol error.
pub(super) fn decode_frame(
    frame: &[u8],
    limits: BootstrapLimits,
) -> Result<Option<FrameKind>, SessionError> {
    match FrameKind::decode_with_limits(frame, limits) {
        Ok((decoded, _rest)) => Ok(Some(decoded)),
        Err(error) if error.is_unknown_value() => {
            tracing::warn!(%error, "skipping a server frame this build cannot read");
            Ok(None)
        }
        Err(error) => Err(SessionError::Protocol(format!(
            "server sent undecodable frame: {error:?}"
        ))),
    }
}

/// Whether `frame` is the `PONG` answering `nonce`; only a `PONG`-typed
/// frame is decoded.
pub(super) fn is_pong(frame: &[u8], nonce: u64) -> bool {
    frame.get(LENGTH_PREFIX_LEN) == Some(&TYPE_PONG)
        && matches!(
            FrameKind::decode(frame),
            Ok((FrameKind::Pong { nonce: got }, [])) if got == nonce
        )
}

/// The viewport barrier holds at most [`MUX_FRAME_CHANNEL`] control frames
/// and [`MUX_FRAME_BYTES`] ahead of its `PONG`.
pub(super) fn check_barrier_bound<'a>(
    held: impl Iterator<Item = &'a Bytes>,
) -> Result<(), SessionError> {
    let (count, bytes) = held.fold((0, 0), |(count, bytes), frame| {
        (count + 1, bytes + frame.len())
    });
    if count > MUX_FRAME_CHANNEL || bytes > MUX_FRAME_BYTES {
        return Err(SessionError::Protocol(
            "viewport ordering barrier exceeded its control-frame bound".to_owned(),
        ));
    }
    Ok(())
}
