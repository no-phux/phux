//! `phux-byc.5` — end-to-end server lifecycle scenarios.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfile, BootstrapProfileKind,
    BootstrapProfileSet, ClientCapabilities, ColorSupport, EngineCodecSet, EngineFeatureSet,
    LayerSet, ServerFeature,
};
use phux_protocol::ids::{BootstrapId, FileUploadId, StreamId, TerminalId};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, DetachReason, ErrorCode, FrameKind, TYPE_ATTACHED,
    TYPE_BOOTSTRAP_BEGIN, TYPE_ERROR, TYPE_HELLO_OK,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, expect_protocol_error_close,
    recv_typed, recv_until_detached, run_local, send_frame, spawn_server, wait_for_raw_socket,
};

/// Bounded join: every server task must terminate within this window
/// once the shutdown signal has been sent. Aliases the shared constant so
/// this file cannot drift back to a hand-picked number (phux-br1f) — see
/// `phux_server_testkit::SERVER_JOIN_DEADLINE` for why the value is not load-bearing.
const SERVER_JOIN_DEADLINE: Duration = phux_server_testkit::SERVER_JOIN_DEADLINE;

/// Build the canonical HELLO payload for these tests. Mirrors the
/// `phux-client::attach::driver::handshake` shape: `TrueColor` + all
/// layers advertised.
fn hello_frame() -> FrameKind {
    hello_for_version(
        PROTOCOL_VERSION.major,
        PROTOCOL_VERSION.minor,
        PROTOCOL_VERSION.patch,
    )
}

fn hello_for_version(major: u16, minor: u16, patch: u16) -> FrameKind {
    hello_with_caps(
        major,
        minor,
        patch,
        ClientCapabilities::new()
            .with_color_support(ColorSupport::TrueColor)
            .with_layers(LayerSet::all()),
    )
}

fn hello_with_caps(
    major: u16,
    minor: u16,
    patch: u16,
    client_caps: ClientCapabilities,
) -> FrameKind {
    FrameKind::Hello {
        client_name: "phux-end-to-end-test".to_owned(),
        protocol_major: major,
        protocol_minor: minor,
        protocol_patch: patch,
        client_caps,
    }
}

async fn negotiate(stream: &mut UnixStream) {
    send_frame(stream, &hello_frame()).await;
    let (type_byte, frame) = recv_typed(stream).await;
    assert_eq!(type_byte, TYPE_HELLO_OK);
    assert!(matches!(frame, FrameKind::HelloOk { .. }));
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

        let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

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
                selected_profile,
                bootstrap_limits,
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
                assert_eq!(selected_profile, BootstrapProfile::SynthesizedVtRaw);
                assert_eq!(bootstrap_limits, BootstrapLimits::default());
                assert_eq!(server_id.len(), 16);
                server_id
            }
            other => panic!("expected HELLO_OK, got {other:?}"),
        };

        let mut second_stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
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
                attach_id,
                snapshot,
                initial_client_id,
            } => {
                assert_eq!(attach_id, 1);
                assert_eq!(snapshot.sessions.len(), 1);
                assert_eq!(snapshot.sessions[0].name, "default");
                assert!(initial_client_id.get() >= 1);
            }
            other => panic!("expected Attached, got {other:?}"),
        }

        let (type_byte, _snap) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_BOOTSTRAP_BEGIN);

        drop(stream);
        shutdown_and_join(shutdown_tx, server_handle, &socket_path).await;
    });
}

#[test]
fn zero_attach_id_is_rejected_before_attached_state() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("default"));
        let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        negotiate(&mut stream).await;

        send_frame(
            &mut stream,
            &FrameKind::Attach {
                attach_id: 0,
                target: phux_protocol::wire::frame::AttachTarget::ByName("default".to_owned()),
                viewport: phux_protocol::wire::frame::ViewportInfo::new(80, 24),
                request_scrollback: false,
                scrollback_limit_lines: 0,
            },
        )
        .await;
        let (_, response) = recv_typed(&mut stream).await;
        assert!(matches!(
            response,
            FrameKind::Error {
                code: ErrorCode::MalformedMessage,
                message,
                ..
            } if message.contains("attach_id must be nonzero")
        ));
        expect_protocol_error_close(&mut stream, WIRE_RECV_TIMEOUT).await;
        shutdown_and_join(shutdown_tx, server_handle, &socket_path).await;
    });
}

