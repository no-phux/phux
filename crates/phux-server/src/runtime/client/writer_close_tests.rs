use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;

use bytes::BytesMut;
use phux_protocol::ids::StreamId;
use phux_protocol::policy::TransportType;
use phux_protocol::wire::frame::{DetachReason, ErrorCode, FrameKind};
use tokio::sync::mpsc;
use tokio::task::LocalSet;
use tokio_util::sync::CancellationToken;

use super::{RevocationWatch, handle_client};
use crate::state::{ClientId, Outbound, SharedState};
use crate::transport::{FrameReader, FrameWriter};

/// What a test transport was asked to do, in order.
#[derive(Debug)]
enum Wrote {
    Frame(Vec<u8>),
    Flush,
    Close,
}

/// The frames and lifecycle a writer produced, coarsened for asserting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WriterEvent {
    Frame,
    Error,
    Detached(Option<DetachReason>),
    Close,
}

/// A transport that records everything, using the default per-frame
/// `write_frames` slicing a message-oriented transport relies on.
#[derive(Clone, Default)]
struct Recorder(Rc<RefCell<Vec<Wrote>>>);

impl Recorder {
    fn slices(&self) -> Vec<Vec<u8>> {
        self.0
            .borrow()
            .iter()
            .filter_map(|wrote| match wrote {
                Wrote::Frame(bytes) => Some(bytes.clone()),
                Wrote::Flush | Wrote::Close => None,
            })
            .collect()
    }

    fn events(&self) -> Vec<WriterEvent> {
        self.0
            .borrow()
            .iter()
            .filter_map(|wrote| match wrote {
                Wrote::Frame(bytes) => Some(match FrameKind::decode(bytes).expect("frame").0 {
                    FrameKind::Error { .. } => WriterEvent::Error,
                    FrameKind::Detached { reason, .. } => WriterEvent::Detached(reason),
                    _ => WriterEvent::Frame,
                }),
                Wrote::Flush => None,
                Wrote::Close => Some(WriterEvent::Close),
            })
            .collect()
    }

    /// How many frames each flushed batch carried.
    fn batches(&self) -> Vec<usize> {
        let mut batches = Vec::new();
        let mut pending = 0;
        for wrote in self.0.borrow().iter() {
            match wrote {
                Wrote::Frame(_) => pending += 1,
                Wrote::Flush => batches.push(std::mem::take(&mut pending)),
                Wrote::Close => {}
            }
        }
        batches
    }
}

#[allow(
    clippy::unused_async_trait_impl,
    reason = "the recording test writer implements the production async transport trait without I/O"
)]
impl FrameWriter for Recorder {
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.0.borrow_mut().push(Wrote::Frame(frame.to_vec()));
        Ok(())
    }

    async fn flush(&mut self) -> io::Result<()> {
        self.0.borrow_mut().push(Wrote::Flush);
        Ok(())
    }

    async fn close(&mut self) -> io::Result<()> {
        self.0.borrow_mut().push(Wrote::Close);
        Ok(())
    }
}

/// Scripted client frames, then pending forever (or EOF when `eof`).
pub(super) struct Script {
    frames: VecDeque<BytesMut>,
    eof: bool,
}

impl Script {
    pub(super) fn new(frames: impl IntoIterator<Item = FrameKind>) -> Self {
        Self {
            frames: frames
                .into_iter()
                .map(|frame| {
                    let mut encoded = BytesMut::new();
                    frame.encode(&mut encoded);
                    encoded
                })
                .collect(),
            eof: false,
        }
    }
}

impl FrameReader for Script {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        if let Some(frame) = self.frames.pop_front() {
            return Ok(Some(frame));
        }
        if self.eof {
            return Ok(None);
        }
        std::future::pending().await
    }
}

/// Run the writer over `frames` already queued and a closed mailbox.
async fn write_all(frames: impl IntoIterator<Item = Outbound>) -> Recorder {
    let recorder = Recorder::default();
    let (tx, rx) = mpsc::channel(64);
    for frame in frames {
        tx.try_send(frame).expect("queue frame");
    }
    drop(tx);
    let (_close_tx, close_rx) = tokio::sync::watch::channel(false);
    writer(recorder.clone(), rx, close_rx).await;
    recorder
}

fn writer<W: FrameWriter>(
    transport: W,
    rx: mpsc::Receiver<Outbound>,
    close: tokio::sync::watch::Receiver<bool>,
) -> impl std::future::Future<Output = ()> {
    super::writer_task(
        transport,
        rx,
        close,
        std::sync::Arc::new(std::sync::atomic::AtomicU8::new(
            phux_protocol::caps::Compression::None.as_u8(),
        )),
        ClientId(1),
        RevocationWatch::default(),
    )
}

