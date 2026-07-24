//! Tunnel and token lifecycle against the PRODUCTION relay (ADR-0057
//! outline semantics): a redial REPLACES the live tunnel (last-writer-wins
//! `RECLAIMED`), a re-mint REPLACES the route's token (revoking the old
//! one at the next handshake), consumers on a lost tunnel fail boundedly,
//! many concurrent consumers share one route, and the `--max-conns` cap
//! refuses over-cap connections with `OVER_CAP` without touching existing
//! ones.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

mod common;

use phux_relay::{
    AUTH_FAILED_CODE, DEFAULT_MAX_CONNS, OVER_CAP_CODE, PROTOCOL_VIOLATION_CODE, RECLAIMED_CODE,
};
use tokio::time::timeout;

use crate::common::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, assert_app_closed, await_route_live, dial_consumer,
    dial_tunnel_raw, echo_when_ready, expect_echo, expect_post_handshake_close, mint,
    spawn_connector, spawn_relay,
};

/// Requirement 5, pinned explicitly: a second connector claiming the same
/// route while the first is still alive WINS. The incumbent is closed with
/// `RECLAIMED` (0x03) and consumers land on the new tunnel.
#[tokio::test]
async fn redial_while_old_alive_new_wins_old_sees_reclaimed() {
    let dir = tempfile::tempdir().unwrap();
    let relay = spawn_relay(dir.path(), DEFAULT_MAX_CONNS).await;
    let token = mint(&relay.tokens_path, "alpha");

    let conn1 = spawn_connector(
        relay.addr,
        &relay.fingerprint,
        "alpha",
        token.clone(),
        b"ONE:",
    )
    .await;
    await_route_live(relay.addr, &relay.fingerprint, "alpha").await;
    let mut consumer1 = dial_consumer(relay.addr, &relay.fingerprint, "alpha")
        .await
        .expect("first consumer dials");
    expect_echo(&mut consumer1, b"ONE:", b"via-one").await;

    // Redial while the incumbent is alive: last-writer-wins.
    let conn2 = spawn_connector(relay.addr, &relay.fingerprint, "alpha", token, b"TWO:").await;
    let err = conn1.closed().await;
    assert_app_closed(&err, RECLAIMED_CODE, "incumbent tunnel reclaimed");
    if let quinn::ConnectionError::ApplicationClosed(app) = &err {
        assert_eq!(app.reason.as_ref(), b"superseded by a newer tunnel claim");
    }

    // The claim already swapped the registry entry, so new consumers land
    // on connector 2 — the tag proves which tunnel served.
    let mut consumer2 = dial_consumer(relay.addr, &relay.fingerprint, "alpha")
        .await
        .expect("consumer dials after the reclaim");
    expect_echo(&mut consumer2, b"TWO:", b"via-two").await;
    assert_eq!(conn2.bridged(), 1, "new tunnel served the new consumer");
    assert_eq!(conn1.bridged(), 1, "old tunnel saw no post-reclaim stream");
}

/// A consumer mid-bridge when its tunnel dies gets a bounded end (the
/// splice's first-finished-direction rule), never a hang; a redial with
/// the same token restores service for new consumers.
#[tokio::test]
async fn consumer_mid_bridge_bounded_error_on_tunnel_loss_then_redial_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let relay = spawn_relay(dir.path(), DEFAULT_MAX_CONNS).await;
    let token = mint(&relay.tokens_path, "alpha");

    let conn1 = spawn_connector(
        relay.addr,
        &relay.fingerprint,
        "alpha",
        token.clone(),
        b"ONE:",
    )
    .await;
    await_route_live(relay.addr, &relay.fingerprint, "alpha").await;
    let mut consumer = dial_consumer(relay.addr, &relay.fingerprint, "alpha")
        .await
        .expect("consumer dials");
    expect_echo(&mut consumer, b"ONE:", b"alive").await;

    // Kill the tunnel under the live bridge.
    conn1.close();
    let mut buf = [0u8; 16];
    let outcome = timeout(WIRE_RECV_TIMEOUT, consumer.recv.read(&mut buf))
        .await
        .expect("bridge must end boundedly when the tunnel dies");
    assert!(
        matches!(outcome, Ok(None) | Err(_)),
        "expected EOF or a stream error after tunnel loss, got {outcome:?}"
    );

    // Redial with the same token; a new consumer is served. The retry
    // helper absorbs the registry-settling window after the dead claim.
    let conn2 = spawn_connector(relay.addr, &relay.fingerprint, "alpha", token, b"TWO:").await;
    let mut recovered =
        echo_when_ready(relay.addr, &relay.fingerprint, "alpha", b"TWO:", b"back").await;
    expect_echo(&mut recovered, b"", b"still-here").await;
    assert_eq!(conn2.bridged(), 1);
}

