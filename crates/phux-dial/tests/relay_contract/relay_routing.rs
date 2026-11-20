//! SNI routing against the production relay (ADR-0052 Decision 1): routes
//! never cross-talk, and unknown or absent SNI is refused at TLS.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_dial::relay::DEFAULT_MAX_CONNS;
use tokio::time::sleep;

use crate::common::{contains_subslice, expect_echo, mint, spawn_relay};

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

    let conn_a = relay.connector("alpha", alpha_token, b"A:").await;
    let conn_b = relay.connector("beta", beta_token, b"B:").await;
    relay.await_route_live("alpha").await;
    relay.await_route_live("beta").await;

    let mut consumer_a = relay.consumer("alpha").await.expect("consumer dials alpha");
    let mut consumer_b = relay.consumer("beta").await.expect("consumer dials beta");
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
    let connector = relay.connector("alpha", token, b"A:").await;
    relay.await_route_live("alpha").await;

    for sni in ["nosuch", "127.0.0.1"] {
        let refused = relay.consumer(sni).await;
        assert!(refused.is_err(), "SNI {sni:?} must fail the handshake");
    }

    // Settle in-flight relay work, then prove nothing reached the tunnel.
    sleep(Duration::from_millis(100)).await;
    assert_eq!(connector.streams_seen(), 0, "no bridged stream ever opened");
    assert_eq!(connector.bridged(), 0);
    assert_eq!(connector.rejected(), 0);
    assert!(connector.tapped_bytes().is_empty());
}