fn pong(nonce: u64) -> Outbound {
    Outbound::Frame(FrameKind::Pong { nonce })
}

struct StalledWriter {
    entered: std::sync::Arc<tokio::sync::Notify>,
    dropped: Rc<std::cell::Cell<bool>>,
}

impl Drop for StalledWriter {
    fn drop(&mut self) {
        self.dropped.set(true);
    }
}

impl FrameWriter for StalledWriter {
    async fn write_frame(&mut self, _frame: &[u8]) -> io::Result<()> {
        self.entered.notify_one();
        std::future::pending().await
    }

    async fn close(&mut self) -> io::Result<()> {
        std::future::pending().await
    }
}

fn binding(
    tx: mpsc::Sender<Outbound>,
    writer: tokio::task::JoinSet<()>,
    writer_close: tokio::sync::watch::Sender<bool>,
) -> super::StreamBinding {
    super::StreamBinding {
        tx,
        stream_id: StreamId::new(1).expect("stream id"),
        writer,
        writer_close,
        diagnostics: None,
        ingress_active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        ingress_cancel: CancellationToken::new(),
    }
}

/// Retirement is bounded: a writer stalled on a peer that stopped
/// reading is reaped even while another sender keeps the mailbox open.
#[tokio::test(flavor = "current_thread")]
async fn retired_stream_reaps_stalled_writer_with_extra_sender() {
    LocalSet::new()
        .run_until(async {
            let entered = std::sync::Arc::new(tokio::sync::Notify::new());
            let dropped = Rc::new(std::cell::Cell::new(false));
            let (tx, rx) = mpsc::channel(1);
            let extra_sender = tx.clone();
            let (close_tx, close_rx) = tokio::sync::watch::channel(false);
            let mut tasks = tokio::task::JoinSet::new();
            let stalled = StalledWriter {
                entered: entered.clone(),
                dropped: dropped.clone(),
            };
            tasks.spawn_local(writer(stalled, rx, close_rx));
            tx.send(pong(1)).await.expect("queue");
            tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
                .await
                .expect("writer entered");
            let mut binding = binding(tx, tasks, close_tx);
            let ingress_active = binding.ingress_active.clone();
            let ingress_cancel = binding.ingress_cancel.clone();
            tokio::time::timeout(std::time::Duration::from_secs(2), binding.retire())
                .await
                .expect("bounded retirement");
            assert!(dropped.get(), "retirement must reap the transport writer");
            assert!(binding.writer.is_empty());
            assert!(!ingress_active.load(std::sync::atomic::Ordering::Acquire));
            assert!(ingress_cancel.is_cancelled());
            assert!(extra_sender.is_closed());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn retired_stream_drains_queued_frames_before_close() {
    LocalSet::new()
        .run_until(async {
            let recorder = Recorder::default();
            let (tx, rx) = mpsc::channel(2);
            let (writer_close, close_rx) = tokio::sync::watch::channel(false);
            tx.try_send(pong(1)).expect("queue");
            tx.try_send(pong(2)).expect("queue");
            let extra_sender = tx.clone();
            let mut tasks = tokio::task::JoinSet::new();
            tasks.spawn_local(writer(recorder.clone(), rx, close_rx));
            let mut binding = binding(tx, tasks, writer_close);
            binding.retire().await;
            assert_eq!(
                recorder.events(),
                [WriterEvent::Frame, WriterEvent::Frame, WriterEvent::Close]
            );
            assert!(extra_sender.is_closed());
            assert!(binding.writer.is_empty());
        })
        .await;
}

/// An already-queued burst costs one transport write and flush.
#[tokio::test(flavor = "current_thread")]
async fn a_queued_burst_costs_one_transport_write() {
    let recorder = write_all((0..8).map(pong)).await;
    assert_eq!(recorder.batches(), [8]);
}

/// Batching coalesces writes, never frames: each slice a message-oriented
/// transport sends is exactly one well-formed frame, including one near
/// a megabyte beside tiny ones.
#[tokio::test(flavor = "current_thread")]
async fn a_batch_slices_back_into_exactly_the_frames_that_went_in() {
    let sent: Vec<FrameKind> = (0..8)
        .chain([20])
        .map(|shift| FrameKind::Error {
            request_id: None,
            code: ErrorCode::InternalError,
            message: "x".repeat(1 << shift),
        })
        .collect();
    let recorder = write_all(sent.iter().cloned().map(Outbound::Frame)).await;
    let slices = recorder.slices();
    assert_eq!(slices.len(), sent.len());
    for (slice, original) in slices.iter().zip(&sent) {
        phux_protocol::wire::framing::check_frame(slice).expect("one well-formed frame");
        let (decoded, rest) = FrameKind::decode(slice).expect("slice decodes");
        assert!(rest.is_empty(), "a slice must not carry a second frame");
        assert_eq!(format!("{decoded:?}"), format!("{original:?}"));
    }
}

/// A lone frame leaves on its own write; nothing lingers for company.
#[tokio::test(flavor = "current_thread")]
async fn a_lone_frame_is_written_without_waiting_for_company() {
    LocalSet::new()
        .run_until(async {
            let recorder = Recorder::default();
            let (tx, rx) = mpsc::channel(8);
            let (_close_tx, close_rx) = tokio::sync::watch::channel(false);
            let task = tokio::task::spawn_local(writer(recorder.clone(), rx, close_rx));
            tx.send(pong(1)).await.expect("queue frame");
            for _ in 0..16 {
                if !recorder.batches().is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert_eq!(recorder.batches(), [1]);
            drop(tx);
            task.await.expect("writer task");
        })
        .await;
}

/// Nothing queued behind a terminal error is written; the ERROR and its
/// DETACHED precede the close.
#[tokio::test(flavor = "current_thread")]
async fn terminal_error_is_written_before_transport_close() {
    let recorder = write_all([
        pong(1),
        Outbound::TerminalError {
            request_id: None,
            code: ErrorCode::CodecUnavailable,
            message: "fatal native stream failure".to_owned(),
        },
        pong(2),
    ])
    .await;
    assert_eq!(
        recorder.events(),
        [
            WriterEvent::Frame,
            WriterEvent::Error,
            WriterEvent::Detached(Some(DetachReason::ProtocolError)),
            WriterEvent::Close
        ],
    );
}

async fn client(
    reader: impl FrameReader + 'static,
    token: CancellationToken,
    root_token: CancellationToken,
) -> Recorder {
    let recorder = Recorder::default();
    Box::pin(handle_client(
        reader,
        recorder.clone(),
        SharedState::new(),
        ClientId(9),
        token,
        root_token,
        None,
        TransportType::UnixSocket,
        false,
    ))
    .await
    .expect("clean close");
    recorder
}

/// However a connection ends, its token fires, so no per-connection task
/// outlives it.
#[tokio::test(flavor = "current_thread")]
async fn eof_cancels_the_connection_token() {
    LocalSet::new()
        .run_until(async {
            let token = CancellationToken::new();
            let mut eof = Script::new([]);
            eof.eof = true;
            client(eof, token.clone(), CancellationToken::new()).await;
            assert!(token.is_cancelled());
        })
        .await;
}

/// Run a negotiated connection over `frames` (HELLO first, EOF last)
/// and decode everything the server wrote, plus whether it closed.
async fn negotiated(frames: impl IntoIterator<Item = FrameKind>) -> (Vec<FrameKind>, bool) {
    let state = SharedState::new();
    let client_id = state.with_mut(|server| {
        let client_id = server.new_client_id();
        server.set_peer_identity(
            client_id,
            phux_protocol::policy::PeerIdentity {
                uid: nix::unistd::geteuid().as_raw(),
                pid: None,
                exe_path: None,
                mcp_host_key: None,
                transport: TransportType::UnixSocket,
                source_addr: None,
            },
        );
        client_id
    });
    let hello = FrameKind::Hello {
        client_name: "dispatch-characterization".to_owned(),
        protocol_major: phux_protocol::PROTOCOL_VERSION.major,
        protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
        protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
        client_caps: phux_protocol::caps::ClientCapabilities::new(),
    };
    let mut script = Script::new(std::iter::once(hello).chain(frames));
    script.eof = true;
    let recorder = Recorder::default();
    LocalSet::new()
        .run_until(Box::pin(handle_client(
            script,
            recorder.clone(),
            state,
            client_id,
            CancellationToken::new(),
            CancellationToken::new(),
            None,
            TransportType::UnixSocket,
            false,
        )))
        .await
        .expect("clean close");
    let decoded = recorder
        .slices()
        .iter()
        .map(|slice| FrameKind::decode(slice).expect("frame").0)
        .collect();
    let closed = recorder.events().last() == Some(&WriterEvent::Close);
    (decoded, closed)
}

/// A negotiated PING is answered and the connection stays up until EOF.
#[tokio::test(flavor = "current_thread")]
async fn a_negotiated_ping_is_answered_and_keeps_the_connection() {
    let (frames, closed) = negotiated([FrameKind::Ping { nonce: 7 }]).await;
    assert!(
        matches!(frames.first(), Some(FrameKind::HelloOk { .. })),
        "{frames:?}"
    );
    assert!(
        frames
            .iter()
            .any(|frame| matches!(frame, FrameKind::Pong { nonce: 7 })),
        "{frames:?}"
    );
    assert!(
        !frames
            .iter()
            .any(|frame| matches!(frame, FrameKind::Error { .. }))
    );
    assert!(!closed, "EOF leaves nobody to close for");
}

/// A server-to-client kind from a client is direction-invalid: ERROR,
/// DETACHED, close.
#[tokio::test(flavor = "current_thread")]
async fn a_server_frame_from_a_client_closes_the_connection() {
    let (frames, closed) = negotiated([FrameKind::Pong { nonce: 1 }]).await;
    assert!(
        frames.iter().any(|frame| matches!(
            frame,
            FrameKind::Error { code: ErrorCode::InvalidCommand, message, .. }
                if message.contains("negotiated phase")
        )),
        "{frames:?}"
    );
    assert!(matches!(
        frames.last(),
        Some(FrameKind::Detached {
            reason: Some(DetachReason::ProtocolError),
            ..
        })
    ));
    assert!(closed);
}

/// ATTACH id zero is reserved: the connection closes as malformed.
#[tokio::test(flavor = "current_thread")]
async fn a_zero_attach_id_closes_the_connection() {
    let (frames, closed) = negotiated([FrameKind::Attach {
        attach_id: 0,
        target: phux_protocol::wire::frame::AttachTarget::ByName("none".to_owned()),
        viewport: phux_protocol::wire::frame::ViewportInfo::new(80, 24),
        request_scrollback: false,
        scrollback_limit_lines: 0,
        role_policy: None,
    }])
    .await;
    assert!(
        frames.iter().any(|frame| matches!(
            frame,
            FrameKind::Error { code: ErrorCode::MalformedMessage, message, .. }
                if message.contains("attach_id must be nonzero")
        )),
        "{frames:?}"
    );
    assert!(closed);
}

/// A reused ATTACH id is refused, but the connection stays up.
#[tokio::test(flavor = "current_thread")]
async fn a_reused_attach_id_is_refused_without_closing() {
    let attach = || FrameKind::Attach {
        attach_id: 3,
        target: phux_protocol::wire::frame::AttachTarget::ByName("none".to_owned()),
        viewport: phux_protocol::wire::frame::ViewportInfo::new(80, 24),
        request_scrollback: false,
        scrollback_limit_lines: 0,
        role_policy: None,
    };
    let (frames, closed) = Box::pin(negotiated([
        attach(),
        attach(),
        FrameKind::Ping { nonce: 5 },
    ]))
    .await;
    assert!(
        frames.iter().any(|frame| matches!(
            frame,
            FrameKind::Error { code: ErrorCode::MalformedMessage, message, .. }
                if message.contains("already used on this connection")
        )),
        "{frames:?}"
    );
    assert!(
        frames
            .iter()
            .any(|frame| matches!(frame, FrameKind::Pong { nonce: 5 })),
        "a later frame is still served: {frames:?}"
    );
    assert!(!closed);
}

struct PingBeforeHelloReader {
    remaining: u8,
}

impl FrameReader for PingBeforeHelloReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        if self.remaining == 0 {
            return std::future::pending().await;
        }
        tokio::time::sleep(crate::transport::HANDSHAKE_DEADLINE / 4).await;
        self.remaining -= 1;
        let mut frame = BytesMut::new();
        FrameKind::Ping {
            nonce: u64::from(self.remaining),
        }
        .encode(&mut frame);
        Ok(Some(frame))
    }
}

/// Pre-HELLO PINGs get PONGs but cannot extend the absolute deadline,
/// which ends in ERROR, DETACHED, close.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn pre_hello_ping_cannot_reset_absolute_handshake_deadline() {
    LocalSet::new()
        .run_until(async {
            let task = tokio::task::spawn_local(client(
                PingBeforeHelloReader { remaining: 8 },
                CancellationToken::new(),
                CancellationToken::new(),
            ));
            tokio::time::advance(crate::transport::HANDSHAKE_DEADLINE).await;
            let events = task.await.expect("client task").events();
            assert!(events.contains(&WriterEvent::Frame));
            assert!(
                events.ends_with(&[
                    WriterEvent::Error,
                    WriterEvent::Detached(Some(DetachReason::ProtocolError)),
                    WriterEvent::Close,
                ]),
                "{events:?}",
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn root_cancellation_flushes_server_shutdown_before_transport_close() {
    LocalSet::new()
        .run_until(async {
            let root_token = CancellationToken::new();
            let task = tokio::task::spawn_local(client(
                Script::new([]),
                root_token.child_token(),
                root_token.clone(),
            ));
            tokio::task::yield_now().await;
            root_token.cancel();
            assert_eq!(
                task.await.expect("client task").events(),
                [
                    WriterEvent::Detached(Some(DetachReason::ServerShutdown)),
                    WriterEvent::Close
                ],
            );
        })
        .await;
}
