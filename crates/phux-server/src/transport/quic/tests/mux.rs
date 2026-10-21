//! QUIC transport tests: Terminal stream provenance and the multiplexer.

use super::*;

#[test]
fn terminal_stream_provenance_rejects_cross_target_and_control_frames() {
    use phux_protocol::ids::{BootstrapId, StreamId};
    use phux_protocol::wire::frame::FrameKind;

    let bound = ResourceId::local(7);
    let stream = StreamId::new(3).unwrap();
    let mut encoded = BytesMut::new();
    FrameKind::HistoryRequest {
        terminal_id: bound.clone(),
        stream_id: stream,
        bootstrap_id: BootstrapId::new(9).unwrap(),
        cursor: bytes::Bytes::new(),
        max_bytes: 1,
        max_rows: 1,
    }
    .encode(&mut encoded);
    assert!(terminal_frame_matches(&encoded, &bound, stream));
    assert!(terminal_frame_matches(&focus_frame(), &bound, stream));
    assert!(!terminal_frame_matches(
        &focus_frame(),
        &ResourceId::local(8),
        stream
    ));
    encoded.clear();
    FrameKind::Ping { nonce: 1 }.encode(&mut encoded);
    assert!(!terminal_frame_matches(&encoded, &bound, stream));
}

/// A loopback connection whose control stream already carried [`FRAME`].
struct Connected {
    _dir: tempfile::TempDir,
    _listener: QuicListener,
    _endpoint: quinn::Endpoint,
    conn: quinn::Connection,
    control: quinn::SendStream,
    control_recv: quinn::RecvStream,
    reader: QuicMuxReader,
    writer: QuicWriter,
}

async fn connected(endpoint: quinn::Endpoint) -> Connected {
    let (dir, listener, addr) = listener(None);
    let client = async {
        let (conn, mut send, recv) = open_control(&endpoint, addr).await;
        send.write_all(&FRAME).await.unwrap();
        (conn, send, recv)
    };
    let (accepted, (conn, control, control_recv)) = join_bounded(listener.accept(), client).await;
    let (reader, writer, _) = accepted.unwrap();
    Connected {
        _dir: dir,
        _listener: listener,
        _endpoint: endpoint,
        conn,
        control,
        control_recv,
        reader,
        writer,
    }
}

impl Connected {
    /// Read the control frame, then upgrade the mux (once only).
    async fn upgrade(&mut self) -> tokio::sync::mpsc::Receiver<QuicStreamEvent> {
        let first = self.reader.read_frame().await.unwrap().unwrap();
        assert_eq!(first.as_ref(), &FRAME);
        let events = self.reader.take_stream_events().expect("upgrade once");
        assert!(self.reader.take_stream_events().is_none(), "one-shot");
        events
    }

    /// Open a Terminal stream and send its `STREAM_BIND`.
    async fn bind(&self, terminal: u32) -> (quinn::SendStream, quinn::RecvStream) {
        let (mut send, recv) = self.conn.open_bi().await.unwrap();
        send.write_all(&stream_bind_bytes(terminal, 1))
            .await
            .unwrap();
        (send, recv)
    }

    async fn read_frame(&mut self) -> BytesMut {
        tokio::time::timeout(Duration::from_secs(5), self.reader.read_frame())
            .await
            .expect("frame arrives")
            .unwrap()
            .unwrap()
    }

    /// The next lifecycle event; a frame in the meantime is a failure.
    async fn next_event_not_frame(
        &mut self,
        events: &mut tokio::sync::mpsc::Receiver<QuicStreamEvent>,
    ) -> QuicStreamEvent {
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                event = events.recv() => event.expect("event channel open"),
                frame = self.reader.read_frame() => panic!("unexpected frame: {frame:?}"),
            }
        })
        .await
        .expect("event arrives")
    }
}

async fn next_event(events: &mut tokio::sync::mpsc::Receiver<QuicStreamEvent>) -> QuicStreamEvent {
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("event arrives")
        .expect("event channel open")
}