#[test]
fn client_sent_hello_ok_is_fatal_after_negotiation() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("default"));
        let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        negotiate(&mut stream).await;

        send_frame(
            &mut stream,
            &FrameKind::HelloOk {
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                server_caps: phux_protocol::caps::ServerCapabilities::new(),
                server_id: Vec::new(),
                selected_profile: BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits: BootstrapLimits::default(),
            },
        )
        .await;
        let (_, response) = recv_typed(&mut stream).await;
        assert!(matches!(
            response,
            FrameKind::Error {
                code: ErrorCode::InvalidCommand,
                ..
            }
        ));
        expect_protocol_error_close(&mut stream, WIRE_RECV_TIMEOUT).await;
        shutdown_and_join(shutdown_tx, server_handle, &socket_path).await;
    });
}

#[test]
fn handshake_selects_explicit_synth_fallback_and_intersects_bounds() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("default"));
        let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        let offered_limits = BootstrapLimits::new(64 * 1024, 128 * 1024).unwrap();
        let bootstrap = BootstrapCapabilities::new()
            .with_profiles(BootstrapProfileSet::with(&[
                BootstrapProfileKind::SynthesizedVtRaw,
            ]))
            .with_native_codecs(EngineCodecSet::new())
            .with_native_features(EngineFeatureSet::new())
            .with_limits(offered_limits);
        send_frame(
            &mut stream,
            &hello_with_caps(
                PROTOCOL_VERSION.major,
                PROTOCOL_VERSION.minor,
                PROTOCOL_VERSION.patch,
                ClientCapabilities::new().with_bootstrap(bootstrap),
            ),
        )
        .await;
        let (_, response) = recv_typed(&mut stream).await;
        assert!(matches!(
            response,
            FrameKind::HelloOk {
                selected_profile: BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits,
                ..
            } if bootstrap_limits == offered_limits
        ));
        drop(stream);
        shutdown_and_join(shutdown_tx, server_handle, &socket_path).await;
    });
}

#[test]
fn native_required_without_common_codec_fails_before_attach() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("default"));
        let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        let native_only = BootstrapCapabilities::new()
            .with_profiles(BootstrapProfileSet::with(&[
                BootstrapProfileKind::NativeState,
            ]))
            .with_native_codecs(EngineCodecSet::new());
        send_frame(
            &mut stream,
            &hello_with_caps(
                PROTOCOL_VERSION.major,
                PROTOCOL_VERSION.minor,
                PROTOCOL_VERSION.patch,
                ClientCapabilities::new().with_bootstrap(native_only),
            ),
        )
        .await;
        let (_, response) = recv_typed(&mut stream).await;
        match response {
            FrameKind::Error { code, message, .. } => {
                assert_eq!(code, ErrorCode::CodecUnavailable);
                assert!(message.contains("exact common codec"));
                assert!(message.contains("SynthesizedVtRaw"));
            }
            other => panic!("expected CODEC_UNAVAILABLE, got {other:?}"),
        }
        expect_protocol_error_close(&mut stream, WIRE_RECV_TIMEOUT).await;
        shutdown_and_join(shutdown_tx, server_handle, &socket_path).await;
    });
}

