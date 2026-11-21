use std::sync::{Arc, Mutex};

use bytes::BytesMut;
use futures_util::FutureExt;
use phux_protocol::wire::frame::{FrameKind, TYPE_PING};
use phux_protocol::wire::framing::LENGTH_PREFIX_LEN;
use tokio::sync::{Notify, watch};

use super::*;
use crate::connection::io::SEND_BUDGET;
use crate::control::{ControlOptions, ControlPlane, encode};

mod diagnostics;

#[test]
fn wsl_reconnects_like_a_local_upgrade_without_overriding_custom_pacing() {
    let target = Target::wsl("/linux.sock".into(), "/custom/phux".into());
    assert_eq!(
        options_for(&target, ConnectOptions::default()).ladder,
        crate::reconnect::Ladder::LOCAL_UPGRADE
    );
    let custom = crate::reconnect::Ladder::flat(Duration::from_secs(2));
    let options = ConnectOptions {
        ladder: custom,
        ..ConnectOptions::default()
    };
    assert_eq!(options_for(&target, options).ladder, custom);
}

struct WriteFixture {
    io: Io,
    // Keep the unread peer alive so the writer waits instead of failing.
    peer: tokio::io::DuplexStream,
    shared: Shared,
    signals: Signals,
    close: watch::Sender<bool>,
    rebind: watch::Sender<u64>,
}

impl WriteFixture {
    fn new(capacity: usize) -> Self {
        let (stream, peer) = tokio::io::duplex(capacity);
        let (reader, writer) = tokio::io::split(stream);
        let (close, close_rx) = watch::channel(false);
        let (rebind, rebind_rx) = watch::channel(0);
        Self {
            io: Io::stream(Box::new(reader), Box::new(writer)),
            peer,
            shared: Arc::new(Mutex::new(ControlPlane::new(ControlOptions::default()))),
            signals: Signals {
                outbound: Arc::new(Notify::new()),
                resync: watch::channel(0).1,
                nudge: watch::channel(0).1,
                rebind: rebind_rx,
                close: close_rx,
            },
            close,
            rebind,
        }
    }
}

/// Start a pump on `fixture`'s socket as `$name`, polled once so it has
/// written its opening, and bind the pinned run future to `$running`.
macro_rules! start_pump {
    ($running:ident, $fixture:ident, $name:literal) => {
        let wake: Wake = Arc::new(|| {});
        let mut pump = Pump::new(
            $name,
            &mut $fixture.io,
            ConnectOptions::default(),
            &$fixture.shared,
            &mut $fixture.signals,
            &wake,
        );
        let $running = pump.run();
        tokio::pin!($running);
        assert!($running.as_mut().now_or_never().is_none());
    };
}

/// The frame type bytes written to `peer` so far, without waiting.
fn written_types(mut peer: tokio::io::DuplexStream) -> Vec<u8> {
    use tokio::io::AsyncReadExt as _;
    let mut written = BytesMut::with_capacity(4096);
    while let Some(Ok(read)) = peer.read_buf(&mut written).now_or_never() {
        if read == 0 {
            break;
        }
    }
    let mut types = Vec::new();
    while let Ok(Some(frame)) = phux_protocol::wire::framing::split_frame(&mut written) {
        types.push(frame[LENGTH_PREFIX_LEN]);
    }
    types
}

/// Between dials there is no socket to migrate, so a path change ends
/// the backoff wait the way a nudge does instead of being dropped.
#[tokio::test(start_paused = true)]
async fn a_path_change_cuts_a_backoff_wait_short() {
    let mut fixture = WriteFixture::new(1024);
    let wake: Wake = Arc::new(|| {});
    let waiting = wait_to_retry(
        Duration::from_secs(60),
        &fixture.shared,
        &mut fixture.signals,
        &wake,
    );
    tokio::pin!(waiting);
    assert!(waiting.as_mut().now_or_never().is_none());
    fixture.rebind.send_modify(|n| *n += 1);
    assert!(!waiting.await, "a path change redials; it never closes");
}