#[tokio::test]
async fn mux_upgrades_and_merges_terminal_streams() {
    let mut c = connected(client_endpoint()).await;
    let mut events = c.upgrade().await;
    let (mut term, _term_recv) = c.bind(7).await;
    let (terminal_id, stream_id, _, _) = spawn_pump(next_event(&mut events).await);
    assert_eq!((terminal_id, stream_id.get()), (ResourceId::local(7), 1));
    term.write_all(&focus_frame()).await.unwrap();
    assert_eq!(c.read_frame().await, focus_frame());
    // A clean client finish surfaces as the detach signal.
    term.finish().unwrap();
    let ended = c.next_event_not_frame(&mut events).await;
    assert!(
        matches!(&ended, QuicStreamEvent::Ended { terminal_id, stream_id }
            if *terminal_id == ResourceId::local(7) && stream_id.get() == 1),
        "{ended:?}"
    );
}

/// A cancelled `read_frame` must not lose a dequeued lifecycle event.
#[tokio::test]
async fn mux_keeps_an_end_event_when_read_is_cancelled() {
    let mut c = connected(client_endpoint()).await;
    let (frames_tx, frames_rx) = tokio::sync::mpsc::channel(2);
    let (events_tx, mut events_rx) = tokio::sync::mpsc::channel(1);
    let ended = |id| QuicStreamEvent::Ended {
        terminal_id: ResourceId::local(id),
        stream_id: phux_protocol::ids::StreamId::new(1).unwrap(),
    };
    events_tx.try_send(ended(1)).unwrap();
    frames_tx.try_send(AdmittedFrame::event(ended(2))).unwrap();
    let mut frame = AdmittedFrame::event(ended(3));
    frame.event = None;
    frame.bytes = BytesMut::from(FRAME.as_slice());
    frames_tx.try_send(frame).unwrap();
    c.reader.frames_rx = Some(frames_rx);
    c.reader.stream_events_tx = Some(events_tx);
    {
        let read = c.reader.read_frame();
        tokio::pin!(read);
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(read.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }
    let is_end = |event: Option<QuicStreamEvent>, id| {
        matches!(event, Some(QuicStreamEvent::Ended { terminal_id, .. })
            if terminal_id == ResourceId::local(id))
    };
    assert!(is_end(events_rx.recv().await, 1));
    assert_eq!(c.read_frame().await.as_ref(), &FRAME);
    assert!(is_end(events_rx.try_recv().ok(), 2));
}

#[tokio::test]
async fn cancelled_partial_writer_resets_instead_of_finishing() {
    let mut transport = quinn::TransportConfig::default();
    transport.stream_receive_window(32_768_u32.into());
    let mut c = connected(client_endpoint_with(Some(transport))).await;
    let context = c.writer.diagnostic_tracker().context();
    let frame = vec![0_u8; 65_536];
    {
        let write = c.writer.write_frame(&frame);
        tokio::pin!(write);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut write)
                .await
                .is_err(),
            "stream credit must hold the partial write"
        );
        let sample = crate::stream_diagnostics::snapshot()
            .streams
            .into_iter()
            .find(|sample| sample.context == context)
            .expect("registered writer");
        assert!(sample.write_in_progress_age_us.unwrap() >= 90_000);
    }
    drop(c.writer);
    assert!(
        !crate::stream_diagnostics::snapshot()
            .streams
            .iter()
            .any(|sample| sample.context == context),
        "cancelled writer registration removed"
    );
    let error = c.control_recv.read_to_end(65_536).await.unwrap_err();
    assert!(
        matches!(error, quinn::ReadToEndError::Read(quinn::ReadError::Reset(code))
            if code == quinn::VarInt::from_u32(0x10)),
        "{error:?}"
    );
}

#[tokio::test]
async fn mux_diagnostics_account_admitted_bytes_until_delivery() {
    let mut c = connected(client_endpoint()).await;
    let context = c.writer.diagnostic_tracker().context();
    let sample = || {
        crate::stream_diagnostics::snapshot()
            .streams
            .into_iter()
            .find(|sample| sample.context == context)
            .unwrap()
    };
    let _events = c.upgrade().await;
    c.control.write_all(&FRAME).await.unwrap();
    while c.reader.frames_rx.as_ref().unwrap().is_empty() {
        tokio::task::yield_now().await;
    }
    let queued = sample();
    assert!(queued.active);
    assert_eq!(queued.queue_bytes, FRAME.len() as u64);
    assert_eq!(queued.queue_items, 1);
    assert!(queued.queue_oldest_age_us.is_some());
    assert_eq!(c.read_frame().await.as_ref(), &FRAME);
    let drained = sample();
    assert_eq!(drained.queue_bytes, 0);
    assert_eq!(drained.queue_oldest_age_us, None);
}

