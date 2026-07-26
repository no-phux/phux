//! `phux-byc.5` — end-to-end server lifecycle scenarios.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

mod common;

use std::time::Duration;

use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet, ServerFeature};
use phux_protocol::ids::FileUploadId;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, TYPE_ATTACHED, TYPE_DETACHED, TYPE_HELLO_OK,
    TYPE_TERMINAL_OUTPUT, TYPE_TERMINAL_SNAPSHOT,
};
use portable_pty::CommandBuilder;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::time::timeout;

use crate::common::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server, spawn_server_with_seed_cmd, wait_for_socket,
};

/// Bounded join: every server task must terminate within this window
/// once the shutdown signal has been sent. Larger than `WIRE_RECV_TIMEOUT`
/// so a tardy actor doesn't get mis-attributed to the test.
const SERVER_JOIN_DEADLINE: Duration = Duration::from_secs(5);

/// Build the canonical HELLO payload for these tests. Mirrors the
/// `phux-client::attach::driver::handshake` shape: `TrueColor` + all
/// layers advertised.
fn hello_frame() -> FrameKind {
    FrameKind::Hello {
        client_name: "phux-end-to-end-test".to_owned(),
        protocol_major: 0,
        protocol_minor: 1,
        protocol_patch: 0,
        client_caps: ClientCapabilities::new()
            .with_color_support(ColorSupport::TrueColor)
            .with_layers(LayerSet::all()),
    }
}

/// Drive shutdown and assert clean teardown: server task joins ok,
/// socket file unlinked.
async fn shutdown_and_join(
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
    server_handle: tokio::task::JoinHandle<Result<(), phux_server::ServerError>>,
    socket_path: &std::path::Path,
) {
    shutdown_tx.send(()).ok();
    timeout(SERVER_JOIN_DEADLINE, server_handle)
        .await
        .expect("server did not shut down within deadline")
        .expect("server join")
        .expect("server run_async ok");
    assert!(
        !socket_path.exists(),
        "socket {} should be unlinked after shutdown",
        socket_path.display(),
    );
}

async fn recv_command_result(stream: &mut UnixStream, expected_request: u32) -> CommandResult {
    loop {
        let (_, frame) = recv_typed(stream).await;
        if let FrameKind::CommandResult { request_id, result } = frame {
            assert_eq!(request_id, expected_request);
            return result;
        }
    }
}

#[test]
fn handshake_hello_round_trip() {
    // SPEC §6.1: HELLO must be acknowledged with HELLO_OK before ATTACH.
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("default"));

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        send_frame(&mut stream, &hello_frame()).await;
        let (type_byte, hello_ok) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_HELLO_OK,
            "after HELLO the first reply must be HELLO_OK",
        );
        // The body must echo the server's selected version and advertise
        // the tiers it mounts (L1+L2+L3) per SPEC §6.1.
        let server_id = match hello_ok {
            FrameKind::HelloOk {
                protocol_major,
                protocol_minor,
                server_caps,
                server_id,
                ..
            } => {
                assert_eq!(
                    (protocol_major, protocol_minor),
                    (
                        phux_protocol::PROTOCOL_VERSION.major,
                        phux_protocol::PROTOCOL_VERSION.minor,
                    ),
                    "HELLO_OK must echo the server's selected version",
                );
                assert_eq!(
                    server_caps.layers,
                    LayerSet::all(),
                    "reference server advertises the full tier set",
                );
                assert!(
                    server_caps
                        .features
                        .contains(ServerFeature::AcknowledgedInput),
                    "reference server advertises acknowledged input",
                );
                assert!(
                    server_caps.features.contains(ServerFeature::FileUpload),
                    "reference server advertises sandboxed file upload",
                );
                assert_eq!(server_id.len(), 16);
                server_id
            }
            other => panic!("expected HELLO_OK, got {other:?}"),
        };

        let mut second_stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut second_stream, &hello_frame()).await;
        let (_, second_hello) = recv_typed(&mut second_stream).await;
        match second_hello {
            FrameKind::HelloOk {
                server_id: second_id,
                ..
            } => assert_eq!(second_id, server_id, "one state has one incarnation id"),
            other => panic!("expected second HELLO_OK, got {other:?}"),
        }
        drop(second_stream);
        send_frame(&mut stream, &attach_by_name("default")).await;

        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "after HELLO_OK + ATTACH the next reply must be ATTACHED",
        );
        match attached {
            FrameKind::Attached {
                snapshot,
                initial_client_id,
            } => {
                assert_eq!(snapshot.sessions.len(), 1);
                assert_eq!(snapshot.sessions[0].name, "default");
                assert!(initial_client_id.get() >= 1);
            }
            other => panic!("expected Attached, got {other:?}"),
        }

        let (type_byte, _snap) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_TERMINAL_SNAPSHOT);

        drop(stream);
        shutdown_and_join(shutdown_tx, server_handle, &socket_path).await;
    });
}

