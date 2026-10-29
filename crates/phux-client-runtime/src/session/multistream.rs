//! QUIC per-Terminal streams (docs/protocol.md#quic-stream-routing, ADR-0115).
//!
//! Each bound Terminal has its own bidi stream, so per-stream backpressure
//! never stalls another pane or the control channel. The client opens every
//! stream (the server never does), writes a `STREAM_BIND` header naming the
//! Terminal and a fresh app-level `StreamId`, then routes that Terminal's
//! input-side frames on it. A pump task per stream reassembles frames into
//! one merged channel the session reads beside the control stream.
//!
//! Every frame carries its origin (Terminal, `StreamId`, and the binding's
//! liveness flag), and is dropped at consumption if that binding has since
//! been retired: §4.2 requires rejecting queued frames from retired
//! bindings. A server-side stream end races `RESOURCE_CLOSED` on control
//! (L1 §4.9), so frames still addressed to an ended Terminal are dropped,
//! not failed.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::BytesMut;
use phux_protocol::caps::BootstrapLimits;
use phux_protocol::ids::{ResourceId, StreamId};
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::framing;
use phux_protocol::wire::stream_bind::{self, StreamBind};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

use super::{SessionError, decode_frame};
use crate::control::encode;

/// Depth of the merged frame channel; a stalled reader backpressures into
/// QUIC flow control, not loss.
pub(super) const MUX_FRAME_CHANNEL: usize = 64;
/// Connection-wide bytes of queued or incomplete Terminal-stream frames.
pub(super) const MUX_FRAME_BYTES: usize = 32 * 1024 * 1024;
/// Most Terminal streams one client keeps open (the server's bidi credit
/// is 128, one of which is control).
const MAX_CLIENT_TERMINAL_STREAMS: usize = 127;
/// Bound on opening a Terminal stream and writing its `STREAM_BIND`.
const TERMINAL_STREAM_OPEN_DEADLINE: Duration = Duration::from_secs(10);
/// Bound on one Terminal frame, from its first byte to its complete body;
/// also bounds the viewport-ordering barrier on control.
pub(super) const TERMINAL_FRAME_DEADLINE: Duration = Duration::from_secs(10);

/// The Terminal whose dedicated stream `frame` routes over, if bound.
pub(super) const fn terminal_target(frame: &FrameKind) -> Option<&ResourceId> {
    match frame {
        FrameKind::InputKey { terminal_id, .. }
        | FrameKind::InputMouse { terminal_id, .. }
        | FrameKind::InputFocus { terminal_id, .. }
        | FrameKind::InputPaste { terminal_id, .. }
        | FrameKind::InputTerminalReply { terminal_id, .. }
        | FrameKind::FrameAck { terminal_id, .. }
        | FrameKind::HistoryRequest { terminal_id, .. }
        | FrameKind::ResizeTerminal { terminal_id, .. } => Some(terminal_id),
        _ => None,
    }
}

/// Which binding a pumped item came from.
#[derive(Debug, Clone)]
struct Origin {
    terminal_id: ResourceId,
    stream_id: StreamId,
    /// Cleared when the binding is retired, so its queued frames are dropped.
    active: Arc<AtomicBool>,
}

/// One complete frame off a Terminal stream, holding its byte budget until
/// it is decoded; or the reason the stream broke.
type Pumped = Result<(BytesMut, OwnedSemaphorePermit), String>;

#[derive(Debug)]
enum MuxItem {
    Frame { origin: Origin, frame: Pumped },
    End(Origin),
}

/// One bound Terminal stream's client-side state.
#[derive(Debug)]
struct MuxBinding {
    origin: Origin,
    /// This stream's send half; Terminal-addressed frames route here.
    send: quinn::SendStream,
    /// Receive pump for this generation; aborted on every local teardown.
    receive_task: tokio::task::AbortHandle,
}

impl MuxBinding {
    fn retire(&mut self) {
        self.origin.active.store(false, Ordering::Release);
        self.receive_task.abort();
        let _ = self.send.finish();
    }
}