#[tokio::test]
async fn mux_resets_a_stream_with_no_well_formed_bind() {
    let mut c = connected(client_endpoint()).await;
    let mut events = c.upgrade().await;
    let (mut bad_send, mut bad_recv) = c.conn.open_bi().await.unwrap();
    bad_send.write_all(b"not a bind header").await.unwrap();
    let mut buf = [0u8; 8];
    let reset = tokio::time::timeout(Duration::from_secs(5), bad_recv.read(&mut buf)).await;
    assert!(
        matches!(reset, Ok(Err(_))),
        "reset stream errors, got {reset:?}"
    );
    // The malformed stream was reset first, so the first event must be
    // the valid bind.
    let _valid = c.bind(7).await;
    let event = next_event(&mut events).await;
    assert!(
        matches!(&event, QuicStreamEvent::Bound { terminal_id, stream_id, .. }
            if *terminal_id == ResourceId::local(7) && stream_id.get() == 1),
        "malformed bind emitted an event: {event:?}"
    );
}

#[tokio::test]
async fn dropping_mux_aborts_incomplete_bind_workers() {
    let mut c = connected(client_endpoint()).await;
    let mut events = c.upgrade().await;
    let (mut incomplete, _recv) = c.conn.open_bi().await.unwrap();
    incomplete.write_all(&[0]).await.unwrap();
    let sender = c.reader.stream_events_tx.clone().expect("mux event sender");
    tokio::time::timeout(Duration::from_secs(5), async {
        // Ours, the reader's, and the incomplete bind worker's.
        while sender.strong_count() < 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("incomplete bind worker starts");
    drop(sender);
    drop(c.reader);
    let closed = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("bind worker ownership closes event channel");
    assert!(closed.is_none());
}

#[tokio::test]
async fn mux_preserves_typed_terminal_framing_failure() {
    let mut c = connected(client_endpoint()).await;
    let mut events = c.upgrade().await;
    let (mut terminal, _recv) = c.bind(7).await;
    spawn_pump(next_event(&mut events).await);
    terminal.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
    let event = c.next_event_not_frame(&mut events).await;
    assert!(matches!(
        event,
        QuicStreamEvent::Failed {
            failure: QuicStreamFailure::Framing(framing::FramingError::LengthOutOfRange {
                length: u32::MAX
            }),
            ..
        }
    ));
}

#[tokio::test]
async fn retired_terminal_frames_cannot_survive_into_shared_dispatch() {
    let mut c = connected(client_endpoint()).await;
    let mut events = c.upgrade().await;
    let (mut terminal, _recv) = c.bind(7).await;
    let (_, _, _, active) = spawn_pump(next_event(&mut events).await);
    terminal.write_all(&focus_frame()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while c.reader.frames_rx.as_ref().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("terminal frame reaches shared dispatch");
    active.store(false, Ordering::Release);
    c.control.write_all(&FRAME).await.unwrap();
    assert_eq!(
        c.read_frame().await.as_ref(),
        &FRAME,
        "retired Terminal frame was discarded"
    );
}

#[tokio::test]
async fn incomplete_terminal_bodies_cannot_starve_control() {
    let mut c = connected(client_endpoint()).await;
    let mut events = c.upgrade().await;
    let (mut first, _first_recv) = c.bind(7).await;
    let (mut second, _second_recv) = c.bind(8).await;
    let (_, _, terminal_budget, _) = spawn_pump(next_event(&mut events).await);
    spawn_pump(next_event(&mut events).await);
    let max_header = phux_protocol::wire::frame::MAX_FRAME_LEN.to_be_bytes();
    first.write_all(&max_header).await.unwrap();
    second.write_all(&max_header).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while terminal_budget.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("terminal pump reserves its incomplete body");
    c.control.write_all(&FRAME).await.unwrap();
    assert_eq!(
        c.read_frame().await.as_ref(),
        &FRAME,
        "control is not starved"
    );
}
