//! Backpressure through the PRODUCTION relay (phux-5y0f).
//!
//! When the consumer's link is the slow hop, the relay must neither absorb
//! the backlog nor hide it from the server. Its consumer-facing send tracks
//! the consumer connection's congestion window, and each tunnel stream gets a
//! small per-stream receive window — the tunnel dials with a tagged initial
//! connection ID, so the relay picks that config before it accepts — so the
//! server's writer blocks after a bounded number of bytes. That is what lets
//! the server's output pump go stale and resync the consumer, exactly as on a
//! direct QUIC link. The bound is per consumer: a stalled consumer never
//! holds back another on the same route, and consumer uploads keep quinn's
//! default credit.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use phux_dial::testing::DropProxy;
use phux_relay::DEFAULT_MAX_CONNS;
use tokio::time::{sleep, timeout};

use crate::common::{
    CONSUMER_TOKEN, WIRE_RECV_TIMEOUT, await_route_live, dial_consumer, dial_tunnel_raw, mint,
    read_preamble, spawn_relay,
};

const ROUTE: &str = "backpressure";

/// Upper bound on what the server side of the tunnel may write into a stalled
/// consumer: the tunnel stream's 64 KiB receive window at the relay, its
/// 8 KiB splice buffer, and one tracked send window toward the consumer
/// (about 31 KiB on a fresh connection), rounded up generously. quinn's defaults let the same
/// write run to about 2.5 MB: 1.25 MB of relay credit plus 1.25 MB of
/// consumer credit.
const MAX_STALLED_BACKLOG: usize = 256 * 1024;

#[tokio::test]
async fn a_stalled_consumer_blocks_the_server_after_a_bounded_backlog() {
    let dir = tempfile::tempdir().unwrap();
    let relay = spawn_relay(dir.path(), DEFAULT_MAX_CONNS).await;
    let token = mint(&relay.tokens_path, ROUTE);

    // The test holds the server side of the tunnel itself, writing with a
    // plain (untracked) quinn stream: only the relay's credit bounds it.
    let (_endpoint, tunnel, _send0, _recv0) =
        dial_tunnel_raw(relay.addr, &relay.fingerprint, ROUTE, Some(token))
            .await
            .expect("tunnel leg establishes");
    await_route_live(relay.addr, &relay.fingerprint, ROUTE).await;

    // The consumer dials through a proxy that can cut relay-to-consumer.
    let proxy = DropProxy::start(relay.addr).await.expect("proxy");
    let consumer = dial_consumer(proxy.addr(), &relay.fingerprint, ROUTE)
        .await
        .expect("consumer dials through the proxy");
    let (mut tun_send, mut tun_recv) = timeout(WIRE_RECV_TIMEOUT, tunnel.accept_bi())
        .await
        .expect("bridge stream within deadline")
        .expect("bridge stream");
    assert_eq!(
        read_preamble(&mut tun_recv).await.as_deref(),
        Some(CONSUMER_TOKEN),
        "the consumer's bearer crosses the relay opaquely"
    );

    proxy.blackhole_downstream();
    let chunk = vec![0x5a_u8; 64 * 1024];
    let mut accepted = 0_usize;
    while accepted < 8 * 1024 * 1024 {
        match timeout(Duration::from_millis(500), tun_send.write(&chunk)).await {
            Ok(Ok(written)) => accepted += written,
            Ok(Err(err)) => panic!("tunnel write failed: {err}"),
            Err(_) => break,
        }
    }
    assert!(
        accepted <= MAX_STALLED_BACKLOG,
        "the relay let the server queue {accepted} bytes toward a stalled consumer"
    );

    drop(consumer);
    relay.shutdown().await;
}

/// Lower bound on what a consumer may upload toward a server that reads
/// nothing: the relay's default stream credit toward the consumer (1.25 MB)
/// plus the server's own credit toward the relay (1.25 MB), less generous
/// slack. Were the consumer leg held to the tunnel's 64 KiB, the same write
/// would stop near 1.3 MB.
const MIN_CONSUMER_UPLOAD: usize = 2 * 1024 * 1024;

/// Only tunnels are bounded: a consumer's upload — a large paste — keeps
/// quinn's default credit at the relay instead of the tunnel's 64 KiB, so it
/// is not paced at 64 KiB per round trip.
#[tokio::test]
async fn a_consumer_upload_is_not_held_to_the_tunnel_window() {
    let dir = tempfile::tempdir().unwrap();
    let relay = spawn_relay(dir.path(), DEFAULT_MAX_CONNS).await;
    let token = mint(&relay.tokens_path, ROUTE);
    let (_endpoint, tunnel, _send0, _recv0) =
        dial_tunnel_raw(relay.addr, &relay.fingerprint, ROUTE, Some(token))
            .await
            .expect("tunnel leg establishes");
    await_route_live(relay.addr, &relay.fingerprint, ROUTE).await;

    let mut consumer = dial_consumer(relay.addr, &relay.fingerprint, ROUTE)
        .await
        .expect("consumer dials the relay");
    // The server side reads the bearer preamble, then nothing more, but
    // keeps both halves open: the upload can only fill flow-control credit.
    let (_tun_send, mut tun_recv) = timeout(WIRE_RECV_TIMEOUT, tunnel.accept_bi())
        .await
        .expect("bridge stream within deadline")
        .expect("bridge stream");
    assert_eq!(
        read_preamble(&mut tun_recv).await.as_deref(),
        Some(CONSUMER_TOKEN)
    );

    let chunk = vec![0xa5_u8; 64 * 1024];
    let mut accepted = 0_usize;
    while accepted < 16 * 1024 * 1024 {
        match timeout(Duration::from_millis(500), consumer.send.write(&chunk)).await {
            Ok(Ok(written)) => accepted += written,
            Ok(Err(err)) => panic!("consumer write failed: {err}"),
            Err(_) => break,
        }
    }
    assert!(
        accepted >= MIN_CONSUMER_UPLOAD,
        "the relay held a consumer upload to {accepted} bytes of credit"
    );

    drop(consumer);
    relay.shutdown().await;
}

