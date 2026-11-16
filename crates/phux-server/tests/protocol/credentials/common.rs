//! Real owner-UDS clients: admission, detached dispatch, approval metadata and journal.

use std::sync::Arc;
use std::time::Duration;

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, LayerSet};
use phux_protocol::credentials::{CredentialOp, CredentialRequest, SecretValues};
use phux_protocol::ids::ApprovalId;
use phux_protocol::wire::frame::{
    APPROVAL_APPROVE, APPROVAL_DENY, AgentEvent, ApprovalOutcome, Command, CommandResult,
    CommandValue, ErrorCode, FrameKind, Scope, StateScope,
};
use phux_server::policy::PolicyEngine;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, ServerHandles, WIRE_RECV_TIMEOUT, await_command_result, command,
    join_after_shutdown, recv_typed, recv_until, send_frame, spawn_server_with,
    wait_for_raw_socket,
};
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;

pub(super) const HELD: u32 = 100;
pub(super) const PROVIDER: &str = "vault-mac";

pub(super) struct Server {
    tmp: TempDir,
    handles: ServerHandles,
}

impl Server {
    pub(super) fn start(ttl_secs: u32, policy: Option<Arc<dyn PolicyEngine>>) -> Self {
        let tmp = TempDir::new().unwrap();
        let handles = spawn_server_with(tmp.path().join("phux.sock"), Some("demo"), |cfg| {
            cfg.approval_ttl_secs = ttl_secs;
            cfg.policy_engine = policy;
        });
        Self { tmp, handles }
    }

    pub(super) async fn connect(&self, name: &str) -> UnixStream {
        self.connect_with_credential_details(name, true).await
    }

    pub(super) async fn connect_with_credential_details(
        &self,
        name: &str,
        details: bool,
    ) -> UnixStream {
        let mut stream =
            wait_for_raw_socket(&self.tmp.path().join("phux.sock"), SOCKET_CONNECT_DEADLINE).await;
        send_frame(
            &mut stream,
            &FrameKind::Hello {
                client_name: name.to_owned(),
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                client_caps: ClientCapabilities::new()
                    .with_layers(LayerSet::all())
                    .with_credential_approvals(details),
            },
        )
        .await;
        assert!(matches!(
            recv_typed(&mut stream).await.1,
            FrameKind::HelloOk { .. }
        ));
        stream
    }

    pub(super) async fn observer(&self) -> UnixStream {
        let mut stream = self.connect("credential-observer").await;
        send_frame(
            &mut stream,
            &FrameKind::SubscribeEvents {
                terminal: None,
                after_seq: None,
            },
        )
        .await;
        barrier(&mut stream).await;
        stream
    }

    pub(super) async fn stop(self) {
        join_after_shutdown(self.handles.0, self.handles.1).await;
    }
}

pub(super) fn request() -> CredentialRequest {
    CredentialRequest {
        provider: PROVIDER.to_owned(),
        references: vec!["op://dev/github/token".to_owned()],
        purpose: "run the deployment client".to_owned(),
    }
}

pub(super) async fn op(stream: &mut UnixStream, id: u32, op: CredentialOp) -> CommandResult {
    command(stream, id, Command::Credentials { op }).await
}

pub(super) async fn next(stream: &mut UnixStream, provider: &str) -> CommandResult {
    op(
        stream,
        1,
        CredentialOp::Next {
            provider: provider.to_owned(),
        },
    )
    .await
}

pub(super) async fn begin(stream: &mut UnixStream) {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id: HELD,
            command: Command::Credentials {
                op: CredentialOp::Request(request()),
            },
        },
    )
    .await;
}

pub(super) async fn opened(observer: &mut UnixStream) -> ApprovalId {
    recv_until(observer, |_, frame| match frame {
        FrameKind::Event {
            event: AgentEvent::ApprovalRequested { id },
            ..
        } => Some(id),
        _ => None,
    })
    .await
}

