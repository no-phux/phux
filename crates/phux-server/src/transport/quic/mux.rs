//! The QUIC frame multiplexer: admitted frames, Terminal stream events, and the merged reader.

use super::{
    Arc, AtomicBool, BytesMut, Duration, FrameReader, LENGTH_PREFIX, Ordering, QuicReader,
    SendWindow, accept_terminal_streams, framing, io, read_framed_bounded,
};

/// Depth of the mux's merged frame channel (control plus every Terminal
/// stream). Per-stream isolation lives in QUIC flow control, not here.
pub(super) const MUX_FRAME_CHANNEL: usize = 64;
pub(super) const MAX_WIRE_FRAME_BYTES: usize =
    phux_protocol::wire::frame::MAX_FRAME_LEN as usize + LENGTH_PREFIX;
/// Two full legal frames (one Terminal, one control), each with its length
/// prefix: rounding down would deadlock one valid maximum frame.
pub(super) const MUX_FRAME_BYTES: usize = 2 * MAX_WIRE_FRAME_BYTES;
/// Terminal streams may hold at most one maximum frame of the budget, so
/// control traffic always has room.
pub(super) const MUX_TERMINAL_FRAME_BYTES: usize = MAX_WIRE_FRAME_BYTES;
/// Once a header announces a body, it must arrive within this bound, so a
/// peer cannot reserve byte permits indefinitely.
pub(super) const FRAME_BODY_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub(crate) struct AdmittedFrame {
    pub(super) bytes: BytesMut,
    pub(super) _connection_bytes: Option<tokio::sync::OwnedSemaphorePermit>,
    pub(super) _terminal_bytes: Option<tokio::sync::OwnedSemaphorePermit>,
    pub(super) origin: Option<Arc<AtomicBool>>,
    pub(super) event: Option<QuicStreamEvent>,
    pub(super) _queue_ticket: Option<crate::stream_diagnostics::QueueTicket>,
}

impl AdmittedFrame {
    pub(super) fn event(event: QuicStreamEvent) -> Self {
        Self {
            bytes: BytesMut::new(),
            _connection_bytes: None,
            _terminal_bytes: None,
            origin: None,
            event: Some(event),
            _queue_ticket: None,
        }
    }
}

/// Depth of the Terminal-stream event channel; binds and ends are rare.
pub(super) const MUX_EVENT_CHANNEL: usize = 32;
/// Half-open streams that may concurrently wait for `STREAM_BIND`.
pub(super) const MAX_PENDING_STREAM_BINDS: usize = 16;

/// Application error code resetting a refused or malformed Terminal stream.
pub(super) const BIND_REFUSED_CODE: u32 = 0x10;

/// Reset a Terminal stream unread (docs/protocol.md#quic-stream-routing). The uncorrelated `ERROR`
/// on control carries the reason.
pub(crate) fn refuse_terminal_stream(mut send: quinn::SendStream, mut recv: quinn::RecvStream) {
    let _ = send.reset(quinn::VarInt::from_u32(BIND_REFUSED_CODE));
    let _ = recv.stop(quinn::VarInt::from_u32(BIND_REFUSED_CODE));
}

/// A Terminal stream's lifecycle event (docs/protocol.md#quic-stream-routing, ADR-0115).
#[derive(Debug)]
pub(crate) enum QuicStreamEvent {
    /// A well-formed `STREAM_BIND` arrived; admission belongs to the client
    /// task, which receives everything needed to start the stream's pump.
    Bound {
        /// The Terminal whose §4 frames ride this stream.
        terminal_id: phux_protocol::ids::ResourceId,
        /// The stream generation this stream opens under.
        stream_id: phux_protocol::ids::StreamId,
        /// This stream's send half, for the per-stream writer.
        send: quinn::SendStream,
        /// Receive half, unread until bind authorization succeeds.
        recv: quinn::RecvStream,
        /// The connection-scoped congestion tracker.
        window: SendWindow,
        /// Merged destination used after bind admission.
        frames: tokio::sync::mpsc::Sender<AdmittedFrame>,
        /// Connection-wide queued/incomplete frame byte budget.
        frame_bytes: std::sync::Arc<tokio::sync::Semaphore>,
        /// Terminal-only share of that budget.
        terminal_frame_bytes: std::sync::Arc<tokio::sync::Semaphore>,
    },
    /// A bound stream ended cleanly: the detach signal.
    Ended {
        /// The Terminal the ended stream carried.
        terminal_id: phux_protocol::ids::ResourceId,
        /// The generation the ended stream opened under.
        stream_id: phux_protocol::ids::StreamId,
    },
    /// A bound stream ended because framing or transport failed; the typed
    /// cause lets the runtime report the right protocol code.
    Failed {
        terminal_id: phux_protocol::ids::ResourceId,
        stream_id: phux_protocol::ids::StreamId,
        failure: QuicStreamFailure,
    },
}

#[derive(Debug)]
pub(crate) enum QuicStreamFailure {
    Framing(framing::FramingError),
    IncompleteFrame,
    Transport(String),
}

