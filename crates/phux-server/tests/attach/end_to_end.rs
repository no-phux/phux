//! HELLO negotiation, fatal handshake violations, and server lifecycle over UDS.

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfile, BootstrapProfileKind,
    BootstrapProfileSet, ClientCapabilities, ColorSupport, EngineCodecSet, EngineFeatureSet,
    LayerSet, ServerFeature,
};
use phux_protocol::ids::{BootstrapId, FileUploadId, ResourceId, StreamId};
use phux_protocol::wire::frame::{
    AttachTarget, Command, CommandResult, CommandValue, ErrorCode, FileUploadAck, FrameKind,
    StateScope, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN, ViewportInfo,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, command,
    expect_protocol_error_close, join_after_shutdown, recv_typed, recv_until, run_local,
    send_frame, spawn_server_with, wait_for_raw_socket,
};

fn hello_with(minor: u16, client_caps: ClientCapabilities) -> FrameKind {
    FrameKind::Hello {
        client_name: "phux-end-to-end-test".to_owned(),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: minor,
        protocol_patch: 0,
        client_caps,
    }
}

fn hello() -> FrameKind {
    let caps = ClientCapabilities::new()
        .with_color_support(ColorSupport::TrueColor)
        .with_layers(LayerSet::all());
    FrameKind::Hello {
        client_name: "phux-end-to-end-test".to_owned(),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        client_caps: caps,
    }
}

const fn synth_only(limits: BootstrapLimits) -> ClientCapabilities {
    ClientCapabilities::new().with_bootstrap(
        BootstrapCapabilities::new()
            .with_profiles(BootstrapProfileSet::with(&[
                BootstrapProfileKind::SynthesizedVtRaw,
            ]))
            .with_native_codecs(EngineCodecSet::new())
            .with_native_features(EngineFeatureSet::new())
            .with_limits(limits),
    )
}

fn server() -> (
    TempDir,
    std::path::PathBuf,
    phux_server_testkit::ServerHandles,
) {
    server_with(|_| {})
}

fn server_with(
    configure: impl FnOnce(&mut phux_server::ServerConfig),
) -> (
    TempDir,
    std::path::PathBuf,
    phux_server_testkit::ServerHandles,
) {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("phux.sock");
    let handles = spawn_server_with(socket.clone(), Some("default"), configure);
    (tmp, socket, handles)
}

async fn raw(socket: &std::path::Path) -> UnixStream {
    wait_for_raw_socket(socket, SOCKET_CONNECT_DEADLINE).await
}

async fn attach_default(stream: &mut UnixStream) {
    send_frame(stream, &attach_by_name("default")).await;
    assert_eq!(recv_typed(stream).await.0, TYPE_ATTACHED);
    assert_eq!(recv_typed(stream).await.0, TYPE_BOOTSTRAP_BEGIN);
}

#[test]
fn hello_ok_advertises_the_reference_server_and_one_incarnation() {
    run_local(async {
        let (_tmp, socket, (shutdown, server)) = server();
        let mut stream = raw(&socket).await;
        send_frame(&mut stream, &hello()).await;
        let FrameKind::HelloOk {
            protocol_major,
            protocol_minor,
            server_caps,
            server_id,
            selected_profile,
            bootstrap_limits,
            ..
        } = recv_typed(&mut stream).await.1
        else {
            panic!("HELLO must be answered with HELLO_OK");
        };
        assert_eq!(
            (protocol_major, protocol_minor),
            (PROTOCOL_VERSION.major, PROTOCOL_VERSION.minor)
        );
        assert_eq!(server_caps.layers, LayerSet::all());
        assert!(
            server_caps
                .features
                .contains(ServerFeature::AcknowledgedInput)
        );
        assert!(server_caps.features.contains(ServerFeature::FileUpload));
        assert_eq!(selected_profile, BootstrapProfile::SynthesizedVtRaw);
        assert_eq!(bootstrap_limits, BootstrapLimits::default());
        assert_eq!(server_id.len(), 16);

        let mut second = raw(&socket).await;
        send_frame(&mut second, &hello()).await;
        assert!(matches!(
            recv_typed(&mut second).await.1,
            FrameKind::HelloOk { server_id: id, .. } if id == server_id
        ));

        // An explicit synth-only offer selects it and intersects the limits.
        let limits = BootstrapLimits::new(64 * 1024, 128 * 1024).unwrap();
        let mut synth = raw(&socket).await;
        send_frame(
            &mut synth,
            &hello_with(PROTOCOL_VERSION.minor, synth_only(limits)),
        )
        .await;
        assert!(matches!(
            recv_typed(&mut synth).await.1,
            FrameKind::HelloOk {
                selected_profile: BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits,
                ..
            } if bootstrap_limits == limits
        ));

        attach_default(&mut stream).await;
        drop((stream, second, synth));
        join_after_shutdown(shutdown, server).await;
        assert!(!socket.exists(), "socket unlinked after shutdown");
    });
}