#[test]
fn negotiated_chunk_limit_rejects_oversized_payload_at_runtime_decode() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("default"));
        let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        let offered_limits = BootstrapLimits::new(1024, 2048).unwrap();
        let bootstrap = BootstrapCapabilities::new()
            .with_profiles(BootstrapProfileSet::with(&[
                BootstrapProfileKind::SynthesizedVtRaw,
            ]))
            .with_native_codecs(EngineCodecSet::new())
            .with_native_features(EngineFeatureSet::new())
            .with_limits(offered_limits);
        send_frame(
            &mut stream,
            &hello_with_caps(
                PROTOCOL_VERSION.major,
                PROTOCOL_VERSION.minor,
                PROTOCOL_VERSION.patch,
                ClientCapabilities::new().with_bootstrap(bootstrap),
            ),
        )
        .await;
        assert!(matches!(
            recv_typed(&mut stream).await.1,
            FrameKind::HelloOk {
                bootstrap_limits,
                ..
            } if bootstrap_limits == offered_limits
        ));
        send_frame(
            &mut stream,
            &FrameKind::BootstrapChunk {
                terminal_id: TerminalId::local(1),
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                chunk_seq: 0,
                payload: bytes::Bytes::from(vec![0; 1025]),
            },
        )
        .await;
        assert!(matches!(
            recv_typed(&mut stream).await.1,
            FrameKind::Error {
                code: ErrorCode::MalformedMessage,
                ..
            }
        ));
        expect_protocol_error_close(&mut stream, WIRE_RECV_TIMEOUT).await;
        shutdown_and_join(shutdown_tx, server_handle, &socket_path).await;
    });
}

#[test]
fn incompatible_handshake_returns_actionable_error() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("default"));

        let cases = [
            (
                PROTOCOL_VERSION.minor + 1,
                "update the phux server",
                "newer client",
            ),
            (
                PROTOCOL_VERSION.minor - 1,
                "update the phux app/client",
                "older client",
            ),
        ];
        for (minor, remediation, label) in cases {
            let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
            send_frame(
                &mut stream,
                &hello_for_version(PROTOCOL_VERSION.major, minor, 0),
            )
            .await;
            let (type_byte, response) = recv_typed(&mut stream).await;
            assert_eq!(type_byte, TYPE_ERROR, "{label} must receive ERROR");
            match response {
                FrameKind::Error { code, message, .. } => {
                    assert_eq!(code, ErrorCode::VersionIncompatible);
                    assert!(
                        message.contains(&format!(
                            "client offered {}.{minor}.0",
                            PROTOCOL_VERSION.major
                        )),
                        "{label} error must name the offered version: {message}",
                    );
                    assert!(
                        message.contains(remediation),
                        "{label} error must prescribe the correct upgrade: {message}",
                    );
                }
                other => panic!("expected VERSION_INCOMPATIBLE, got {other:?}"),
            }
            expect_protocol_error_close(&mut stream, WIRE_RECV_TIMEOUT).await;
        }

        shutdown_and_join(shutdown_tx, server_handle, &socket_path).await;
    });
}

#[test]
fn uds_requires_hello_for_stateful_frames_but_ping_remains_stateless() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("default"));

        let mut attach_stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut attach_stream, &attach_by_name("default")).await;
        let (_, response) = recv_typed(&mut attach_stream).await;
        assert!(matches!(
            response,
            FrameKind::Error {
                code: ErrorCode::VersionIncompatible,
                ..
            }
        ));
        expect_protocol_error_close(&mut attach_stream, WIRE_RECV_TIMEOUT).await;

        let mut ping_stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut ping_stream, &FrameKind::Ping { nonce: 0x5eed }).await;
        let (_, pong) = recv_typed(&mut ping_stream).await;
        assert!(matches!(pong, FrameKind::Pong { nonce: 0x5eed }));

        send_frame(
            &mut ping_stream,
            &FrameKind::Command {
                request_id: 7,
                command: Command::GetState {
                    scope: phux_protocol::wire::frame::StateScope::Server,
                },
            },
        )
        .await;
        let (_, response) = recv_typed(&mut ping_stream).await;
        assert!(matches!(
            response,
            FrameKind::Error {
                code: ErrorCode::VersionIncompatible,
                ..
            }
        ));
        expect_protocol_error_close(&mut ping_stream, WIRE_RECV_TIMEOUT).await;

        shutdown_and_join(shutdown_tx, server_handle, &socket_path).await;
    });
}

