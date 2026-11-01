use super::dispatch::handle_client;
use super::writer_close_tests::Script;
use crate::state::SharedState;
use crate::transport::FrameWriter;
use phux_protocol::PROTOCOL_VERSION;
use std::io;
use tokio::task::LocalSet;
use tokio_util::sync::CancellationToken;

use phux_protocol::caps::{
    BootstrapCapabilities, ClientCapabilities, EngineCodec, EngineFeatureSet,
};
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::wire::frame::{AttachTarget, DetachReason, ErrorCode, FrameKind, ViewportInfo};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc};

use phux_terminal::actor::{ConsumerAttachOutcome, ResourceOutput};

struct DuplexWriter(tokio::io::WriteHalf<tokio::io::DuplexStream>);

impl FrameWriter for DuplexWriter {
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.0.write_all(frame).await
    }

    async fn close(&mut self) -> io::Result<()> {
        self.0.shutdown().await
    }
}

/// The next frame the peer reads, or `None` at EOF; one second at most.
async fn next_frame(
    reader: &mut tokio::io::ReadHalf<tokio::io::DuplexStream>,
) -> Option<FrameKind> {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        let mut header = [0_u8; 4];
        match reader.read_exact(&mut header).await {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return None,
            Err(error) => panic!("read: {error}"),
        }
        let mut framed = phux_protocol::wire::framing::frame_buffer(header).expect("frame");
        reader
            .read_exact(&mut framed[phux_protocol::wire::framing::LENGTH_PREFIX_LEN..])
            .await
            .expect("frame body");
        Some(FrameKind::decode(&framed).expect("decode").0)
    })
    .await
    .expect("peer read timed out")
}

fn native_failure_handle() -> (
    crate::resource::ResourceHandle,
    mpsc::Receiver<phux_terminal::actor::ConsumerAttachRequest>,
    mpsc::Receiver<phux_terminal::actor::NativeBootstrapRequest>,
) {
    let (output, _output_seed) = broadcast::channel::<ResourceOutput>(8);
    let (consumer_attach, consumer_attach_rx) = mpsc::channel(8);
    let (native_bootstrap, native_bootstrap_rx) = mpsc::channel(8);
    (
        crate::resource::ResourceHandle::new(
            None,
            output,
            phux_terminal::actor::TerminalHandle {
                consumer_attach,
                consumer_detach: mpsc::channel(8).0,
                consumer_ack: mpsc::channel(8).0,
                upgrade: mpsc::channel(8).0,
                native_bootstrap,
                ..phux_terminal::actor::TerminalHandle::detached_for_test(80, 24)
            },
        ),
        consumer_attach_rx,
        native_bootstrap_rx,
    )
}

/// A native bootstrap preflight failure is a terminal error: the peer
/// reads `HELLO_OK`, ERROR, DETACHED, and only then EOF.
#[tokio::test(flavor = "current_thread")]
async fn native_preflight_failure_flushes_error_then_duplex_eof() {
    LocalSet::new()
        .run_until(async {
            let state = SharedState::new();
            let (_session, terminal) = state.with_mut(|server| server.seed_session("fatal-duplex"));
            let (handle, mut consumer_attach_rx, mut native_bootstrap_rx) = native_failure_handle();
            let client_id = state.with_mut(|server| {
                let _ = server.register_resource_handle(terminal, handle, CancellationToken::new());
                let client_id = server.new_client_id();
                server.set_peer_identity(
                    client_id,
                    PeerIdentity {
                        uid: nix::unistd::geteuid().as_raw(),
                        pid: None,
                        exe_path: None,
                        mcp_host_key: None,
                        transport: TransportType::UnixSocket,
                        source_addr: None,
                    },
                );
                client_id
            });

            let native = BootstrapCapabilities::new().with_native(
                EngineCodec::LibghosttySnapshotV1,
                EngineFeatureSet::required_native(),
            );
            let reader = Script::new([
                FrameKind::Hello {
                    client_name: "fatal-duplex-test".to_owned(),
                    protocol_major: PROTOCOL_VERSION.major,
                    protocol_minor: PROTOCOL_VERSION.minor,
                    protocol_patch: PROTOCOL_VERSION.patch,
                    client_caps: ClientCapabilities::new().with_bootstrap(native),
                },
                FrameKind::Attach {
                    attach_id: 1,
                    target: AttachTarget::ByName("fatal-duplex".to_owned()),
                    viewport: ViewportInfo::new(80, 24),
                    request_scrollback: false,
                    scrollback_limit_lines: 0,
                    role_policy: None,
                },
            ]);
            let (server_io, peer_io) = tokio::io::duplex(64 * 1024);
            let (_server_read, server_write) = tokio::io::split(server_io);
            let (mut client_read, _client_write) = tokio::io::split(peer_io);
            let task = tokio::task::spawn_local(handle_client(
                reader,
                DuplexWriter(server_write),
                state,
                client_id,
                CancellationToken::new(),
                CancellationToken::new(),
                None,
                TransportType::UnixSocket,
                false,
            ));
            let actor = tokio::task::spawn_local(async move {
                let registration = consumer_attach_rx.recv().await.expect("registration");
                registration
                    .reply
                    .send(Ok(ConsumerAttachOutcome {
                        tick_managed: false,
                        state_sync_bootstrap: None,
                    }))
                    .expect("registration reply");
                native_bootstrap_rx
                    .recv()
                    .await
                    .expect("native preflight")
                    .reply
                    .send(Err(
                        phux_terminal::native_state::NativeStateError::OutOfMemory,
                    ))
                    .expect("native failure reply");
            });

            assert!(matches!(
                next_frame(&mut client_read).await,
                Some(FrameKind::HelloOk {
                    selected_profile: phux_protocol::caps::BootstrapProfile::NativeState {
                        codec: EngineCodec::LibghosttySnapshotV1,
                        ..
                    },
                    ..
                })
            ));
            assert!(matches!(
                next_frame(&mut client_read).await,
                Some(FrameKind::Error {
                    code: ErrorCode::CodecUnavailable,
                    ..
                })
            ));
            assert!(matches!(
                next_frame(&mut client_read).await,
                Some(FrameKind::Detached {
                    reason: Some(DetachReason::ProtocolError),
                    ..
                })
            ));
            assert!(next_frame(&mut client_read).await.is_none());
            actor.await.expect("actor task");
            task.await.expect("client task").expect("client result");
        })
        .await;
}