/// Each case is a fresh connection whose frames end in a fatal protocol
/// error: an `ERROR` with the expected code and message, then SPEC §14's
/// `DETACHED { PROTOCOL_ERROR }` and close.
#[test]
#[allow(clippy::too_many_lines, reason = "one table of handshake cases")]
fn handshake_violations_are_fatal_with_actionable_errors() {
    run_local(async {
        let (_tmp, socket, (shutdown, server)) = server();
        let native_only = ClientCapabilities::new().with_bootstrap(
            BootstrapCapabilities::new()
                .with_profiles(BootstrapProfileSet::with(&[
                    BootstrapProfileKind::NativeState,
                ]))
                .with_native_codecs(EngineCodecSet::new()),
        );
        let hello_ok = FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: phux_protocol::caps::ServerCapabilities::new(),
            server_id: Vec::new(),
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        };
        let oversized_chunk = FrameKind::BootstrapChunk {
            terminal_id: ResourceId::local(1),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            chunk_seq: 0,
            payload: bytes::Bytes::from(vec![0; 1025]),
        };
        let zero_attach_id = FrameKind::Attach {
            attach_id: 0,
            target: AttachTarget::ByName("default".to_owned()),
            viewport: ViewportInfo::new(80, 24),
            request_scrollback: false,
            scrollback_limit_lines: 0,
            role_policy: None,
        };
        let newer = format!(
            "client offered {}.{}.0",
            PROTOCOL_VERSION.major,
            PROTOCOL_VERSION.minor + 1
        );
        let older = format!(
            "client offered {}.{}.0",
            PROTOCOL_VERSION.major,
            PROTOCOL_VERSION.minor - 1
        );
        let cases: Vec<(Vec<FrameKind>, ErrorCode, Vec<&str>)> = vec![
            (
                vec![hello_with(
                    PROTOCOL_VERSION.minor + 1,
                    ClientCapabilities::new(),
                )],
                ErrorCode::VersionIncompatible,
                vec![&newer, "update the phux server"],
            ),
            (
                vec![hello_with(
                    PROTOCOL_VERSION.minor - 1,
                    ClientCapabilities::new(),
                )],
                ErrorCode::VersionIncompatible,
                vec![&older, "update the phux app/client"],
            ),
            (
                vec![hello_with(PROTOCOL_VERSION.minor, native_only)],
                ErrorCode::CodecUnavailable,
                vec!["exact common codec", "SynthesizedVtRaw"],
            ),
            (
                vec![attach_by_name("default")],
                ErrorCode::VersionIncompatible,
                vec![],
            ),
            (
                vec![hello(), zero_attach_id],
                ErrorCode::MalformedMessage,
                vec!["attach_id must be nonzero"],
            ),
            (vec![hello(), hello_ok], ErrorCode::InvalidCommand, vec![]),
            (
                vec![
                    hello(),
                    attach_by_name("default"),
                    hello_with(PROTOCOL_VERSION.minor, ClientCapabilities::new()),
                ],
                ErrorCode::InvalidCommand,
                vec!["HELLO already completed"],
            ),
            (
                vec![
                    hello_with(
                        PROTOCOL_VERSION.minor,
                        synth_only(BootstrapLimits::new(1024, 2048).unwrap()),
                    ),
                    oversized_chunk,
                ],
                ErrorCode::MalformedMessage,
                vec![],
            ),
        ];
        for (frames, expected, fragments) in cases {
            let mut stream = raw(&socket).await;
            for frame in &frames {
                send_frame(&mut stream, frame).await;
            }
            let (code, message) = recv_until(&mut stream, |_, frame| match frame {
                FrameKind::Error { code, message, .. } => Some((code, message)),
                _ => None,
            })
            .await;
            assert_eq!(code, expected, "{frames:?}: {message}");
            for fragment in fragments {
                assert!(message.contains(fragment), "{message:?} lacks {fragment:?}");
            }
            expect_protocol_error_close(&mut stream, WIRE_RECV_TIMEOUT).await;
        }

        // PING is stateless before HELLO; a COMMAND is not.
        let mut stream = raw(&socket).await;
        send_frame(&mut stream, &FrameKind::Ping { nonce: 0x5eed }).await;
        assert!(matches!(
            recv_typed(&mut stream).await.1,
            FrameKind::Pong { nonce: 0x5eed }
        ));
        let get_state = FrameKind::Command {
            request_id: 7,
            command: Command::GetState {
                scope: StateScope::Server,
            },
        };
        send_frame(&mut stream, &get_state).await;
        assert!(matches!(
            recv_typed(&mut stream).await.1,
            FrameKind::Error {
                code: ErrorCode::VersionIncompatible,
                ..
            }
        ));
        expect_protocol_error_close(&mut stream, WIRE_RECV_TIMEOUT).await;

        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn put_file_round_trip_publishes_only_the_verified_file() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let upload_dir = tmp.path().join("uploads");
        let (_tmp, socket, (shutdown, server)) =
            server_with(|cfg| cfg.env.upload_dir = Some(upload_dir.clone()));
        let mut stream = raw(&socket).await;
        send_frame(&mut stream, &hello()).await;
        recv_typed(&mut stream).await;
        send_frame(&mut stream, &attach_by_name("default")).await;
        let FrameKind::Attached { snapshot, .. } = recv_typed(&mut stream).await.1 else {
            panic!("expected ATTACHED");
        };
        let terminal_id = snapshot.resources[0].id.clone();

        let bytes = b"wire-native image bytes";
        let split = 7;
        let upload_id = FileUploadId::new([0x5a; 16]).unwrap();
        let chunk =
            |offset: usize, end: usize, final_chunk: bool, extension: &str| Command::PutFile {
                upload_id,
                terminal_id: terminal_id.clone(),
                extension: extension.to_owned(),
                offset: offset as u64,
                data: bytes[offset..end].to_vec(),
                final_chunk,
                sha256: final_chunk.then(|| Sha256::digest(bytes).into()),
            };
        assert_eq!(
            command(&mut stream, 41, chunk(0, split, false, "PNG")).await,
            CommandResult::OkWith(CommandValue::FileUpload(FileUploadAck {
                next_offset: split as u64,
                path: None,
            }))
        );
        assert!(
            std::fs::read_dir(&upload_dir).unwrap().all(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with('.')),
            "a non-final chunk must expose no public file"
        );
        let CommandResult::OkWith(CommandValue::FileUpload(ack)) =
            command(&mut stream, 42, chunk(split, bytes.len(), true, "png")).await
        else {
            panic!("expected a file-upload ack");
        };
        assert_eq!(ack.next_offset, bytes.len() as u64);
        let path = ack.path.expect("verified final path");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(
            std::path::Path::new(&path).starts_with(&upload_dir),
            "inside the sandbox"
        );

        drop(stream);
        join_after_shutdown(shutdown, server).await;
    });
}

/// A mid-frame disconnect must not wedge the accept loop, and shutdown must
/// drop live client futures (which own input-lane handles) before joining
/// the input lane.
#[test]
fn server_survives_mid_frame_disconnect_and_shuts_down_with_live_clients() {
    run_local(async {
        let (_tmp, socket, (shutdown, server)) = server();

        let mut partial = raw(&socket).await;
        partial.write_all(&64u32.to_be_bytes()).await.unwrap();
        let _ = AsyncWriteExt::shutdown(&mut partial).await;
        drop(partial);

        let mut live = Vec::new();
        for _ in 0..2 {
            let mut client = raw(&socket).await;
            send_frame(&mut client, &hello()).await;
            recv_typed(&mut client).await;
            attach_default(&mut client).await;
            live.push(client);
        }
        join_after_shutdown(shutdown, server).await;
        assert!(!socket.exists(), "socket unlinked after shutdown");
        drop(live);
    });
}