#[test]
fn put_file_round_trip_publishes_only_the_verified_file() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let upload_dir = tmp.path().join("uploads");
        // This integration-test binary has no other PutFile test, so no task
        // can read the process-global override while it is changing.
        unsafe { std::env::set_var("PHUX_UPLOAD_DIR", &upload_dir) };
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("default"));

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &hello_frame()).await;
        let (_, hello) = recv_typed(&mut stream).await;
        assert!(matches!(hello, FrameKind::HelloOk { .. }));
        send_frame(&mut stream, &attach_by_name("default")).await;
        let (_, attached) = recv_typed(&mut stream).await;
        let terminal_id = match attached {
            FrameKind::Attached { snapshot, .. } => snapshot.panes[0].id.clone(),
            other => panic!("expected ATTACHED, got {other:?}"),
        };
        let _ = recv_typed(&mut stream).await; // authoritative terminal snapshot

        let bytes = b"wire-native image bytes";
        let split = 7;
        let upload_id = FileUploadId::new([0x5a; 16]).unwrap();
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 41,
                command: Command::PutFile {
                    upload_id,
                    terminal_id: terminal_id.clone(),
                    extension: "PNG".to_owned(),
                    offset: 0,
                    data: bytes[..split].to_vec(),
                    final_chunk: false,
                    sha256: None,
                },
            },
        )
        .await;
        assert_eq!(
            recv_command_result(&mut stream, 41).await,
            CommandResult::OkWith(CommandValue::FileUpload(
                phux_protocol::wire::frame::FileUploadAck {
                    next_offset: u64::try_from(split).unwrap(),
                    path: None,
                }
            ))
        );
        assert!(
            std::fs::read_dir(&upload_dir).unwrap().all(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with('.')),
            "a non-final chunk must expose no public file"
        );

        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 42,
                command: Command::PutFile {
                    upload_id,
                    terminal_id,
                    extension: "png".to_owned(),
                    offset: u64::try_from(split).unwrap(),
                    data: bytes[split..].to_vec(),
                    final_chunk: true,
                    sha256: Some(Sha256::digest(bytes).into()),
                },
            },
        )
        .await;
        let path = match recv_command_result(&mut stream, 42).await {
            CommandResult::OkWith(CommandValue::FileUpload(ack)) => {
                assert_eq!(ack.next_offset, u64::try_from(bytes.len()).unwrap());
                ack.path.expect("verified final path")
            }
            other => panic!("expected file-upload acknowledgement, got {other:?}"),
        };
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(
            std::path::Path::new(&path).starts_with(&upload_dir),
            "server must choose a path inside its upload sandbox"
        );

        unsafe { std::env::remove_var("PHUX_UPLOAD_DIR") };
        drop(stream);
        shutdown_and_join(shutdown_tx, server_handle, &socket_path).await;
    });
}

#[test]
fn attach_returns_snapshot() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("default"));

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("default")).await;

        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED);
        match attached {
            FrameKind::Attached {
                snapshot,
                initial_client_id,
            } => {
                assert_eq!(snapshot.sessions.len(), 1);
                assert_eq!(snapshot.sessions[0].name, "default");
                assert_eq!(snapshot.windows.len(), 1);
                assert_eq!(snapshot.panes.len(), 1);
                assert!(initial_client_id.get() >= 1);
            }
            other => panic!("expected Attached, got {other:?}"),
        }

        let (type_byte, snap) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_TERMINAL_SNAPSHOT);
        match snap {
            FrameKind::TerminalSnapshot {
                cols,
                rows,
                vt_replay_bytes,
                ..
            } => {
                assert_eq!(cols, 80);
                assert_eq!(rows, 24);
                // The reset preamble is the load-bearing invariant for
                // any client-side replay (see `byc_6_1_attach_snapshot`).
                assert!(
                    vt_replay_bytes.starts_with(b"\x1b[!p\x1b[2J\x1b[H"),
                    "snapshot must carry the reset preamble",
                );
            }
            other => panic!("expected TerminalSnapshot, got {other:?}"),
        }

        drop(stream);
        shutdown_and_join(shutdown_tx, server_handle, &socket_path).await;
    });
}