/// Requirement 6: eight concurrent consumers on one route, each seeing
/// exactly its own tagged echo; the connector bridged eight distinct
/// streams and no payload leaked across taps.
#[tokio::test]
async fn concurrent_consumers_one_route() {
    let dir = tempfile::tempdir().unwrap();
    let relay = spawn_relay(dir.path(), DEFAULT_MAX_CONNS).await;
    let token = mint(&relay.tokens_path, "alpha");
    let connector = spawn_connector(relay.addr, &relay.fingerprint, "alpha", token, b"E:").await;
    await_route_live(relay.addr, &relay.fingerprint, "alpha").await;

    let mut tasks = Vec::new();
    for i in 0..8 {
        let addr = relay.addr;
        let fingerprint = relay.fingerprint.clone();
        tasks.push(tokio::spawn(async move {
            let payload = format!("payload-{i}");
            let mut consumer = dial_consumer(addr, &fingerprint, "alpha")
                .await
                .expect("concurrent consumer dials");
            expect_echo(&mut consumer, b"E:", payload.as_bytes()).await;
            consumer.conn.close(0u32.into(), b"done");
        }));
    }
    for task in tasks {
        timeout(WIRE_RECV_TIMEOUT, task)
            .await
            .expect("consumer task within deadline")
            .expect("consumer task");
    }

    assert_eq!(connector.bridged(), 8, "every consumer was bridged");
    assert_eq!(connector.rejected(), 0);
    let mut ids = connector.state.lock().unwrap().bridged_stream_ids.clone();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 8, "each consumer got its own fresh stream");

    // Byte-level isolation: each payload appears in exactly one tap.
    let taps = connector.tapped_bytes();
    assert_eq!(taps.len(), 8);
    for i in 0..8 {
        let payload = format!("payload-{i}");
        let hits = taps
            .iter()
            .filter(|tap| common::contains_subslice(tap, payload.as_bytes()))
            .count();
        assert_eq!(hits, 1, "payload {i} must ride exactly one stream");
    }
}

/// The outline's replace-on-remint (token-route bijection): re-minting a
/// route rotates its token. The live tunnel is NOT torn down (revocation
/// is at the next handshake), the OLD token is refused with `AUTH_FAILED`
/// on its next dial, and the NEW token claims the route (reclaiming the
/// incumbent).
#[tokio::test]
async fn remint_replaces_token_and_invalidates_old_at_next_handshake() {
    let dir = tempfile::tempdir().unwrap();
    let relay = spawn_relay(dir.path(), DEFAULT_MAX_CONNS).await;
    let token1 = mint(&relay.tokens_path, "alpha");

    let conn1 = spawn_connector(
        relay.addr,
        &relay.fingerprint,
        "alpha",
        token1.clone(),
        b"ONE:",
    )
    .await;
    await_route_live(relay.addr, &relay.fingerprint, "alpha").await;

    // Rotate: the store now binds alpha to token2 only.
    let token2 = mint(&relay.tokens_path, "alpha");
    assert_ne!(token1, token2);

    // The established tunnel is untouched by the rotation: it still serves.
    let mut consumer = dial_consumer(relay.addr, &relay.fingerprint, "alpha")
        .await
        .expect("consumer dials the still-live tunnel");
    expect_echo(&mut consumer, b"ONE:", b"pre-rotation-tunnel").await;

    // The OLD token is dead at its next handshake.
    let (_ep, old_conn, _s0, _r0) =
        dial_tunnel_raw(relay.addr, &relay.fingerprint, "alpha", Some(token1))
            .await
            .expect("TLS + ALPN succeed; the refusal is at the token layer");
    let err = timeout(SOCKET_CONNECT_DEADLINE, old_conn.closed())
        .await
        .expect("old token refused promptly");
    assert_app_closed(&err, AUTH_FAILED_CODE, "old token after remint");

    // The NEW token claims the route, reclaiming the incumbent.
    let conn2 = spawn_connector(relay.addr, &relay.fingerprint, "alpha", token2, b"TWO:").await;
    let err = conn1.closed().await;
    assert_app_closed(&err, RECLAIMED_CODE, "incumbent reclaimed by new token");
    let mut consumer2 = dial_consumer(relay.addr, &relay.fingerprint, "alpha")
        .await
        .expect("consumer dials the rotated tunnel");
    expect_echo(&mut consumer2, b"TWO:", b"post-rotation").await;
    assert_eq!(conn2.bridged(), 1);
}

