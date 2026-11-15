//! Revocation at the sensitive boundary: approved, claimed, not yet delivered.
use super::*;
use phux_protocol::caps::LayerSet;
use phux_protocol::credentials::{CredentialOp, CredentialRequest, SecretValues};
use phux_protocol::ids::ApprovalId;
use phux_protocol::wire::frame::{
    APPROVAL_APPROVE, Command, CommandResult, CommandValue, DetachReason, ErrorCode, Scope,
};

type Client = WebSocketStream<TcpStream>;
const STEP: Duration = Duration::from_secs(5);
const PROVIDER: &str = "revocation-provider";
const REQUEST_ID: u32 = 100;

async fn send(client: &mut Client, frame: FrameKind) {
    let mut bytes = BytesMut::new();
    frame.encode(&mut bytes);
    client
        .send(Message::Binary(bytes.to_vec().into()))
        .await
        .unwrap();
}

fn decode(bytes: &[u8]) -> FrameKind {
    let (frame, tail) = FrameKind::decode(bytes).unwrap();
    assert!(tail.is_empty());
    frame
}

async fn receive(client: &mut Client) -> FrameKind {
    tokio::time::timeout(STEP, async {
        loop {
            match client.next().await {
                Some(Ok(Message::Binary(bytes))) => return decode(&bytes),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                other => panic!("expected a credential wire reply, got {other:?}"),
            }
        }
    })
    .await
    .expect("credential wire reply deadline")
}

async fn connect(addr: SocketAddr, token: &str) -> Client {
    let tcp = TcpStream::connect(addr).await.unwrap();
    let (mut client, _) = tokio_tungstenite::client_async(bearer_request(addr, token), tcp)
        .await
        .unwrap();
    send(
        &mut client,
        FrameKind::Hello {
            client_name: "credential-revocation".into(),
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::default()
                .with_layers(LayerSet::all())
                .with_credential_approvals(true),
        },
    )
    .await;
    assert!(matches!(
        receive(&mut client).await,
        FrameKind::HelloOk { .. }
    ));
    client
}

async fn command(client: &mut Client, op: CredentialOp) -> CommandResult {
    send(
        client,
        FrameKind::Command {
            request_id: 1,
            command: Command::Credentials { op },
        },
    )
    .await;
    match receive(client).await {
        FrameKind::CommandResult {
            request_id: 1,
            result,
        } => result,
        other => panic!("unexpected credential reply: {other:?}"),
    }
}

fn next() -> CredentialOp {
    CredentialOp::Next {
        provider: PROVIDER.into(),
    }
}

fn complete(id: ApprovalId) -> CredentialOp {
    CredentialOp::Complete {
        id: id.to_string(),
        values: Some(SecretValues(vec!["revoked-delivery-canary".into()])),
    }
}

fn assert_denied(result: &CommandResult) {
    assert!(
        matches!(
            result,
            CommandResult::Error {
                code: ErrorCode::PermissionDenied,
                ..
            }
        ),
        "revoked exchange must refuse delivery: {result:?}"
    );
}

