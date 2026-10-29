//! Satellites' mail and claims on this hub (`docs/agents.md#mail-and-path-claims`).
//!
//! A satellite dials the hub's QUIC listener under [`COORD_ALPN`] with its
//! satellite credential, the one it joined with. Each stream it opens is one
//! call: a work-store request line, answered with one reply line by a
//! [`Gateway`] for the satellite the credential names, against the server's
//! work store (`ServerConfig::work_store`). The gateway keeps the satellite to its own addresses, and nothing
//! here reaches a terminal. A wait whose stream the satellite stops is
//! abandoned, taking nothing; a revoked credential closes the connection
//! within [`super::REVOCATION_POLL`].
//!
//! [`COORD_ALPN`]: phux_protocol::policy::COORD_ALPN

#![allow(
    clippy::redundant_pub_crate,
    reason = "pub(crate) module; pub would be crate-visible and unreachable_pub would reject it"
)]

use std::os::fd::AsFd as _;
use std::sync::Arc;

use phux_work::federation::Gateway;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

/// Calls one satellite connection may have in flight; more wait their turn.
/// A harness holds one open wait, and each hook call is short.
const MAX_CALLS: usize = 8;

/// Longest request line, the work store's own limit.
const MAX_LINE: usize = 1 << 20;

/// QUIC application close code for a refused satellite, the listener's
/// `AUTH_FAILED`.
const AUTH_FAILED_CODE: u32 = 0x01;

/// Admit one connection that negotiated the coordination ALPN and serve its
/// calls, or close it with `AUTH_FAILED` before any stream: the certificate
/// must map to an active satellite credential with a name mail can carry.
pub(crate) fn admit(
    connection: quinn::Connection,
    registry: Option<&Arc<crate::workload::ReloadingWorkloadRegistry>>,
    store: Option<phux_work::Client>,
) {
    let remote = connection.remote_address();
    let refuse = |why: &str| {
        debug!(%remote, "satellite mail refused: {why}");
        connection.close(AUTH_FAILED_CODE.into(), b"unauthorized");
    };
    let Some(registry) = registry else {
        return refuse("this listener verifies no workload certificates");
    };
    let Some(store) = store else {
        return refuse("this server keeps no work store for satellites");
    };
    let Some((credential, name)) =
        super::peer_leaf(&connection).and_then(|leaf| registry.lookup_satellite_certificate(&leaf))
    else {
        return refuse("no active satellite credential");
    };
    let gateway = match Gateway::new(store, &name) {
        Ok(gateway) => gateway,
        Err(err) => {
            warn!(satellite = %name, "satellite mail refused: {err}");
            return refuse("its name cannot carry mail");
        }
    };
    info!(satellite = %name, %remote, "satellite connected for mail and claims");
    tokio::task::spawn_local(super::watch_revocation(
        connection.clone(),
        Arc::clone(registry),
        credential,
        name,
    ));
    tokio::task::spawn_local(serve(connection, gateway));
}

#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: connection tasks run on the server's LocalSet"
)]
async fn serve(connection: quinn::Connection, gateway: Gateway) {
    let calls = Arc::new(Semaphore::new(MAX_CALLS));
    while let Ok((send, recv)) = connection.accept_bi().await {
        let Ok(turn) = Arc::clone(&calls).acquire_owned().await else {
            return;
        };
        let gateway = gateway.clone();
        tokio::task::spawn_local(async move {
            call(send, recv, gateway).await;
            drop(turn);
        });
    }
}

/// Answer one call. The gateway runs on a blocking thread holding a pipe's
/// read end as its lifeline; the satellite stopping the stream, or the
/// connection ending, closes the write end, so a parked wait hangs up on the
/// keeper before it takes anything.
async fn call(mut send: quinn::SendStream, mut recv: quinn::RecvStream, gateway: Gateway) {
    let line = match recv.read_to_end(MAX_LINE).await {
        Ok(line) => line,
        Err(err) => return debug!("satellite mail call unread: {err}"),
    };
    let Ok(line) = String::from_utf8(line) else {
        return debug!("satellite mail call is not UTF-8");
    };
    let (lifeline, alive) = match std::io::pipe() {
        Ok(pipe) => pipe,
        Err(err) => return warn!("satellite mail call: no lifeline pipe: {err}"),
    };
    let mut answer =
        tokio::task::spawn_blocking(move || gateway.answer(&line, Some(lifeline.as_fd())));
    let answered = tokio::select! {
        reply = &mut answer => Some(reply),
        _ = send.stopped() => None,
    };
    drop(alive);
    let Some(Ok(reply)) = answered else {
        // Stopped: the gateway now returns at once, having taken nothing.
        drop(answer.await);
        return;
    };
    if let Err(err) = send.write_all(reply.as_bytes()).await {
        return debug!("satellite mail reply unsent: {err}");
    }
    drop(send.finish());
}