/// The outline's `--max-conns` cap: with `max_conns = 2` exhausted by one
/// tunnel and one live consumer bridge, the next connection completes its
/// handshake and is refused with `OVER_CAP` (0x05) — while the existing
/// tunnel and consumer keep working untouched.
#[tokio::test]
async fn over_cap_connection_refused_existing_unaffected() {
    let dir = tempfile::tempdir().unwrap();
    let relay = spawn_relay(dir.path(), 2).await;
    let token = mint(&relay.tokens_path, "alpha");

    // Slot 1: the tunnel. (No idle probe here — a probe would transiently
    // hold a cap slot; the retrying echo helper syncs on the claim
    // instead, and its failed pre-tunnel attempts release their slots
    // immediately.)
    let connector = spawn_connector(relay.addr, &relay.fingerprint, "alpha", token, b"A:").await;
    // Slot 2: a consumer holding a live bridge.
    let mut consumer1 =
        echo_when_ready(relay.addr, &relay.fingerprint, "alpha", b"A:", b"first").await;

    // Cap reached: the next connection is refused post-handshake.
    let refused = dial_consumer(relay.addr, &relay.fingerprint, "alpha").await;
    expect_post_handshake_close(refused, OVER_CAP_CODE, "relay at connection capacity").await;

    // Existing connections are untouched: the bridge still echoes and the
    // refused connection never reached the connector.
    expect_echo(&mut consumer1, b"", b"still-served").await;
    assert_eq!(connector.bridged(), 1, "only the admitted consumer bridged");
    assert_eq!(connector.streams_seen(), 1);
    assert!(
        connector.conn.close_reason().is_none(),
        "the tunnel must survive an over-cap refusal"
    );

    // Releasing the consumer's slot restores admission.
    consumer1.conn.close(0u32.into(), b"done");
    let mut consumer2 = echo_when_ready(
        relay.addr,
        &relay.fingerprint,
        "alpha",
        b"A:",
        b"after-release",
    )
    .await;
    expect_echo(&mut consumer2, b"", b"capacity-back").await;
    assert_eq!(connector.bridged(), 2);
}

/// Decision 6 / invariant 4, the stream-0 watchdog: after a completed
/// auth preamble, stream 0 is reserved. A connector that writes one more
/// byte on it is closed with specifically `PROTOCOL_VIOLATION` (0x04) and
/// the watchdog's reason text — richer relay dialogue requires an ALPN
/// bump, never in-band bytes.
#[tokio::test]
async fn byte_on_stream0_after_preamble_closes_with_protocol_violation() {
    let dir = tempfile::tempdir().unwrap();
    let relay = spawn_relay(dir.path(), DEFAULT_MAX_CONNS).await;
    let token = mint(&relay.tokens_path, "alpha");

    // A raw tunnel: the production dialer has already written the auth
    // preamble on stream 0, and the relay admitted the route.
    let (_ep, conn, mut send0, _recv0) =
        dial_tunnel_raw(relay.addr, &relay.fingerprint, "alpha", Some(token))
            .await
            .expect("tunnel establishes");
    await_route_live(relay.addr, &relay.fingerprint, "alpha").await;

    // One extra byte on the reserved stream trips the watchdog.
    send0
        .write_all(b"x")
        .await
        .expect("extra byte on stream 0 is writable");
    let err = timeout(WIRE_RECV_TIMEOUT, conn.closed())
        .await
        .expect("violation close resolves within deadline");
    assert_app_closed(
        &err,
        PROTOCOL_VIOLATION_CODE,
        "byte on stream 0 after preamble",
    );
    if let quinn::ConnectionError::ApplicationClosed(app) = &err {
        assert_eq!(
            app.reason.as_ref(),
            b"stream 0 is reserved after the auth preamble"
        );
    }
}

/// Deleting the store file revokes every route at the next handshake
/// (outline D9 liveness, the revocation half): live connections persist,
/// but a redial is refused and consumers find the route unknown at TLS.
#[tokio::test]
async fn deleting_store_revokes_at_next_handshake() {
    let dir = tempfile::tempdir().unwrap();
    let relay = spawn_relay(dir.path(), DEFAULT_MAX_CONNS).await;
    let token = mint(&relay.tokens_path, "alpha");
    let connector = spawn_connector(
        relay.addr,
        &relay.fingerprint,
        "alpha",
        token.clone(),
        b"A:",
    )
    .await;
    await_route_live(relay.addr, &relay.fingerprint, "alpha").await;

    std::fs::remove_file(&relay.tokens_path).unwrap();

    // A redial with the (formerly valid) token is refused...
    let (_ep, conn, _s0, _r0) =
        dial_tunnel_raw(relay.addr, &relay.fingerprint, "alpha", Some(token))
            .await
            .expect("TLS + ALPN succeed; the refusal is at the token layer");
    let err = timeout(SOCKET_CONNECT_DEADLINE, conn.closed())
        .await
        .expect("revoked token refused promptly");
    assert_app_closed(&err, AUTH_FAILED_CODE, "token after store deletion");

    // ...and the route is no longer enrolled for consumers (TLS refusal,
    // not ROUTE_OFFLINE — the enrolled set is re-read per handshake too).
    let refused = dial_consumer(relay.addr, &relay.fingerprint, "alpha").await;
    assert!(
        refused.is_err(),
        "an unenrolled route must be refused at the TLS layer"
    );
    assert_eq!(
        connector.bridged(),
        0,
        "no consumer ever reached the tunnel"
    );
}
