//! End-to-end tests for `phux stdio-bridge` (phux-v45.9, ADR-0007).
//!
//! The bridge is the remote end of the SSH-stdio transport: it must be a
//! byte-transparent splice between its stdin/stdout and the server's
//! Unix socket. These tests spawn the REAL binary against a local UDS
//! listener — no ssh required (the hub-side spawn/argv path is covered
//! in `phux-server::hub::link` with a stub program). What is asserted
//! here is the honest transport property: arbitrary bytes (including
//! length-prefixed wire framing with embedded NUL/newline bytes) cross
//! both directions unmodified, stdout stays protocol-pure, and the
//! process exits cleanly when either side closes.

#![allow(clippy::expect_used, reason = "tests")]

use std::process::Stdio;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

const fn phux_bin() -> &'static str {
    env!("CARGO_BIN_EXE_phux")
}

/// A frame-shaped byte blob: u32 BE length prefix + type byte + payload
/// with bytes a line- or text-oriented bridge would mangle (NUL, CR, LF,
/// 0xFF). The bridge must not care that this "is" a frame — it is opaque
/// bytes — but using the wire shape keeps the test honest about what
/// will actually flow in phux-v45.4.
fn frame_like(type_byte: u8, payload: &[u8]) -> Vec<u8> {
    let length = u32::try_from(payload.len() + 1).expect("test frame fits u32");
    let mut bytes = length.to_be_bytes().to_vec();
    bytes.push(type_byte);
    bytes.extend_from_slice(payload);
    bytes
}

#[tokio::test]
async fn bridge_splices_frame_shaped_bytes_both_ways_unmodified() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind uds");

    let mut child = tokio::process::Command::new(phux_bin())
        .args(["stdio-bridge", "--socket"])
        .arg(&socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn bridge");
    let mut child_stdin = child.stdin.take().expect("stdin");
    let mut child_stdout = child.stdout.take().expect("stdout");

    let (mut server_side, _) = listener.accept().await.expect("bridge connects");

    // Client -> server: a frame-shaped blob written to the bridge's stdin
    // must arrive on the UDS byte-for-byte.
    let inbound = frame_like(0x01, b"hello\x00\r\n\xff over ssh");
    child_stdin.write_all(&inbound).await.expect("write stdin");
    child_stdin.flush().await.expect("flush stdin");
    let mut got = vec![0u8; inbound.len()];
    server_side.read_exact(&mut got).await.expect("read uds");
    assert_eq!(got, inbound);

    // Server -> client: bytes written to the UDS must come out of the
    // bridge's stdout byte-for-byte, with nothing injected before them.
    let outbound = frame_like(0xB3, b"\x00\x00binary\npayload\xfe");
    server_side.write_all(&outbound).await.expect("write uds");
    server_side.flush().await.expect("flush uds");
    let mut got = vec![0u8; outbound.len()];
    child_stdout
        .read_exact(&mut got)
        .await
        .expect("read stdout");
    assert_eq!(got, outbound, "stdout must be protocol-pure");

    // Server closes: the bridge must notice and exit cleanly.
    drop(server_side);
    drop(listener);
    let status = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
        .await
        .expect("bridge exits after server close")
        .expect("wait");
    assert!(status.success(), "clean close is exit 0, got {status}");
}

#[tokio::test]
async fn bridge_exits_cleanly_when_the_remote_peer_hangs_up_stdin() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind uds");

    let mut child = tokio::process::Command::new(phux_bin())
        .args(["stdio-bridge", "--socket"])
        .arg(&socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn bridge");
    let child_stdin = child.stdin.take().expect("stdin");

    let (_server_side, _) = listener.accept().await.expect("bridge connects");

    // The dialing side going away is stdin EOF here.
    drop(child_stdin);
    let status = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
        .await
        .expect("bridge exits after stdin EOF")
        .expect("wait");
    assert!(status.success(), "peer hangup is exit 0, got {status}");
}

#[tokio::test]
async fn bridge_fails_fast_with_a_diagnostic_when_the_socket_is_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("no-server-here.sock");

    let output = tokio::process::Command::new(phux_bin())
        .args(["stdio-bridge", "--socket"])
        .arg(&socket)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("run bridge");

    assert!(!output.status.success(), "missing socket must be nonzero");
    assert!(
        output.stdout.is_empty(),
        "stdout stays protocol-pure even on failure: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot connect") && stderr.contains("no-server-here.sock"),
        "diagnostic names the socket: {stderr}"
    );
}