/// The per-Terminal stream state of one QUIC connection.
#[derive(Debug)]
pub(super) struct Multistream {
    /// The connection the Terminal streams ride.
    conn: quinn::Connection,
    /// Live Terminal streams by Terminal.
    bindings: HashMap<ResourceId, MuxBinding>,
    /// Next app-level `StreamId`: monotonic, never zero; a re-bind gets a new
    /// one (a new generation, L1 §4.6).
    next_stream_id: u64,
    /// Merged items from every bound Terminal stream's pump task.
    items_rx: mpsc::Receiver<MuxItem>,
    /// Sender half the pump tasks hold.
    items_tx: mpsc::Sender<MuxItem>,
    /// Connection-wide queued/incomplete Terminal-stream frame byte budget.
    frame_bytes: Arc<Semaphore>,
    /// Terminals whose stream ended or whose resource closed on control.
    ended: HashSet<ResourceId>,
    /// Maximum time from a Terminal frame's first byte to its complete body.
    terminal_frame_deadline: Duration,
}

impl Multistream {
    pub(super) fn new(conn: quinn::Connection) -> Self {
        let (items_tx, items_rx) = mpsc::channel(MUX_FRAME_CHANNEL);
        Self {
            conn,
            bindings: HashMap::new(),
            next_stream_id: 1,
            items_rx,
            items_tx,
            frame_bytes: Arc::new(Semaphore::new(MUX_FRAME_BYTES)),
            ended: HashSet::new(),
            terminal_frame_deadline: TERMINAL_FRAME_DEADLINE,
        }
    }

    #[cfg(any(test, feature = "testing"))]
    pub(super) const fn set_terminal_frame_deadline(&mut self, deadline: Duration) {
        self.terminal_frame_deadline = deadline;
    }

    /// Whether a frame for `terminal_id` has somewhere to go: a live stream,
    /// or one the server ended, whose frames [`Self::send`] drops.
    pub(super) fn can_route(&self, terminal_id: &ResourceId) -> bool {
        self.bindings.contains_key(terminal_id) || self.ended.contains(terminal_id)
    }

    /// Write `frame` on `terminal_id`'s stream. A frame for a Terminal whose
    /// stream the server already ended is dropped.
    ///
    /// # Errors
    ///
    /// [`SessionError::Protocol`] for a Terminal never bound (a caller
    /// bug: check [`Self::can_route`]); [`SessionError::Io`] when the
    /// stream write fails.
    pub(super) async fn send(
        &mut self,
        terminal_id: &ResourceId,
        frame: &FrameKind,
    ) -> Result<(), SessionError> {
        let Some(binding) = self.bindings.get_mut(terminal_id) else {
            if self.ended.contains(terminal_id) {
                tracing::debug!(
                    ?terminal_id,
                    "Terminal frame dropped: its stream already ended"
                );
                return Ok(());
            }
            return Err(SessionError::Protocol(format!(
                "Terminal frame requires a live QUIC binding: {terminal_id:?}"
            )));
        };
        binding
            .send
            .write_all(&encode(frame))
            .await
            .map_err(|error| SessionError::Io(std::io::Error::other(error)))
    }

    /// Open and bind a stream for `terminal_id`; a no-op when it is bound.
    ///
    /// # Errors
    ///
    /// [`SessionError::Protocol`] past the stream cap or on `StreamId`
    /// exhaustion; [`SessionError::Connect`] when the stream cannot be
    /// opened; [`SessionError::Io`] when its `STREAM_BIND` cannot be written.
    pub(super) async fn bind(&mut self, terminal_id: &ResourceId) -> Result<(), SessionError> {
        if self.bindings.contains_key(terminal_id) {
            return Ok(());
        }
        if self.bindings.len() >= MAX_CLIENT_TERMINAL_STREAMS {
            return Err(SessionError::Protocol(format!(
                "QUIC Terminal stream cap exceeded ({MAX_CLIENT_TERMINAL_STREAMS})"
            )));
        }
        let stream_id = self.allocate_stream_id().ok_or_else(|| {
            SessionError::Protocol("QUIC Terminal stream id exhausted".to_owned())
        })?;
        let (send, recv) = self.open_bound_stream(terminal_id, stream_id).await?;
        let origin = Origin {
            terminal_id: terminal_id.clone(),
            stream_id,
            active: Arc::new(AtomicBool::new(true)),
        };
        let receive_task = tokio::spawn(pump_terminal_stream(
            recv,
            origin.clone(),
            self.items_tx.clone(),
            Arc::clone(&self.frame_bytes),
            self.terminal_frame_deadline,
        ));
        let binding = MuxBinding {
            origin,
            send,
            receive_task: receive_task.abort_handle(),
        };
        self.bindings.insert(terminal_id.clone(), binding);
        self.ended.remove(terminal_id);
        Ok(())
    }