/// QUIC read half with multi-stream upgrade (`docs/protocol.md#quic-stream-routing`).
///
/// Before [`FrameReader::take_stream_events`] it reads the control stream
/// directly; afterwards control and every bound Terminal stream pump into
/// one merged, byte-budgeted channel. Dropping the reader aborts the pumps.
pub(crate) struct QuicMuxReader {
    control: Option<QuicReader>,
    conn: quinn::Connection,
    window: SendWindow,
    pub(super) frames_rx: Option<tokio::sync::mpsc::Receiver<AdmittedFrame>>,
    control_open: bool,
    control_done: Option<tokio::sync::oneshot::Receiver<io::Result<()>>>,
    tasks: tokio::task::JoinSet<()>,
    last_origin: crate::transport::FrameOrigin,
    pub(super) stream_events_tx: Option<tokio::sync::mpsc::Sender<QuicStreamEvent>>,
    // `read_frame` is cancelled whenever runtime handles another select arm.
    // Keep dequeued lifecycle events here until their destination is reserved.
    pending_event: Option<QuicStreamEvent>,
    pub(super) diagnostics: Option<crate::stream_diagnostics::StreamTracker>,
}

impl QuicMuxReader {
    pub(crate) fn new(
        recv: quinn::RecvStream,
        conn: quinn::Connection,
        window: SendWindow,
    ) -> Self {
        Self {
            control: Some(QuicReader::from_stream(recv)),
            conn,
            window,
            frames_rx: None,
            control_open: true,
            control_done: None,
            tasks: tokio::task::JoinSet::new(),
            last_origin: crate::transport::FrameOrigin::Control,
            stream_events_tx: None,
            pending_event: None,
            diagnostics: None,
        }
    }
}

impl FrameReader for QuicMuxReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        let Some(rx) = self.frames_rx.as_mut() else {
            // Pre-upgrade: the upgrade takes `control` with it, so it is
            // present exactly here.
            let Some(reader) = self.control.as_mut() else {
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "quic mux upgraded without its control stream",
                ));
            };
            self.last_origin = crate::transport::FrameOrigin::Control;
            return reader.read_frame().await;
        };
        loop {
            if !self.control_open {
                return Ok(None);
            }
            if self.pending_event.is_some() {
                let Some(events) = self.stream_events_tx.as_ref() else {
                    return Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        "quic mux lost its stream event channel",
                    ));
                };
                let Ok(permit) = events.reserve().await else {
                    return Ok(None);
                };
                if let Some(event) = self.pending_event.take() {
                    permit.send(event);
                }
            }
            tokio::select! {
                biased;
                done = async {
                    match self.control_done.as_mut() {
                        Some(rx) => rx.await,
                        None => core::future::pending().await,
                    }
                } => {
                    // Control end is connection end, even with live Terminal
                    // streams: nothing can continue without control.
                    self.control_open = false;
                    return done.map_or_else(|_| Ok(None), |result| result.map(|()| None));
                }
                frame = rx.recv() => {
                    let Some(admitted) = frame else {
                        return Ok(None);
                    };
                    if let Some(event) = admitted.event {
                        self.pending_event = Some(event);
                        continue;
                    }
                    // A retired Terminal stream's queued frames are dropped.
                    if admitted.origin.as_ref().is_some_and(|origin| {
                        !origin.load(Ordering::Acquire)
                    }) {
                        continue;
                    }
                    self.last_origin = if admitted.origin.is_some() {
                        crate::transport::FrameOrigin::Terminal
                    } else {
                        crate::transport::FrameOrigin::Control
                    };
                    return Ok(Some(admitted.bytes));
                },
            }
        }
    }

    fn frame_origin(&self) -> crate::transport::FrameOrigin {
        self.last_origin
    }

    fn take_stream_events(&mut self) -> Option<tokio::sync::mpsc::Receiver<QuicStreamEvent>> {
        if self.frames_rx.is_some() {
            return None;
        }
        let (frames_tx, frames_rx) = tokio::sync::mpsc::channel(MUX_FRAME_CHANNEL);
        let frame_bytes = std::sync::Arc::new(tokio::sync::Semaphore::new(MUX_FRAME_BYTES));
        let terminal_frame_bytes =
            std::sync::Arc::new(tokio::sync::Semaphore::new(MUX_TERMINAL_FRAME_BYTES));
        let (events_tx, events_rx) = tokio::sync::mpsc::channel(MUX_EVENT_CHANNEL);
        self.stream_events_tx = Some(events_tx.clone());
        let (control_done_tx, control_done_rx) = tokio::sync::oneshot::channel();
        let mut control = self.control.take()?;
        let conn = self.conn.clone();
        let window = self.window.clone();
        let control_frames_tx = frames_tx.clone();
        let control_frame_bytes = frame_bytes.clone();
        let diagnostics = self.diagnostics.clone();
        self.tasks.spawn(async move {
            loop {
                let read = read_framed_bounded(
                    &mut control.recv,
                    &control_frame_bytes,
                    None,
                    None,
                    diagnostics.as_ref(),
                )
                .await;
                match read {
                    Ok(Some(frame)) => {
                        if control_frames_tx.send(frame).await.is_err() {
                            return;
                        }
                    }
                    Ok(None) => {
                        let _ = control_done_tx.send(Ok(()));
                        return;
                    }
                    Err(err) => {
                        let _ = control_done_tx.send(Err(err));
                        return;
                    }
                }
            }
        });
        self.tasks.spawn(accept_terminal_streams(
            conn,
            window,
            frames_tx,
            events_tx,
            frame_bytes,
            terminal_frame_bytes,
        ));
        self.frames_rx = Some(frames_rx);
        self.control_done = Some(control_done_rx);
        Some(events_rx)
    }
}
