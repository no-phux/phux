//! Tunnel admission against the PRODUCTION relay (ADR-0052 Decision 2,
//! ADR-0057 close-code semantics): a minted route token admits a tunnel
//! end-to-end; a wrong token is refused with `AUTH_FAILED` and never
//! wedges the endpoint; a stalled preamble is bounded by the deadline;
//! and relay admission is never consumer authorization.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

mod common;

use phux_relay::{AUTH_FAILED_CODE, DEFAULT_MAX_CONNS, ROUTE_OFFLINE_CODE};
use tokio::time::timeout;

use crate::common::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, assert_app_closed, await_route_live, dial_consumer,
    dial_consumer_with_bearer, dial_tunnel_raw, echo_when_ready, expect_post_handshake_close, mint,
    spawn_connector, spawn_relay,
};

/// Requirement 1 (mint -> tunnel-auth roundtrip): a token minted through
/// the production library fn authenticates a connector tunnel, and a
/// consumer's bytes round-trip through it. Minting AFTER the relay is
/// already running also proves the outline's pair-while-running liveness:
/// the store is re-read per handshake, no restart needed.
#[tokio::test]
async fn enrolled_token_admits_tunnel_and_serves_consumers() {
    let dir = tempfile::tempdir().unwrap();
    let relay = spawn_relay(dir.path(), DEFAULT_MAX_CONNS).await;
    let token = mint(&relay.tokens_path, "alpha");

    let connector = spawn_connector(relay.addr, &relay.fingerprint, "alpha", token, b"A:").await;
    await_route_live(relay.addr, &relay.fingerprint, "alpha").await;

    let mut consumer = dial_consumer(relay.addr, &relay.fingerprint, "alpha")
        .await
        .expect("consumer dials the live route");
    common::expect_echo(&mut consumer, b"A:", b"payload-alpha").await;

    assert_eq!(connector.bridged(), 1, "exactly one consumer bridged");
    assert_eq!(connector.rejected(), 0);
}

/// A connector presenting a token that is not in the store is refused with
/// an application close carrying `AUTH_FAILED` (0x01) and the reason
/// `unauthorized`; no tunnel registers (a consumer then sees
/// `ROUTE_OFFLINE`), and the endpoint survives — a correctly-tokened
/// connector registers and serves right after.
#[tokio::test]
async fn wrong_tunnel_token_refused_with_auth_failed() {
    let dir = tempfile::tempdir().unwrap();
    let relay = spawn_relay(dir.path(), DEFAULT_MAX_CONNS).await;
    let token = mint(&relay.tokens_path, "alpha");

    let (_ep, conn, _send0, _recv0) = dial_tunnel_raw(
        relay.addr,
        &relay.fingerprint,
        "alpha",
        Some(vec![0xEE; 32]),
    )
    .await
    .expect("TLS + ALPN succeed; the refusal is at the token layer");
    let err = timeout(SOCKET_CONNECT_DEADLINE, conn.closed())
        .await
        .expect("relay must close promptly, not idle the connection out");
    assert_app_closed(&err, AUTH_FAILED_CODE, "wrong tunnel token");
    if let quinn::ConnectionError::ApplicationClosed(app) = &err {
        assert_eq!(app.reason.as_ref(), b"unauthorized");
    }

    // No tunnel registered: the enrolled route is offline for consumers.
    let refused = dial_consumer(relay.addr, &relay.fingerprint, "alpha").await;
    expect_post_handshake_close(refused, ROUTE_OFFLINE_CODE, "route offline").await;

    // Per-connection failure isolation: the listener survived, and the
    // real token still admits a tunnel that serves.
    let connector = spawn_connector(relay.addr, &relay.fingerprint, "alpha", token, b"OK:").await;
    await_route_live(relay.addr, &relay.fingerprint, "alpha").await;
    let mut consumer = dial_consumer(relay.addr, &relay.fingerprint, "alpha")
        .await
        .expect("consumer dials after the refused connector");
    common::expect_echo(&mut consumer, b"OK:", b"after-refusal").await;
    assert_eq!(connector.bridged(), 1);
}

