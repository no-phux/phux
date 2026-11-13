//! A provider, requester and approving client on the remote-client transport.

use std::net::Ipv4Addr;

use futures_util::{SinkExt, StreamExt};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, LayerSet};
use phux_protocol::credentials::{CredentialOp, SecretValues};
use phux_protocol::wire::RemoteListenerTransport;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, FrameKind, Scope};
use phux_server::ServerConfig;
use phux_server_testkit::{WIRE_RECV_TIMEOUT, bound_listener_addr, encode_frame_vec, run_local};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

type Ws = WebSocketStream<TcpStream>;

async fn send(ws: &mut Ws, frame: &FrameKind) {
    ws.send(Message::Binary(encode_frame_vec(frame).into()))
        .await
        .unwrap();
}

async fn receive(ws: &mut Ws) -> FrameKind {
    loop {
        let message = tokio::time::timeout(WIRE_RECV_TIMEOUT, ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Binary(bytes) = message {
            let (frame, rest) = FrameKind::decode(&bytes).unwrap();
            assert!(rest.is_empty());
            return frame;
        }
    }
}

async fn connect(addr: std::net::SocketAddr) -> Ws {
    let tcp = TcpStream::connect(addr).await.unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async(format!("ws://{addr}/"), tcp)
        .await
        .unwrap();
    send(
        &mut ws,
        &FrameKind::Hello {
            client_name: "remote-credentials".into(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::default()
                .with_layers(LayerSet::all())
                .with_credential_approvals(true),
        },
    )
    .await;
    assert!(matches!(receive(&mut ws).await, FrameKind::HelloOk { .. }));
    ws
}

async fn command(ws: &mut Ws, request_id: u32, op: CredentialOp) -> CommandResult {
    send(
        ws,
        &FrameKind::Command {
            request_id,
            command: Command::Credentials { op },
        },
    )
    .await;
    match receive(ws).await {
        FrameKind::CommandResult {
            request_id: actual,
            result,
        } => {
            assert_eq!(actual, request_id);
            result
        }
        other => panic!("unexpected frame {other:?}"),
    }
}

async fn pending_approval(approver: &mut Ws) -> phux_protocol::ids::ApprovalId {
    tokio::time::timeout(WIRE_RECV_TIMEOUT, async {
        loop {
            send(
                approver,
                &FrameKind::ListMetadata {
                    request_id: 2,
                    scope: Scope::Global,
                },
            )
            .await;
            if let FrameKind::MetadataKeys { keys, .. } = receive(approver).await
                && let Some(id) = keys.iter().find_map(|key| {
                    key.strip_prefix(phux_protocol::wire::frame::APPROVAL_KEY_PREFIX)
                        .and_then(phux_protocol::ids::ApprovalId::parse)
                })
            {
                break id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

async fn claimed_job(provider: &mut Ws) -> serde_json::Value {
    tokio::time::timeout(WIRE_RECV_TIMEOUT, async {
        loop {
            if let CommandResult::OkWith(CommandValue::Json(job)) = command(
                provider,
                5,
                CredentialOp::Next {
                    provider: super::common::PROVIDER.into(),
                },
            )
            .await
            {
                break serde_json::from_str(&job).unwrap();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[test]
fn all_three_roles_work_over_the_remote_websocket_transport() {
    run_local(async {
        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("remote.sock");
        let mut cfg = ServerConfig {
            socket_path: socket_path.clone(),
            ..ServerConfig::with_default_socket()
        };
        // Admit loopback peers through the same explicit embedder authority seam
        // used by transport qualification. Production remote authentication is unchanged.
        phux_server_testkit::embedder::embed_owner_engine(&mut cfg);
        let (stop, shutdown) = oneshot::channel();
        let server = tokio::task::spawn_local(async move {
            phux_server_testkit::terminal_server(cfg)
                .listen_ws((Ipv4Addr::LOCALHOST, 0).into())
                .run_async(async {
                    let _ = shutdown.await;
                })
                .await
        });
        let addr = bound_listener_addr(&socket_path, RemoteListenerTransport::Wss).await;
        let mut provider = connect(addr).await;
        let mut requester = connect(addr).await;
        let mut approver = connect(addr).await;
        assert_eq!(
            command(
                &mut provider,
                1,
                CredentialOp::Next {
                    provider: super::common::PROVIDER.into()
                }
            )
            .await,
            CommandResult::Ok
        );
        send(
            &mut requester,
            &FrameKind::Command {
                request_id: 8,
                command: Command::Credentials {
                    op: CredentialOp::Request(super::common::request()),
                },
            },
        )
        .await;

        // Poll metadata on the approving connection; it never receives values.
        let id = pending_approval(&mut approver).await;
        assert_eq!(
            command(
                &mut provider,
                3,
                CredentialOp::Next {
                    provider: super::common::PROVIDER.into()
                }
            )
            .await,
            CommandResult::Ok
        );
        send(
            &mut approver,
            &FrameKind::SetMetadata {
                request_id: 4,
                scope: Scope::Global,
                key: id.decide_key(),
                value: b"approve".to_vec(),
            },
        )
        .await;
        let job = claimed_job(&mut provider).await;
        assert_eq!(job["id"], id.to_string());
        let values = SecretValues(vec!["remote-in-memory-canary".into()]);
        assert_eq!(
            command(
                &mut provider,
                6,
                CredentialOp::Complete {
                    id: id.to_string(),
                    values: Some(values.clone())
                }
            )
            .await,
            CommandResult::Ok
        );
        assert_eq!(
            receive(&mut requester).await,
            FrameKind::CommandResult {
                request_id: 8,
                result: CommandResult::OkWith(CommandValue::CredentialSecrets(values))
            }
        );
        drop((provider, requester, approver));
        let _ = stop.send(());
        server.await.unwrap().unwrap();
    });
}