    /// Finish and forget one Terminal stream, if it is bound.
    pub(super) fn unbind(&mut self, terminal_id: &ResourceId) {
        if let Some(mut binding) = self.bindings.remove(terminal_id) {
            binding.retire();
        }
    }

    /// Forget a closed resource's binding and drop any subsequent input.
    pub(super) fn close_terminal(&mut self, terminal_id: &ResourceId) {
        self.unbind(terminal_id);
        self.ended.insert(terminal_id.clone());
    }

    /// Finish every client-opened Terminal stream.
    pub(super) fn unbind_all(&mut self) {
        for (_, mut binding) in self.bindings.drain() {
            binding.retire();
        }
        self.ended.clear();
    }

    /// The next frame from any bound Terminal stream. Stale frames and
    /// stream ends are absorbed; waits while none are bound.
    pub(super) async fn recv(
        &mut self,
        limits: BootstrapLimits,
    ) -> Result<FrameKind, SessionError> {
        loop {
            // A sender lives in `self`, so the channel never closes.
            let Some(item) = self.items_rx.recv().await else {
                return std::future::pending().await;
            };
            if let Some(frame) = self.accept(item, limits)? {
                return Ok(frame);
            }
        }
    }

    /// A Terminal frame already merged, if any, without waiting.
    pub(super) fn try_recv(
        &mut self,
        limits: BootstrapLimits,
    ) -> Result<Option<FrameKind>, SessionError> {
        while let Ok(item) = self.items_rx.try_recv() {
            if let Some(frame) = self.accept(item, limits)? {
                return Ok(Some(frame));
            }
        }
        Ok(None)
    }

    /// Decode a current binding's frame, or absorb a stale frame or an end.
    fn accept(
        &mut self,
        item: MuxItem,
        limits: BootstrapLimits,
    ) -> Result<Option<FrameKind>, SessionError> {
        match item {
            MuxItem::Frame { origin, frame } => {
                if !self.is_current(&origin) {
                    return Ok(None);
                }
                let (bytes, _budget) = frame.map_err(SessionError::Protocol)?;
                decode_frame(&bytes, limits)
            }
            MuxItem::End(origin) => {
                self.end_stream(&origin);
                Ok(None)
            }
        }
    }

    /// Retire the binding a server-side stream end names, if it is still the
    /// current one; a stale end from a replaced generation is ignored.
    fn end_stream(&mut self, origin: &Origin) {
        if !self.is_current(origin) {
            return;
        }
        if let Some(mut binding) = self.bindings.remove(&origin.terminal_id) {
            binding.retire();
            self.ended.insert(origin.terminal_id.clone());
        }
    }

    fn is_current(&self, origin: &Origin) -> bool {
        origin.active.load(Ordering::Acquire)
            && self
                .bindings
                .get(&origin.terminal_id)
                .is_some_and(|binding| {
                    binding.origin.stream_id == origin.stream_id
                        && Arc::ptr_eq(&binding.origin.active, &origin.active)
                })
    }

    const fn allocate_stream_id(&mut self) -> Option<StreamId> {
        let raw = self.next_stream_id;
        self.next_stream_id = self.next_stream_id.wrapping_add(1);
        if self.next_stream_id == 0 {
            self.next_stream_id = 1;
        }
        StreamId::new(raw)
    }

    /// Open a bidi stream and write its `STREAM_BIND`. The header carries
    /// the app-level stream id, never the QUIC one.
    async fn open_bound_stream(
        &self,
        terminal_id: &ResourceId,
        stream_id: StreamId,
    ) -> Result<(quinn::SendStream, quinn::RecvStream), SessionError> {
        let (mut send, recv) =
            tokio::time::timeout(TERMINAL_STREAM_OPEN_DEADLINE, self.conn.open_bi())
                .await
                .map_err(|_| SessionError::Connect("opening Terminal stream timed out".to_owned()))?
                .map_err(|error| {
                    SessionError::Connect(format!("opening Terminal stream: {error}"))
                })?;
        let mut header = BytesMut::with_capacity(64);
        stream_bind::encode(
            &StreamBind {
                terminal_id: terminal_id.clone(),
                stream_id,
            },
            &mut header,
        );
        tokio::time::timeout(TERMINAL_STREAM_OPEN_DEADLINE, send.write_all(&header))
            .await
            .map_err(|_| SessionError::Connect("writing STREAM_BIND timed out".to_owned()))?
            .map_err(|error| SessionError::Io(std::io::Error::other(error)))?;
        Ok((send, recv))
    }
}