#[test]
fn input_routes_to_pane() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        // `cat` is the deterministic echo fixture: cooked-mode PTY + a
        // line-buffered reader. Mirrors `pty_pump` / `input_dispatch`.
        let cmd = CommandBuilder::new("/bin/cat");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("default")).await;

        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED);
        let wire_pane_id = match attached {
            FrameKind::Attached { snapshot, .. } => {
                assert_eq!(snapshot.panes.len(), 1);
                snapshot.panes[0].id.clone()
            }
            other => panic!("expected Attached, got {other:?}"),
        };

        let (type_byte, _snap) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_TERMINAL_SNAPSHOT);

        send_frame(
            &mut stream,
            &FrameKind::InputKey {
                terminal_id: wire_pane_id.clone(),
                event: KeyEvent {
                    action: KeyAction::Press,
                    key: PhysicalKey::A,
                    mods: ModSet::empty(),
                    consumed_mods: ModSet::empty(),
                    composing: false,
                    text: Some("a".to_owned()),
                    unshifted_codepoint: Some('a' as u32),
                },
            },
        )
        .await;
        send_frame(
            &mut stream,
            &FrameKind::InputKey {
                terminal_id: wire_pane_id,
                event: KeyEvent {
                    action: KeyAction::Press,
                    key: PhysicalKey::Enter,
                    mods: ModSet::empty(),
                    consumed_mods: ModSet::empty(),
                    composing: false,
                    text: None,
                    unshifted_codepoint: None,
                },
            },
        )
        .await;

        // Accumulate TERMINAL_OUTPUT chunks until `b'a'` shows up or we
        // hit the wire deadline. The PTY driver echoes the press itself
        // (cooked mode) and `cat` re-echoes the line on Enter.
        let mut acc: Vec<u8> = Vec::new();
        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        while tokio::time::Instant::now() < deadline && !acc.contains(&b'a') {
            let remaining = deadline - tokio::time::Instant::now();
            let Ok((tb, frame)) = timeout(remaining, recv_typed(&mut stream)).await else {
                break;
            };
            if tb == TYPE_TERMINAL_OUTPUT
                && let FrameKind::TerminalOutput { bytes, .. } = frame
            {
                acc.extend_from_slice(&bytes);
            }
        }
        assert!(
            acc.contains(&b'a'),
            "INPUT_KEY('a') must round-trip through the PTY and appear in TERMINAL_OUTPUT \
             (got {} bytes: {:?})",
            acc.len(),
            acc,
        );

        drop(stream);
        shutdown_and_join(shutdown_tx, server_handle, &socket_path).await;
    });
}

#[test]
fn detach_clean_shutdown() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("default"));

        let mut client_a = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut client_a, &attach_by_name("default")).await;

        let (type_byte, _attached_a) = recv_typed(&mut client_a).await;
        assert_eq!(type_byte, TYPE_ATTACHED);
        let (type_byte, _snap_a) = recv_typed(&mut client_a).await;
        assert_eq!(type_byte, TYPE_TERMINAL_SNAPSHOT);

        send_frame(&mut client_a, &FrameKind::Detach).await;
        let (type_byte, detached) = recv_typed(&mut client_a).await;
        assert_eq!(type_byte, TYPE_DETACHED);
        assert!(
            matches!(detached, FrameKind::Detached),
            "expected Detached, got {detached:?}",
        );
        drop(client_a);

        // Server still accepting: a fresh client must complete an
        // ATTACH against the same runtime instance.
        let mut client_b = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut client_b, &attach_by_name("default")).await;
        let (type_byte, _attached_b) = recv_typed(&mut client_b).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "post-detach reattach must succeed",
        );
        let (type_byte, _snap_b) = recv_typed(&mut client_b).await;
        assert_eq!(type_byte, TYPE_TERMINAL_SNAPSHOT);

        drop(client_b);
        shutdown_and_join(shutdown_tx, server_handle, &socket_path).await;
    });
}

/// Shutdown must destroy live accept-loop client futures before joining the
/// dedicated input lane. Each client future owns an `InputLaneHandle`; joining
/// first would wait forever for the channel to close.
#[test]
fn shutdown_with_live_real_clients_releases_input_lane_handles() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("default"));

        let mut client_a = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        let mut client_b = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        for client in [&mut client_a, &mut client_b] {
            send_frame(client, &attach_by_name("default")).await;
            assert_eq!(recv_typed(client).await.0, TYPE_ATTACHED);
            assert_eq!(recv_typed(client).await.0, TYPE_TERMINAL_SNAPSHOT);
        }

        // Deliberately keep both real sockets alive across server shutdown.
        // Their accept-loop tasks still hold lane handles at cancellation.
        shutdown_and_join(shutdown_tx, server_handle, &socket_path).await;
        drop((client_a, client_b));
    });
}

#[test]
fn server_survives_mid_frame_disconnect() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("default"));

        // Connection 1: write a 4-byte length prefix declaring a body
        // of 64 bytes, then close without sending the body. The server
        // must observe EOF mid-frame and tear the per-client task down
        // without panicking.
        let mut partial = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        let bogus_header: [u8; 4] = 64u32.to_be_bytes();
        partial.write_all(&bogus_header).await.unwrap();
        partial.flush().await.unwrap();
        // Half-close the write side, then drop. `shutdown` is best-effort
        // on a UnixStream; ignore failures (some kernels return ENOTCONN
        // on already-closing sockets).
        let _ = AsyncWriteExt::shutdown(&mut partial).await;
        drop(partial);

        // Connection 2: a real client. If the server task crashed or
        // the accept loop is wedged this will never complete the
        // attach handshake.
        let mut healthy: UnixStream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut healthy, &attach_by_name("default")).await;
        let (type_byte, _attached) = recv_typed(&mut healthy).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "server must keep accepting after a mid-frame disconnect",
        );
        let (type_byte, _snap) = recv_typed(&mut healthy).await;
        assert_eq!(type_byte, TYPE_TERMINAL_SNAPSHOT);

        drop(healthy);
        shutdown_and_join(shutdown_tx, server_handle, &socket_path).await;
    });
}