/// A refused token ends even an attached window; a lost link redials.
#[test]
fn only_a_refused_token_ends_the_session_from_the_link() {
    let refused = LinkError::Closed(DialError::AuthRefused("unauthorized".to_owned()));
    match link_end("mini", &refused) {
        ConnectionEnd::Refused(message) => assert!(message.contains("re-pair"), "{message}"),
        other => panic!("a refused token is final, got {other:?}"),
    }
    let closed = LinkError::Closed(DialError::Connect("closed by peer".to_owned()));
    assert!(matches!(
        link_end("mini", &closed),
        ConnectionEnd::Dropped(Some(_))
    ));
    let lost = LinkError::Read(std::io::ErrorKind::ConnectionReset.into());
    assert!(matches!(
        link_end("mini", &lost),
        ConnectionEnd::Dropped(Some(_))
    ));
    // No redial gets past ssh refusing the host key.
    let host_key = LinkError::Read(std::io::Error::other(
        "stdio bridge closed (exit status: 255): Host key verification failed.",
    ));
    assert!(matches!(
        link_end("mini", &host_key),
        ConnectionEnd::Refused(_)
    ));
}

#[tokio::test]
async fn close_interrupts_a_blocked_opening_write() {
    // One byte of capacity cannot hold HELLO. Polling once gets the
    // opening write stuck, without sleeps or a real socket's buffers.
    let mut fixture = WriteFixture::new(1);
    start_pump!(running, fixture, "blocked-hello");
    assert_eq!(lock(&fixture.shared).connection_epoch(), 1);
    fixture.close.send(true).expect("driver receives close");
    let end = tokio::time::timeout(Duration::from_secs(1), running)
        .await
        .expect("close must interrupt the blocked HELLO");
    assert!(matches!(end, ConnectionEnd::Closed));
}

#[tokio::test]
async fn close_interrupts_a_blocked_outbound_flush() {
    // HELLO fits, then a batch of pings fills the unread peer's buffer.
    let mut fixture = WriteFixture::new(1024);
    let outbound = Arc::clone(&fixture.signals.outbound);
    start_pump!(running, fixture, "blocked-flush");
    {
        let mut control = lock(&fixture.shared);
        for nonce in 0..1024 {
            control.queue_frame(&FrameKind::Ping { nonce });
        }
    }
    outbound.notify_one();
    assert!(running.as_mut().now_or_never().is_none());
    assert!(
        !lock(&fixture.shared).has_outbound(),
        "flush took the batch"
    );
    fixture.close.send(true).expect("driver receives close");
    let end = tokio::time::timeout(Duration::from_secs(1), running)
        .await
        .expect("close must interrupt a selected outbound write");
    assert!(matches!(end, ConnectionEnd::Closed));
}

#[tokio::test(start_paused = true)]
async fn a_send_that_makes_no_progress_drops_the_link() {
    let mut fixture = WriteFixture::new(1);
    start_pump!(running, fixture, "stalled-send");
    tokio::time::advance(SEND_BUDGET).await;
    let end = running
        .as_mut()
        .now_or_never()
        .expect("a shut window must end the attempt");
    match end {
        ConnectionEnd::Dropped(Some(message)) if message == "send stalled" => {}
        other => panic!("a shut window must drop the attempt, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn a_quiet_attached_session_probes() {
    let mut fixture = WriteFixture::new(64 * 1024);
    let outbound = Arc::clone(&fixture.signals.outbound);
    start_pump!(running, fixture, "quiet");
    lock(&fixture.shared).set_status_for_test(Status::Attached);
    outbound.notify_one();
    assert!(running.as_mut().now_or_never().is_none());
    tokio::time::advance(QUIET_PROBE).await;
    assert!(running.as_mut().now_or_never().is_none());

    let types = written_types(fixture.peer);
    assert!(
        types.contains(&TYPE_PING),
        "a quiet attached session must probe; wrote {types:?}"
    );
}

/// An answered probe publishes its round trip: off QUIC, the time since the
/// probe went out. A new connection forgets the last one.
#[tokio::test(start_paused = true)]
async fn an_answered_probe_publishes_its_round_trip() {
    use tokio::io::AsyncWriteExt as _;
    let mut fixture = WriteFixture::new(64 * 1024);
    start_pump!(running, fixture, "round-trip");
    fixture.rebind.send_modify(|n| *n += 1);
    assert!(running.as_mut().now_or_never().is_none());
    assert_eq!(lock(&fixture.shared).round_trip(), None);

    tokio::time::advance(Duration::from_millis(40)).await;
    let mut peer = fixture.peer;
    peer.write_all(&encode(&FrameKind::Pong { nonce: 1 }))
        .await
        .unwrap();
    assert!(running.as_mut().now_or_never().is_none());
    assert_eq!(
        lock(&fixture.shared).round_trip(),
        Some(Duration::from_millis(40))
    );
    lock(&fixture.shared).connection_opened();
    assert_eq!(lock(&fixture.shared).round_trip(), None);
}
