//! phux-2o3r — `ERROR { code: FRAME_TOO_LARGE }` on a SPEC §5 framing
//! violation.
//!
//! SPEC §5: a peer receiving a frame whose `length` lies outside
//! `1..=16_777_216` MUST send `ERROR { code: FRAME_TOO_LARGE }` and close
//! the transport. Before phux-2o3r every reader closed silently — the
//! close half of the requirement without the ERROR half.
//!
//! Wire-level integration tests, driving `handle_client` (the production
//! per-client read loop) through a real UDS connection rather than
//! unit-testing the reader in isolation. Each test writes a raw 4-byte
//! frame header that violates §5 and asserts, in order:
//!
//! 1. an `ERROR` frame arrives with `code == ErrorCode::FrameTooLarge`
//!    (wire value 4) and no `request_id` (the violation is not a COMMAND);
//! 2. a `DETACHED { reason: PROTOCOL_ERROR }` follows (SPEC §14 fatal-error
//!    close order, matching every other `close_for_protocol_error` path);
//! 3. the server then closes the connection (EOF).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_protocol::wire::frame::{ErrorCode, FrameKind, MAX_FRAME_LEN, TYPE_ERROR};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, expect_protocol_error_close, recv_typed, run_local, spawn_server,
    wait_for_socket,
};

/// How long the server gets to finish the close after its final frame.
const EOF_DEADLINE: Duration = Duration::from_secs(5);

/// Drive one raw §5-violating frame header at a freshly spawned server and
/// assert the ERROR → DETACHED → EOF close sequence.
fn assert_framing_violation_close(header: [u8; 4]) {
    run_local(async move {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // ---- The violation: a bare frame header with an out-of-range length.
        stream.write_all(&header).await.unwrap();
        stream.flush().await.unwrap();

        // ---- 1. ERROR { code: FRAME_TOO_LARGE } ----
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
                assert!(
                    request_id.is_none(),
                    "a framing violation is not COMMAND-correlated (got {request_id:?})",
                );
                assert_eq!(
                    code,
                    ErrorCode::FrameTooLarge,
                    "expected ErrorCode::FrameTooLarge per SPEC §5",
                );
                assert_eq!(code.as_wire(), 4, "FRAME_TOO_LARGE is wire value 4");
                let declared = u32::from_be_bytes(header);
                assert!(
                    message.contains(&declared.to_string()),
                    "error message must name the offending length {declared}, got: {message:?}",
                );
            }
            other => panic!("expected FrameKind::Error, got {other:?}"),
        }

        // ---- 2+3. DETACHED { reason: PROTOCOL_ERROR }, then the close. ----
        // The §14 fatal-close tail is identical for every protocol violation,
        // so it is asserted by the shared testkit helper; only the ERROR above
        // is specific to §5 framing.
        expect_protocol_error_close(&mut stream, EOF_DEADLINE).await;

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}

/// `length` one past the 16 MiB cap: the canonical `FRAME_TOO_LARGE` case.
#[test]
fn oversized_frame_length_gets_error_then_close() {
    assert_framing_violation_close((MAX_FRAME_LEN + 1).to_be_bytes());
}

/// `length == 0` leaves no room for the type byte — the other side of the
/// `1..=MAX_FRAME_LEN` bound, and the same §5 violation.
#[test]
fn zero_frame_length_gets_error_then_close() {
    assert_framing_violation_close(0u32.to_be_bytes());
}

/// The all-ones header an endianness bug or garbage stream produces.
#[test]
fn max_u32_frame_length_gets_error_then_close() {
    assert_framing_violation_close(u32::MAX.to_be_bytes());
}