/// A connector that opens stream 0 and stalls mid-preamble neither wedges
/// the accept loop (a well-behaved route round-trips concurrently) nor
/// holds its connection: the preamble deadline refuses it with
/// `AUTH_FAILED`.
#[tokio::test]
async fn stalled_preamble_does_not_wedge_relay() {
    let dir = tempfile::tempdir().unwrap();
    let relay = spawn_relay(dir.path(), DEFAULT_MAX_CONNS).await;
    let alpha_token = mint(&relay.tokens_path, "alpha");
    mint(&relay.tokens_path, "beta");

    // The staller: tunnel leg with NO preamble written by the dialer, then
    // only the 4-byte length prefix — read_exact of the token now pends.
    let (_ep, stall_conn, mut stall_send, _stall_recv) =
        dial_tunnel_raw(relay.addr, &relay.fingerprint, "beta", None)
            .await
            .expect("staller establishes");
    stall_send
        .write_all(&32u32.to_be_bytes())
        .await
        .expect("write partial preamble");

    // Concurrently, a well-behaved connector + consumer complete a full
    // round-trip on another route: one silent tunnel blocks nothing.
    let connector =
        spawn_connector(relay.addr, &relay.fingerprint, "alpha", alpha_token, b"A:").await;
    await_route_live(relay.addr, &relay.fingerprint, "alpha").await;
    let mut consumer = dial_consumer(relay.addr, &relay.fingerprint, "alpha")
        .await
        .expect("consumer dials while the staller is pending");
    common::expect_echo(&mut consumer, b"A:", b"unblocked").await;
    assert_eq!(connector.bridged(), 1);

    // The staller is refused at the preamble deadline (5s), bounded.
    let err = timeout(SOCKET_CONNECT_DEADLINE, stall_conn.closed())
        .await
        .expect("staller must be refused at the deadline, not parked forever");
    assert_app_closed(&err, AUTH_FAILED_CODE, "stalled preamble");
}

/// Relay admission is not consumer authorization (ADR-0051 Decision 4):
/// a consumer with a wrong bearer is bridged opaquely by the relay, then
/// refused per-stream by the connector (the server side of the tunnel).
/// The tunnel survives: a good consumer on the SAME tunnel succeeds.
#[tokio::test]
async fn bad_consumer_bearer_rejected_per_stream_tunnel_survives() {
    let dir = tempfile::tempdir().unwrap();
    let relay = spawn_relay(dir.path(), DEFAULT_MAX_CONNS).await;
    let token = mint(&relay.tokens_path, "alpha");
    let connector = spawn_connector(relay.addr, &relay.fingerprint, "alpha", token, b"A:").await;
    await_route_live(relay.addr, &relay.fingerprint, "alpha").await;

    let mut bad = dial_consumer_with_bearer(
        relay.addr,
        &relay.fingerprint,
        "alpha",
        Some(b"wrong-consumer-bearer-0123456789".to_vec()),
    )
    .await
    .expect("the relay admits the bad-bearer consumer (it never reads bearers)");
    // The connector resets the bridged stream: the consumer's read ends
    // boundedly with ZERO payload bytes (the tag never arrives).
    let mut buf = [0u8; 1];
    let outcome = timeout(WIRE_RECV_TIMEOUT, bad.recv.read(&mut buf))
        .await
        .expect("refused stream ends boundedly");
    match outcome {
        Ok(None) | Err(_) => {}
        Ok(Some(n)) => panic!("refused consumer must never receive payload bytes, got {n}"),
    }

    // Same tunnel, good bearer: served. Counts prove per-stream isolation.
    let mut good = dial_consumer(relay.addr, &relay.fingerprint, "alpha")
        .await
        .expect("good consumer dials the surviving tunnel");
    common::expect_echo(&mut good, b"A:", b"after-reject").await;
    assert_eq!(connector.bridged(), 1, "one admitted consumer");
    assert_eq!(connector.rejected(), 1, "one refused bearer");
}

/// The token-route bijection decides which route a tunnel claims; the
/// connector's dial SNI is not load-bearing (ADR-0052 Decision 2 as the
/// outline resolves it). A connector dialing with SNI `beta` but
/// presenting alpha's token registers on ALPHA — and beta stays offline.
#[tokio::test]
async fn tunnel_binds_to_its_tokens_route_not_its_dial_sni() {
    let dir = tempfile::tempdir().unwrap();
    let relay = spawn_relay(dir.path(), DEFAULT_MAX_CONNS).await;
    let alpha_token = mint(&relay.tokens_path, "alpha");
    mint(&relay.tokens_path, "beta");

    let connector =
        spawn_connector(relay.addr, &relay.fingerprint, "beta", alpha_token, b"A:").await;
    await_route_live(relay.addr, &relay.fingerprint, "alpha").await;

    // Consumers naming alpha reach the tunnel; beta has no live tunnel.
    let mut consumer =
        echo_when_ready(relay.addr, &relay.fingerprint, "alpha", b"A:", b"via-alpha").await;
    common::expect_echo(&mut consumer, b"", b"more").await;
    assert_eq!(connector.bridged(), 1);

    let beta = dial_consumer(relay.addr, &relay.fingerprint, "beta").await;
    expect_post_handshake_close(beta, ROUTE_OFFLINE_CODE, "route offline").await;
}
