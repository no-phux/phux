//! `phux-byc.6.6` — `attach_unknown_session_returns_error`.
//!
//! Wire-level integration test. Connects a synthetic client to a
//! freshly-spawned `ServerRuntime` with no pre-seeded session, sends
//! `ATTACH { ByName("does-not-exist") }`, and verifies:
//!
//! 1. A single `ERROR` frame arrives with
//!    `code == ErrorCode::SessionNotFound` (numeric value 102 per
//!    SPEC §14) and `request_id == None` (ATTACH is not a COMMAND
//!    correlated by `request_id`).
//! 2. The error message mentions the requested session name, so
//!    operators get an actionable diagnostic.
//! 3. No `ATTACHED` frame and no `TERMINAL_SNAPSHOT` frame are sent
//!    (the ATTACH must fail atomically — never partially-attach).
//! 4. The connection stays open: per SPEC §14 only *fatal* errors
//!    are followed by `DETACHED { reason: PROTOCOL_ERROR }` and
//!    close. `SessionNotFound` is a recoverable, per-request error;
//!    the client can retry with a different target on the same
//!    connection. We prove the channel is still live by issuing a
//!    `PING` and verifying a `PONG` round-trip.
//!
//! This test supersedes the byc.8 precursor
//! `attach_lifecycle::attach_unknown_session_returns_error`, which
//! covers (1) and (2) but does NOT check (3) "no spurious frames"
//! or (4) "connection stays open". The byc.6.6 ticket explicitly
//! calls out that the connection MUST survive an ATTACH error per
//! SPEC §14, so we make that assertion mechanical here.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

mod common;

use std::time::Duration;

use phux_protocol::wire::frame::{ErrorCode, FrameKind, TYPE_ERROR};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;

use crate::common::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, encode_frame, recv_typed,
    run_local, send_frame, spawn_server, wait_for_socket,
};

/// Distinctive PING nonce for the post-error liveness probe. Any value
/// is fine; we pick a recognisable bit pattern so a failing assertion
/// reads as "PONG nonce mismatch" rather than "PONG body decode error".
const PING_NONCE: u64 = 0xDEAD_BEEF_CAFE_F00D;

#[test]
fn byc_6_6_attach_unknown_session_returns_error_keeps_connection_open() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        // No pre-seeded session — every ATTACH must fail.
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // ---- ATTACH against an unknown session ----
        send_frame(&mut stream, &attach_by_name("does-not-exist")).await;

        // ---- Expect exactly one ERROR frame ----
        let (type_byte, frame) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_ERROR,
            "first server-to-client frame must be ERROR (got type 0x{type_byte:02x})",
        );
        match frame {
            FrameKind::Error {
                request_id,
                code,
                message,
            } => {
                // ATTACH is not a COMMAND, so the error must not carry a
                // request_id (SPEC §14: "present if the error is
                // associated with a COMMAND").
                assert!(
                    request_id.is_none(),
                    "ATTACH errors must not carry a request_id (got {request_id:?})",
                );
                assert_eq!(
                    code,
                    ErrorCode::SessionNotFound,
                    "expected ErrorCode::SessionNotFound (SPEC §14 code 102)",
                );
                // Sanity-check the numeric mapping pinned by SPEC §14.
                assert_eq!(code as u16, 102, "SessionNotFound is 102 per SPEC §14");
                assert!(
                    message.contains("does-not-exist"),
                    "error message must mention the requested session, got: {message:?}",
                );
            }
            other => panic!("expected FrameKind::Error, got {other:?}"),
        }

        // ---- Negative: NO spurious follow-up frames ----
        // The ATTACH failed atomically; the server must NOT have queued
        // an ATTACHED or TERMINAL_SNAPSHOT. Read for a short window with a
        // tight deadline and assert no bytes arrive. The server is
        // single-threaded on a current_thread runtime; if it were going
        // to send anything more, it would be in the writer's mailbox
        // by now.
        let mut sink = [0u8; 16];
        match timeout(Duration::from_millis(100), stream.read(&mut sink)).await {
            Err(_) => {
                // Timeout — no bytes. Exactly what we want.
            }
            Ok(Ok(0)) => panic!(
                "server closed the connection after a recoverable ATTACH error; \
                 SPEC §14 only requires close for fatal errors",
            ),
            Ok(Ok(n)) => panic!(
                "server sent {n} unexpected byte(s) after the ERROR frame: {:02x?}",
                &sink[..n],
            ),
            Ok(Err(e)) => panic!("read error while checking for spurious frames: {e}"),
        }

        // ---- Liveness: PING/PONG proves the channel is still usable ----
        // SPEC §14: non-fatal errors do NOT close the transport. Send a
        // PING with a distinctive nonce and confirm the typed PONG comes back.
        let ping = encode_frame(&FrameKind::Ping { nonce: PING_NONCE });
        stream.write_all(&ping).await.unwrap();
        stream.flush().await.unwrap();
        let (_type_byte, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(&mut stream))
            .await
            .expect("timed out waiting for PONG frame");
        assert_eq!(
            frame,
            FrameKind::Pong { nonce: PING_NONCE },
            "PONG must echo the PING nonce — proves channel liveness post-error",
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}
