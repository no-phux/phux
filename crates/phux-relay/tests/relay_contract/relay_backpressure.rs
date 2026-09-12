//! Backpressure through the PRODUCTION relay (phux-5y0f).
//!
//! When the consumer's link is the slow hop, the relay must neither absorb
//! the backlog nor hide it from the server. Its consumer-facing send tracks
//! the consumer connection's congestion window, and it grants each stream a
//! small receive window, so the server's writer blocks after a bounded number
//! of bytes — which is what lets the server's output pump go stale and resync
//! the consumer, exactly as on a direct QUIC link.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_dial::testing::DropProxy;
use phux_relay::DEFAULT_MAX_CONNS;
use tokio::time::timeout;

use crate::common::{
    CONSUMER_TOKEN, WIRE_RECV_TIMEOUT, await_route_live, dial_consumer, dial_tunnel_raw, mint,
    read_preamble, spawn_relay,
};

const ROUTE: &str = "backpressure";

/// Upper bound on what the server side of the tunnel may write into a stalled
/// consumer: the relay's 64 KiB stream receive window, its 8 KiB splice
/// buffer, and one tracked send window toward the consumer (about 31 KiB on a
/// fresh connection), rounded up generously. quinn's defaults let the same
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