pub(super) async fn ended(observer: &mut UnixStream, expected: ApprovalId) -> ApprovalOutcome {
    recv_until(observer, |_, frame| match frame {
        FrameKind::Event {
            event: AgentEvent::ApprovalDecided { id, outcome },
            ..
        } if id == expected => Some(outcome),
        _ => None,
    })
    .await
}

pub(super) async fn metadata(stream: &mut UnixStream, key: &str) -> Option<Vec<u8>> {
    send_frame(
        stream,
        &FrameKind::GetMetadata {
            request_id: 3,
            scope: Scope::Global,
            key: key.to_owned(),
        },
    )
    .await;
    recv_until(stream, |_, frame| match frame {
        FrameKind::MetadataValue {
            request_id: 3,
            value,
        } => Some(value),
        other @ FrameKind::Error { .. } => panic!("metadata refused: {other:?}"),
        _ => None,
    })
    .await
}

pub(super) async fn decide(stream: &mut UnixStream, id: ApprovalId, approve: bool) {
    send_frame(
        stream,
        &FrameKind::SetMetadata {
            request_id: 4,
            scope: Scope::Global,
            key: id.decide_key(),
            value: if approve {
                APPROVAL_APPROVE
            } else {
                APPROVAL_DENY
            }
            .to_vec(),
        },
    )
    .await;
    // Successful SET_METADATA has no reply; the next same-connection reply is a barrier.
    barrier(stream).await;
}

pub(super) async fn barrier(stream: &mut UnixStream) {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id: 5,
            command: Command::GetState {
                scope: StateScope::Server,
            },
        },
    )
    .await;
    let result = recv_until(stream, |_, frame| match frame {
        FrameKind::CommandResult {
            request_id: HELD, ..
        } => panic!("held request answered before approval"),
        FrameKind::CommandResult {
            request_id: 5,
            result,
        } => Some(result),
        FrameKind::Error { code, message, .. } => panic!("barrier refused: {code:?}: {message}"),
        _ => None,
    })
    .await;
    assert!(
        matches!(result, CommandResult::OkWith(CommandValue::State(_))),
        "{result:?}"
    );
}

pub(super) async fn register(stream: &mut UnixStream) {
    tokio::time::timeout(WIRE_RECV_TIMEOUT, async {
        loop {
            if next(stream, PROVIDER).await == CommandResult::Ok {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("disconnected provider's label becomes reusable");
}

pub(super) async fn disconnect(mut stream: UnixStream) {
    stream.shutdown().await.expect("half-close the peer");
    // handle_client withdraws approvals and broker entries before closing its writer.
    let mut remaining = Vec::new();
    tokio::time::timeout(WIRE_RECV_TIMEOUT, stream.read_to_end(&mut remaining))
        .await
        .expect("server acknowledges the disconnect")
        .expect("clean server EOF");
}

pub(super) async fn claim(stream: &mut UnixStream) -> Value {
    tokio::time::timeout(WIRE_RECV_TIMEOUT, async {
        loop {
            match next(stream, PROVIDER).await {
                CommandResult::Ok => tokio::time::sleep(Duration::from_millis(5)).await,
                CommandResult::OkWith(CommandValue::Json(json)) => {
                    return serde_json::from_str(&json).unwrap();
                }
                other => panic!("claim refused: {other:?}"),
            }
        }
    })
    .await
    .expect("approved request becomes claimable")
}

pub(super) async fn complete(
    stream: &mut UnixStream,
    id: ApprovalId,
    values: Option<Vec<String>>,
) -> CommandResult {
    op(
        stream,
        6,
        CredentialOp::Complete {
            id: id.to_string(),
            values: values.map(SecretValues),
        },
    )
    .await
}

pub(super) fn denied(result: &CommandResult) {
    assert!(
        matches!(
            result,
            CommandResult::Error {
                code: ErrorCode::PermissionDenied,
                ..
            }
        ),
        "{result:?}"
    );
}

pub(super) async fn answer(stream: &mut UnixStream) -> CommandResult {
    await_command_result(stream, HELD).await
}