async fn pending(approver: &mut Client) -> ApprovalId {
    tokio::time::timeout(STEP, async {
        loop {
            send(
                approver,
                FrameKind::ListMetadata {
                    request_id: 2,
                    scope: Scope::Global,
                },
            )
            .await;
            let FrameKind::MetadataKeys {
                request_id: 2,
                keys,
            } = receive(approver).await
            else {
                panic!("expected approval listing");
            };
            if let Some(id) = keys.iter().find_map(|key| {
                key.strip_prefix(phux_protocol::wire::frame::APPROVAL_KEY_PREFIX)
                    .and_then(ApprovalId::parse)
            }) {
                break id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("request opens an approval")
}

async fn approve_and_claim(approver: &mut Client, provider: &mut Client) -> ApprovalId {
    let id = pending(approver).await;
    send(
        approver,
        FrameKind::SetMetadata {
            request_id: 3,
            scope: Scope::Global,
            key: id.decide_key(),
            value: APPROVAL_APPROVE.to_vec(),
        },
    )
    .await;
    tokio::time::timeout(STEP, async {
        loop {
            match command(provider, next()).await {
                CommandResult::Ok => tokio::task::yield_now().await,
                CommandResult::OkWith(CommandValue::Json(job)) => {
                    let job: serde_json::Value = serde_json::from_str(&job).unwrap();
                    assert_eq!(job["id"], id.to_string());
                    assert_eq!(job["request"]["references"][0], "op://dev/item/token");
                    break;
                }
                other => panic!("approved job claim refused: {other:?}"),
            }
        }
    })
    .await
    .expect("provider claims before revocation");
    id
}

async fn assert_revoked(client: &mut Client) {
    let ending = tokio::time::timeout(STEP, async {
        let mut frames = Vec::new();
        loop {
            match client.next().await {
                Some(Ok(Message::Binary(bytes))) => frames.push(decode(&bytes)),
                Some(Ok(Message::Close(_))) => {
                    // Observe EOF too, not merely DETACHED or Close.
                    let _ = client.flush().await;
                }
                Some(Ok(_)) => {}
                None | Some(Err(_)) => break,
            }
        }
        frames
    })
    .await
    .expect("revoked transport ends");
    assert!(
        matches!(
            ending.as_slice(),
            [
                FrameKind::Error {
                    request_id: None,
                    code: ErrorCode::PermissionDenied,
                    ..
                },
                FrameKind::Detached {
                    reason: Some(DetachReason::AuthorizationRevoked),
                    ..
                },
            ]
        ),
        "no secret result may precede or follow revocation: {ending:?}"
    );
}

async fn after_claim(revoke_provider: bool) {
    let (listener, addr, requester_token, tokens) = token_listener().await;
    let provider_token = crate::auth::mint_token(tokens.path()).unwrap();
    let approver_token = crate::auth::mint_token(tokens.path()).unwrap();
    let state = crate::runtime::apps::terminal::test_support::terminal_state();
    state.with_mut(|s| s.set_policy_engine(Arc::new(crate::policy::OwnerForAll)));
    let root = CancellationToken::new();
    let accept_state = state.clone();
    let accept_root = root.clone();
    let accept = tokio::task::spawn_local(async move {
        crate::runtime::connections::accept_loop(&listener, accept_state, accept_root, None).await
    });
    crate::runtime::revocation::spawn_revocation_watcher(&state, &root);
    let mut requester = connect(addr, &requester_token).await;
    let mut provider = connect(addr, provider_token.secret()).await;
    let mut approver = connect(addr, approver_token.secret()).await;
    assert_eq!(command(&mut provider, next()).await, CommandResult::Ok);
    send(
        &mut requester,
        FrameKind::Command {
            request_id: REQUEST_ID,
            command: Command::Credentials {
                op: CredentialOp::Request(CredentialRequest {
                    provider: PROVIDER.into(),
                    references: vec!["op://dev/item/token".into()],
                    purpose: "credential revocation regression".into(),
                }),
            },
        },
    )
    .await;
    let id = approve_and_claim(&mut approver, &mut provider).await;
    let revoked_id = if revoke_provider {
        &provider_token.id
    } else {
        "test-credential"
    };
    crate::auth::revoke_credential(tokens.path(), revoked_id).unwrap();
    if revoke_provider {
        assert_revoked(&mut provider).await;
        let FrameKind::CommandResult {
            request_id: REQUEST_ID,
            result,
        } = receive(&mut requester).await
        else {
            panic!("provider revocation must settle the waiting requester");
        };
        assert_denied(&result);
        // Refusal proves teardown ran. A new provider cannot inherit the old claim.
        assert_eq!(command(&mut approver, next()).await, CommandResult::Ok);
        assert_denied(&command(&mut approver, complete(id)).await);
    } else {
        assert_revoked(&mut requester).await;
        assert_denied(&command(&mut provider, complete(id)).await);
        assert_eq!(command(&mut provider, next()).await, CommandResult::Ok);
    }
    root.cancel();
    accept.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn revocation_after_claim_never_delivers_credentials_or_transfers_the_claim() {
    LocalSet::new()
        .run_until(async {
            for revoke_provider in [false, true] {
                tokio::time::timeout(Duration::from_secs(20), after_claim(revoke_provider))
                    .await
                    .expect("bounded credential revocation case");
            }
        })
        .await;
}