/// Pump complete frames from one Terminal stream into the merged queue,
/// then report its end. A broken stream is reported as a failed frame
/// first, so the framing error survives the multiplexer (§4.2).
async fn pump_terminal_stream(
    mut recv: quinn::RecvStream,
    origin: Origin,
    items: mpsc::Sender<MuxItem>,
    budget: Arc<Semaphore>,
    deadline: Duration,
) {
    loop {
        let frame = match read_terminal_frame(&mut recv, &budget, deadline).await {
            Ok(Some(frame)) => Ok(frame),
            Ok(None) => break,
            Err(error) => Err(error),
        };
        let broken = frame.is_err();
        let item = MuxItem::Frame {
            origin: origin.clone(),
            frame,
        };
        if items.send(item).await.is_err() {
            return;
        }
        if broken {
            break;
        }
    }
    let _ = items.send(MuxItem::End(origin)).await;
}

/// The next complete frame, or `None` on a clean finish at a frame
/// boundary. Once a frame's first byte arrives, the rest must follow within
/// `deadline`.
async fn read_terminal_frame(
    recv: &mut quinn::RecvStream,
    budget: &Arc<Semaphore>,
    deadline: Duration,
) -> Result<Option<(BytesMut, OwnedSemaphorePermit)>, String> {
    let mut header = [0_u8; framing::LENGTH_PREFIX_LEN];
    match recv.read_exact(&mut header[..1]).await {
        Ok(()) => {}
        Err(quinn::ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Terminal stream truncated before frame header: {error}"
            ));
        }
    }
    tokio::time::timeout(deadline, read_frame_rest(recv, header, budget))
        .await
        .map_err(|_| "Terminal stream incomplete-frame deadline elapsed".to_owned())?
        .map(Some)
}

/// The rest of a frame whose first header byte is in `header`, its bytes
/// counted against the connection-wide budget before they are buffered.
async fn read_frame_rest(
    recv: &mut quinn::RecvStream,
    mut header: [u8; framing::LENGTH_PREFIX_LEN],
    budget: &Arc<Semaphore>,
) -> Result<(BytesMut, OwnedSemaphorePermit), String> {
    recv.read_exact(&mut header[1..])
        .await
        .map_err(|error| error.to_string())?;
    let body_len = framing::decode_length(header).map_err(|error| error.to_string())?;
    let total = framing::LENGTH_PREFIX_LEN + body_len;
    let permits = u32::try_from(total).map_err(|error| error.to_string())?;
    let permit = Arc::clone(budget)
        .acquire_many_owned(permits)
        .await
        .map_err(|error| format!("Terminal stream byte budget closed: {error}"))?;
    let mut frame = framing::frame_buffer(header).map_err(|error| error.to_string())?;
    recv.read_exact(&mut frame[framing::LENGTH_PREFIX_LEN..])
        .await
        .map_err(|error| format!("Terminal stream finished mid-frame: {error}"))?;
    Ok((frame, permit))
}

#[cfg(test)]
mod tests {
    use phux_protocol::ids::BootstrapId;

    use super::*;

    #[test]
    fn only_terminal_scoped_input_side_frames_route_on_terminal_streams() {
        let terminal_id = ResourceId::local(3);
        let ack = FrameKind::FrameAck {
            terminal_id: terminal_id.clone(),
            stream_id: StreamId::new(1).expect("stream"),
            bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
            seq: 1,
        };
        assert_eq!(terminal_target(&ack), Some(&terminal_id));
        let resize = FrameKind::ResizeTerminal {
            terminal_id: terminal_id.clone(),
            cols: 80,
            rows: 24,
            cell_px: None,
        };
        assert_eq!(terminal_target(&resize), Some(&terminal_id));
        assert_eq!(terminal_target(&FrameKind::Ping { nonce: 1 }), None);
        assert_eq!(
            terminal_target(&FrameKind::Bell { terminal_id }),
            None,
            "server-to-client frames never route by the client"
        );
    }
}