#[test]
fn post_attach_duplicate_hello_is_rejected_and_flushed() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("default"));

        let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &hello_frame()).await;
        assert!(matches!(
            recv_typed(&mut stream).await.1,
            FrameKind::HelloOk { .. }
        ));
        send_frame(&mut stream, &attach_by_name("default")).await;
        assert!(matches!(
            recv_typed(&mut stream).await.1,
            FrameKind::Attached { .. }
        ));
        let _ = recv_typed(&mut stream).await;

        let changed_caps = FrameKind::Hello {
            client_name: "mutating-duplicate".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new(),
        };
        send_frame(&mut stream, &changed_caps).await;
        let response = loop {
            let (_, frame) = recv_typed(&mut stream).await;
            if matches!(frame, FrameKind::Error { .. }) {
                break frame;
            }
        };
        match response {
            FrameKind::Error { code, message, .. } => {
                assert_eq!(code, ErrorCode::InvalidCommand);
                assert!(message.contains("HELLO already completed"));
            }
            other => panic!("expected duplicate HELLO error, got {other:?}"),
        }
        expect_protocol_error_close(&mut stream, WIRE_RECV_TIMEOUT).await;

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

        let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
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

// NOTE: `attach_returns_snapshot` was removed as fully subsumed by
// `byc_6_1_attach_snapshot::byc_6_1_attach_returns_session_id_and_round_trip_snapshot`
// (same spawn_server path, identical assertions plus the ADR-0013 round-trip
// fixed-point check). `input_routes_to_pane` was removed because the
// cat-echo INPUT_KEY round-trip is asserted at Screen level in
// `reconnect_scenario` and `multi_client_scenario` (and again in
// `input_dispatch` / `pty_pump`).

#[test]
fn detach_clean_shutdown() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("default"));

        let mut client_a = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        negotiate(&mut client_a).await;
        send_frame(&mut client_a, &attach_by_name("default")).await;

        let (type_byte, _attached_a) = recv_typed(&mut client_a).await;
        assert_eq!(type_byte, TYPE_ATTACHED);
        let (type_byte, _snap_a) = recv_typed(&mut client_a).await;
        assert_eq!(type_byte, TYPE_BOOTSTRAP_BEGIN);

        send_frame(&mut client_a, &FrameKind::Detach).await;
        let detached = recv_until_detached(&mut client_a).await;
        assert!(matches!(
            detached,
            FrameKind::Detached {
                reason: Some(DetachReason::Requested),
                ..
            }
        ));
        drop(client_a);

        // Server still accepting: a fresh client must complete an
        // ATTACH against the same runtime instance.
        let mut client_b = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        negotiate(&mut client_b).await;
        send_frame(&mut client_b, &attach_by_name("default")).await;
        let (type_byte, _attached_b) = recv_typed(&mut client_b).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "post-detach reattach must succeed",
        );
        let (type_byte, _snap_b) = recv_typed(&mut client_b).await;
        assert_eq!(type_byte, TYPE_BOOTSTRAP_BEGIN);

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

        let mut client_a = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        let mut client_b = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        for client in [&mut client_a, &mut client_b] {
            negotiate(client).await;
            send_frame(client, &attach_by_name("default")).await;
            assert_eq!(recv_typed(client).await.0, TYPE_ATTACHED);
            assert_eq!(recv_typed(client).await.0, TYPE_BOOTSTRAP_BEGIN);
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
        let mut partial = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
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
        let mut healthy: UnixStream =
            wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        negotiate(&mut healthy).await;
        send_frame(&mut healthy, &attach_by_name("default")).await;
        let (type_byte, _attached) = recv_typed(&mut healthy).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "server must keep accepting after a mid-frame disconnect",
        );
        let (type_byte, _snap) = recv_typed(&mut healthy).await;
        assert_eq!(type_byte, TYPE_BOOTSTRAP_BEGIN);

        drop(healthy);
        shutdown_and_join(shutdown_tx, server_handle, &socket_path).await;
    });
}