/// Write to a tunnel stream until it errors, counting what quinn accepted.
async fn flood(mut send: quinn::SendStream, written: Arc<AtomicUsize>) {
    let chunk = [0x77_u8; 16 * 1024];
    while send.write_all(&chunk).await.is_ok() {
        written.fetch_add(chunk.len(), Ordering::SeqCst);
    }
}

/// Wait until `written` stops growing for half a second: the stream it
/// counts is saturated.
async fn until_saturated(written: &AtomicUsize) {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    let mut last = written.load(Ordering::SeqCst);
    while tokio::time::Instant::now() < deadline {
        sleep(Duration::from_millis(500)).await;
        let now = written.load(Ordering::SeqCst);
        if now == last && now > 0 {
            return;
        }
        last = now;
    }
    panic!("the stalled consumer's tunnel stream never saturated");
}

/// Minimum a healthy consumer must receive in [`HEALTHY_WINDOW`] while
/// another consumer on its route is stalled. Loopback moves many times this;
/// a head-of-line freeze moves nothing.
const MIN_HEALTHY_BYTES: usize = 1024 * 1024;

/// How long the healthy consumer is watched after the stall saturates.
const HEALTHY_WINDOW: Duration = Duration::from_secs(2);

/// One consumer on a route stops reading (its QUIC stack still acks, as a
/// hung UI or an agent that never drains would); another on the same route
/// must keep receiving. The bound is per tunnel stream, so the stalled one
/// can only ever hold its own window.
#[tokio::test]
async fn a_stalled_consumer_does_not_freeze_another_on_the_same_route() {
    let dir = tempfile::tempdir().unwrap();
    let relay = spawn_relay(dir.path(), DEFAULT_MAX_CONNS).await;
    let token = mint(&relay.tokens_path, ROUTE);
    let (_endpoint, tunnel, _send0, _recv0) =
        dial_tunnel_raw(relay.addr, &relay.fingerprint, ROUTE, Some(token))
            .await
            .expect("tunnel leg establishes");
    await_route_live(relay.addr, &relay.fingerprint, ROUTE).await;

    let stalled = dial_consumer(relay.addr, &relay.fingerprint, ROUTE)
        .await
        .expect("stalled consumer dials");
    let (stalled_send, mut stalled_recv) = timeout(WIRE_RECV_TIMEOUT, tunnel.accept_bi())
        .await
        .expect("first bridge within deadline")
        .expect("first bridge stream");
    assert_eq!(
        read_preamble(&mut stalled_recv).await.as_deref(),
        Some(CONSUMER_TOKEN)
    );
    let healthy = dial_consumer(relay.addr, &relay.fingerprint, ROUTE)
        .await
        .expect("healthy consumer dials");
    let (healthy_send, mut healthy_recv) = timeout(WIRE_RECV_TIMEOUT, tunnel.accept_bi())
        .await
        .expect("second bridge within deadline")
        .expect("second bridge stream");
    assert_eq!(
        read_preamble(&mut healthy_recv).await.as_deref(),
        Some(CONSUMER_TOKEN)
    );

    // The server side floods both consumers; only the healthy one reads.
    let stalled_written = Arc::new(AtomicUsize::new(0));
    let healthy_written = Arc::new(AtomicUsize::new(0));
    let stalled_flood = tokio::spawn(flood(stalled_send, Arc::clone(&stalled_written)));
    let healthy_flood = tokio::spawn(flood(healthy_send, Arc::clone(&healthy_written)));
    let received = Arc::new(AtomicUsize::new(0));
    let reader_count = Arc::clone(&received);
    let mut reader_recv = healthy.recv;
    let reader = tokio::spawn(async move {
        let mut buf = vec![0_u8; 64 * 1024];
        while let Ok(Some(n)) = reader_recv.read(&mut buf).await {
            reader_count.fetch_add(n, Ordering::SeqCst);
        }
    });

    until_saturated(&stalled_written).await;
    let before = received.load(Ordering::SeqCst);
    sleep(HEALTHY_WINDOW).await;
    let moved = received.load(Ordering::SeqCst) - before;
    assert!(
        moved >= MIN_HEALTHY_BYTES,
        "a stalled consumer froze its route: the healthy consumer received only {moved} bytes \
         in {HEALTHY_WINDOW:?} (stalled stream holds {} bytes written)",
        stalled_written.load(Ordering::SeqCst)
    );

    stalled_flood.abort();
    healthy_flood.abort();
    reader.abort();
    drop(stalled);
    relay.shutdown().await;
}
