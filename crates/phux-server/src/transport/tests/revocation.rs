use super::*;

/// ADR-0116: revoking a bearer ends its established session with the
/// workload-auth §7 goodbye, refuses reconnection, and cleans up state.
#[allow(
    clippy::too_many_lines,
    reason = "one linear end-to-end connection lifecycle is clearer than stateful test helpers"
)]
#[tokio::test(flavor = "current_thread")]
async fn bearer_revoke_now_terminates_live_sessions() {
    LocalSet::new()
        .run_until(async {
            let (listener, addr, token_hex, tokens) = token_listener().await;
            let state = crate::state::SharedState::new();
            // A transport test: an embedder engine admits the peer (ADR-0072).
            state.with_mut(|s| {
                s.set_policy_engine(std::sync::Arc::new(crate::policy::OwnerForAll));
            });
            let root_token = CancellationToken::new();
            crate::runtime::commands::seed_session_with_actor(
                &state,
                "authenticated",
                phux_config::ScrollbackLimits::default(),
                &root_token,
            )
            .expect("seed attached-session target");

            let accept_state = state.clone();
            let accept_token = root_token.clone();
            let accept_task = tokio::task::spawn_local(async move {
                crate::runtime::connections::accept_loop(
                    &listener,
                    accept_state,
                    accept_token,
                    None,
                )
                .await
            });
            crate::runtime::revocation::spawn_revocation_watcher(&state, &root_token);

            let tcp = TcpStream::connect(addr).await.unwrap();
            let (mut client, _) =
                tokio_tungstenite::client_async(bearer_request(addr, &token_hex), tcp)
                    .await
                    .expect("initial credential is admitted");
            let encode = |frame: FrameKind| {
                let mut encoded = BytesMut::new();
                frame.encode(&mut encoded);
                Message::Binary(encoded.to_vec().into())
            };
            client
                .send(encode(FrameKind::Hello {
                    client_name: "authenticated-runtime-test".to_owned(),
                    protocol_major: phux_protocol::PROTOCOL_VERSION.major,
                    protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
                    protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
                    client_caps: ClientCapabilities::default(),
                }))
                .await
                .unwrap();
            client
                .send(encode(FrameKind::Attach {
                    attach_id: 1,
                    target: AttachTarget::ByName("authenticated".to_owned()),
                    viewport: ViewportInfo::new(80, 24),
                    request_scrollback: false,
                    scrollback_limit_lines: 0,
                    role_policy: None,
                }))
                .await
                .unwrap();

            let mut got_attached = false;
            let mut got_bootstrap = false;
            tokio::time::timeout(Duration::from_secs(2), async {
                while !(got_attached && got_bootstrap) {
                    let Some(Ok(Message::Binary(data))) = client.next().await else {
                        continue;
                    };
                    match FrameKind::decode(&data).expect("decode runtime frame").0 {
                        FrameKind::Attached { .. } => got_attached = true,
                        FrameKind::BootstrapBegin { .. } => got_bootstrap = true,
                        _ => {}
                    }
                }
            })
            .await
            .expect("authenticated client attaches through handle_client");

            let client_id = state.with(|server| {
                assert_eq!(server.attached().len(), 1);
                assert!(server.idle_since().is_none());
                *server.attached().keys().next().unwrap()
            });
            let credential = state.with(|server| {
                server
                    .authenticated_credential(client_id)
                    .cloned()
                    .expect("accept loop retains credential attestation")
            });
            assert_eq!(credential.id, "test-credential");

            crate::auth::revoke_credential(tokens.path(), "test-credential").unwrap();

            let mut ending = Vec::new();
            tokio::time::timeout(Duration::from_secs(5), async {
                while let Some(message) = client.next().await {
                    match message {
                        Ok(Message::Binary(data)) => {
                            ending.push(FrameKind::decode(&data).expect("decode runtime frame").0);
                        }
                        Ok(Message::Close(_)) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
            })
            .await
            .expect("a revoked bearer's live session is closed");
            assert!(
                matches!(
                    ending.as_slice(),
                    [
                        ..,
                        FrameKind::Error {
                            request_id: None,
                            code: phux_protocol::wire::frame::ErrorCode::PermissionDenied,
                            ..
                        },
                        FrameKind::Detached {
                            reason: Some(
                                phux_protocol::wire::frame::DetachReason::AuthorizationRevoked
                            ),
                            ..
                        },
                    ]
                ),
                "the goodbye is ERROR then DETACHED, and nothing follows it: {ending:?}"
            );

            let reconnect_tcp = TcpStream::connect(addr).await.unwrap();
            let reconnect =
                tokio_tungstenite::client_async(bearer_request(addr, &token_hex), reconnect_tcp)
                    .await
                    .expect_err("revoked credential cannot reconnect");
            assert_generic_unauthorized(reconnect);

            drop(client);
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let cleaned = state.with(|server| {
                        !server.attached().contains_key(&client_id)
                            && server.peer_identity(client_id).is_none()
                            && server.authenticated_credential(client_id).is_none()
                            && server.idle_since().is_some()
                    });
                    if cleaned {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("connection close cleans attachment and credential state");

            root_token.cancel();
            accept_task
                .await
                .expect("accept-loop task")
                .expect("accept loop shuts down cleanly");
        })
        .await;
}
