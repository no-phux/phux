//! SNI routing against the production relay (ADR-0052 Decision 1): routes
//! never cross-talk, unknown or absent SNI is refused at TLS, and an enrolled
//! route with no live tunnel is refused with `ROUTE_OFFLINE` after the
//! handshake.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_relay::{DEFAULT_MAX_CONNS, ROUTE_OFFLINE_CODE};
use tokio::time::sleep;

use crate::common::{
    await_route_live, contains_subslice, dial_consumer, expect_echo, expect_post_handshake_close,
    mint, spawn_connector, spawn_relay,
};

const PAYLOAD_A: &[u8] = b"payload-for-route-alpha";
const PAYLOAD_B: &[u8] = b"payload-for-route-beta";

/// Two routes, two connectors: each tap holds its own consumer's payload
/// and never the other's.
#[tokio::test]
async fn two_routes_no_crosstalk() {
    let dir = tempfile::tempdir().unwrap();
    let relay = spawn_relay(dir.path(), DEFAULT_MAX_CONNS).await;
    let alpha_token = mint(&relay.tokens_path, "alpha");
    let beta_token = mint(&relay.tokens_path, "beta");

    let conn_a = spawn_connector(relay.addr, &relay.fingerprint, "alpha", alpha_token, b"A:").await;
    let conn_b = spawn_connector(relay.addr, &relay.fingerprint, "beta", beta_token, b"B:").await;
    await_route_live(relay.addr, &relay.fingerprint, "alpha").await;
    await_route_live(relay.addr, &relay.fingerprint, "beta").await;

    let mut consumer_a = dial_consumer(relay.addr, &relay.fingerprint, "alpha")
        .await
        .expect("consumer dials alpha");
    let mut consumer_b = dial_consumer(relay.addr, &relay.fingerprint, "beta")
        .await
        .expect("consumer dials beta");
    expect_echo(&mut consumer_a, b"A:", PAYLOAD_A).await;
    expect_echo(&mut consumer_b, b"B:", PAYLOAD_B).await;

    assert_eq!(conn_a.bridged(), 1, "alpha bridged exactly its consumer");
    assert_eq!(conn_b.bridged(), 1, "beta bridged exactly its consumer");

    // Byte-level: each tap holds its own payload, never the other's.
    let taps_a = conn_a.tapped_bytes();
    let taps_b = conn_b.tapped_bytes();
    assert_eq!(taps_a.len(), 1);
    assert_eq!(taps_b.len(), 1);
    assert!(contains_subslice(&taps_a[0], PAYLOAD_A));
    assert!(
        !contains_subslice(&taps_a[0], PAYLOAD_B),
        "beta's payload must never appear on alpha's tunnel"
    );
    assert!(contains_subslice(&taps_b[0], PAYLOAD_B));
    assert!(
        !contains_subslice(&taps_b[0], PAYLOAD_A),
        "alpha's payload must never appear on beta's tunnel"
    );
}

/// A consumer naming an unknown route, or sending no SNI at all (an IP
/// literal server name sends none), is refused during the TLS handshake and
/// zero bytes reach any tunnel.
#[tokio::test]
async fn unknown_or_absent_sni_refused_no_bytes_reach_any_tunnel() {
    let dir = tempfile::tempdir().unwrap();
    let relay = spawn_relay(dir.path(), DEFAULT_MAX_CONNS).await;
    let token = mint(&relay.tokens_path, "alpha");
    let connector = spawn_connector(relay.addr, &relay.fingerprint, "alpha", token, b"A:").await;
    await_route_live(relay.addr, &relay.fingerprint, "alpha").await;

    for sni in ["nosuch", "127.0.0.1"] {
        let refused = dial_consumer(relay.addr, &relay.fingerprint, sni).await;
        assert!(refused.is_err(), "SNI {sni:?} must fail the handshake");
    }

    // Settle in-flight relay work, then prove nothing reached the tunnel.
    sleep(Duration::from_millis(100)).await;
    assert_eq!(connector.streams_seen(), 0, "no bridged stream ever opened");
    assert_eq!(connector.bridged(), 0);
    assert_eq!(connector.rejected(), 0);
    assert!(connector.tapped_bytes().is_empty());
}

/// An enrolled route with no live tunnel completes the handshake and is
/// closed `ROUTE_OFFLINE`; once a connector arrives, a retry succeeds.
#[tokio::test]
async fn consumer_before_tunnel_gets_route_offline_then_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let relay = spawn_relay(dir.path(), DEFAULT_MAX_CONNS).await;
    let token = mint(&relay.tokens_path, "alpha");

    // Enrolled route, no connector yet: bounded post-handshake refusal.
    let refused = dial_consumer(relay.addr, &relay.fingerprint, "alpha").await;
    expect_post_handshake_close(refused, ROUTE_OFFLINE_CODE, "route offline").await;

    // The connector arrives; the retry succeeds through it.
    let connector = spawn_connector(relay.addr, &relay.fingerprint, "alpha", token, b"A:").await;
    await_route_live(relay.addr, &relay.fingerprint, "alpha").await;
    let mut consumer = dial_consumer(relay.addr, &relay.fingerprint, "alpha")
        .await
        .expect("retry dials once the tunnel is live");
    expect_echo(&mut consumer, b"A:", b"recovered").await;
    assert_eq!(connector.bridged(), 1);
}
