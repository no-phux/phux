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

use super::dispatch::handle_client;
use super::plumbing::StreamBinding;
use super::writer::{RevocationWatch, writer_task};
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
    writer_task(
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
) -> StreamBinding {
    StreamBinding {
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
        Outbound::FatalError {
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

mod connection;
