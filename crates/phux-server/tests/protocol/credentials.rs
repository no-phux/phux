//! Credential exchanges through production client admission and dispatch, never handler-only.

mod common;
mod privacy;
mod provider_limits;
mod remote;

use std::sync::Arc;
use std::time::Duration;

use phux_protocol::credentials::{CredentialOp, SecretValues};
use phux_protocol::ids::OperationId;
use phux_protocol::policy::PeerIdentity;
use phux_protocol::scope::TerminalScopeSet;
use phux_protocol::wire::frame::{
    AgentEvent, ApprovalOutcome, Command, CommandResult, CommandValue, ErrorCode, FrameKind, Scope,
    StateScope,
};
use phux_server::auth::AuthenticatedCredential;
use phux_server::policy::{ConnectionGrant, GrantFuture, PolicyEngine};
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, command, recv_typed_before, recv_until, run_local, send_frame,
    wait_for_socket,
};

use common::*;

#[test]
fn owner_uds_still_requires_exact_approval_and_delivers_values_once() {
    run_local(async {
        let server = Server::start(300, None);
        let mut requester = server.connect("credential-requester").await;
        let mut provider = server.connect("credential-provider").await;
        let mut approver = server.connect("credential-approver").await;
        let mut observer = server.observer().await;
        assert_eq!(next(&mut provider, PROVIDER).await, CommandResult::Ok);
        begin(&mut requester).await;
        let id = opened(&mut observer).await;
        let record: serde_json::Value =
            serde_json::from_slice(&metadata(&mut approver, &id.record_key()).await.unwrap())
                .unwrap();
        assert_eq!(
            record["credential"]["request"],
            serde_json::to_value(request()).unwrap()
        );
        assert_eq!(
            record["credential"]["provider"]["client_name"],
            "credential-provider"
        );
        assert_eq!(record["requester"]["client_name"], "credential-requester");
        assert_ne!(
            record["credential"]["provider"]["client"],
            record["requester"]["client"]
        );
        assert_eq!(
            next(&mut provider, PROVIDER).await,
            CommandResult::Ok,
            "no provider read before approval"
        );
        denied(&complete(&mut provider, id, Some(vec!["premature".into()])).await);
        // A second command on the requesting connection is not blocked by the held exchange.
        barrier(&mut requester).await;
        assert!(
            recv_typed_before(
                &mut requester,
                tokio::time::Instant::now() + Duration::from_millis(30)
            )
            .await
            .is_none()
        );
        decide(&mut approver, id, true).await;
        assert_eq!(ended(&mut observer, id).await, ApprovalOutcome::Approved);
        let claimed = claim(&mut provider).await;
        assert_eq!(claimed["id"], id.to_string());
        assert_eq!(claimed["request"], record["credential"]["request"]);
        assert_eq!(claimed["requester"]["client_name"], "credential-requester");
        assert_eq!(
            next(&mut provider, PROVIDER).await,
            CommandResult::Ok,
            "claim is one-use"
        );
        let values = vec!["one-use-credential-canary".to_owned()];
        assert_eq!(
            complete(&mut provider, id, Some(values.clone())).await,
            CommandResult::Ok
        );
        let result = answer(&mut requester).await;
        assert_eq!(
            result,
            CommandResult::OkWith(CommandValue::CredentialSecrets(SecretValues(values)))
        );
        assert!(!format!("{result:?}").contains("one-use-credential-canary"));
        denied(&complete(&mut provider, id, Some(vec!["replay".into()])).await);
        assert!(metadata(&mut approver, &id.record_key()).await.is_none());
        assert!(metadata(&mut approver, &id.decide_key()).await.is_none());
        // An identical subsequent command is a new approval, never a cached secret answer.
        begin(&mut requester).await;
        let fresh = opened(&mut observer).await;
        assert_ne!(fresh, id);
        assert_eq!(next(&mut provider, PROVIDER).await, CommandResult::Ok);
        decide(&mut approver, fresh, false).await;
        denied(&answer(&mut requester).await);
        server.stop().await;
    });
}

#[test]
fn denied_or_offline_requests_never_reach_the_provider() {
    run_local(async {
        let server = Server::start(300, None);
        let mut requester = server.connect("requester").await;
        let mut approver = server.connect("approver").await;
        let mut observer = server.observer().await;
        begin(&mut requester).await;
        assert!(matches!(
            answer(&mut requester).await,
            CommandResult::Error { .. }
        ));
        let mut provider = server.connect("provider").await;
        assert_eq!(next(&mut provider, PROVIDER).await, CommandResult::Ok);
        begin(&mut requester).await;
        let id = opened(&mut observer).await;
        decide(&mut approver, id, false).await;
        denied(&answer(&mut requester).await);
        assert_eq!(ended(&mut observer, id).await, ApprovalOutcome::Denied);
        assert_eq!(next(&mut provider, PROVIDER).await, CommandResult::Ok);
        denied(&complete(&mut provider, id, Some(vec!["unapproved".into()])).await);
        assert!(metadata(&mut approver, &id.record_key()).await.is_none());
        server.stop().await;
    });
}

#[test]
fn legacy_clients_cannot_approve_without_reviewing_credential_details_but_can_deny() {
    run_local(async {
        let server = Server::start(300, None);
        let mut requester = server.connect("requester").await;
        let mut provider = server.connect("provider").await;
        let mut legacy = server
            .connect_with_credential_details("legacy-client", false)
            .await;
        let mut observer = server.observer().await;
        assert_eq!(next(&mut provider, PROVIDER).await, CommandResult::Ok);
        begin(&mut requester).await;
        let id = opened(&mut observer).await;
        send_frame(
            &mut legacy,
            &FrameKind::SetMetadata {
                request_id: 7,
                scope: Scope::Global,
                key: id.decide_key(),
                value: b"approve".to_vec(),
            },
        )
        .await;
        let code = recv_until(&mut legacy, |_, frame| match frame {
            FrameKind::Error {
                request_id: Some(7),
                code,
                ..
            } => Some(code),
            _ => None,
        })
        .await;
        assert_eq!(code, ErrorCode::Unsupported);
        assert_eq!(next(&mut provider, PROVIDER).await, CommandResult::Ok);
        assert!(metadata(&mut legacy, &id.record_key()).await.is_some());
        decide(&mut legacy, id, false).await;
        denied(&answer(&mut requester).await);
        assert_eq!(ended(&mut observer, id).await, ApprovalOutcome::Denied);
        server.stop().await;
    });
}

#[test]
fn provider_name_and_completion_are_bound_to_the_original_connection() {
    run_local(async {
        let server = Server::start(300, None);
        let mut requester = server.connect("requester").await;
        let mut provider = server.connect("provider").await;
        let mut impostor = server.connect("provider").await; // HELLO names are not authority.
        let mut approver = server.connect("approver").await;
        let mut observer = server.observer().await;
        assert_eq!(next(&mut provider, PROVIDER).await, CommandResult::Ok);
        assert!(matches!(
            next(&mut impostor, PROVIDER).await,
            CommandResult::Error { .. }
        ));
        assert_eq!(next(&mut impostor, "other-device").await, CommandResult::Ok);
        begin(&mut requester).await;
        let id = opened(&mut observer).await;
        decide(&mut approver, id, true).await;
        let _ = claim(&mut provider).await;
        assert_eq!(next(&mut impostor, "other-device").await, CommandResult::Ok);
        denied(&complete(&mut impostor, id, Some(vec!["impostor".into()])).await);
        assert_eq!(
            complete(&mut provider, id, Some(vec!["legitimate".into()])).await,
            CommandResult::Ok
        );
        assert_eq!(
            answer(&mut requester).await,
            CommandResult::OkWith(CommandValue::CredentialSecrets(SecretValues(vec![
                "legitimate".into()
            ])))
        );
        server.stop().await;
    });
}

#[test]
fn wrong_counts_are_rejected_without_spending_the_claim_and_provider_failure_is_terminal() {
    run_local(async {
        let server = Server::start(300, None);
        let mut requester = server.connect("requester").await;
        let mut provider = server.connect("provider").await;
        let mut approver = server.connect("approver").await;
        let mut observer = server.observer().await;
        assert_eq!(next(&mut provider, PROVIDER).await, CommandResult::Ok);
        begin(&mut requester).await;
        let id = opened(&mut observer).await;
        decide(&mut approver, id, true).await;
        let _ = claim(&mut provider).await;
        let result = complete(
            &mut provider,
            id,
            Some(vec!["first-canary".into(), "second-canary".into()]),
        )
        .await;
        assert!(matches!(
            result,
            CommandResult::Error {
                code: ErrorCode::InvalidCommand,
                ..
            }
        ));
        assert!(!format!("{result:?}").contains("canary"));
        assert_eq!(complete(&mut provider, id, None).await, CommandResult::Ok);
        denied(&answer(&mut requester).await);
        denied(&complete(&mut provider, id, Some(vec!["late".into()])).await);
        server.stop().await;
    });
}

#[test]
fn disconnect_withdraws_pending_approvals_and_spends_claimed_exchanges() {
    run_local(async {
        let server = Server::start(300, None);
        let mut approver = server.connect("approver").await;
        let mut observer = server.observer().await;
        for approved in [false, true] {
            let mut provider = server.connect("provider").await;
            let mut requester = server.connect("requester").await;
            // Previous provider disconnect is eventually observed before its label is reusable.
            register(&mut provider).await;
            begin(&mut requester).await;
            let id = opened(&mut observer).await;
            if approved {
                decide(&mut approver, id, true).await;
                let _ = claim(&mut provider).await;
            }
            drop(provider);
            denied(&answer(&mut requester).await);
            assert_eq!(
                ended(&mut observer, id).await,
                if approved {
                    ApprovalOutcome::Approved
                } else {
                    ApprovalOutcome::Withdrawn
                }
            );
            assert!(metadata(&mut approver, &id.record_key()).await.is_none());
        }
        for approved in [false, true] {
            let mut provider = server.connect("provider").await;
            let mut requester = server.connect("requester").await;
            register(&mut provider).await;
            begin(&mut requester).await;
            let id = opened(&mut observer).await;
            if approved {
                decide(&mut approver, id, true).await;
                let _ = claim(&mut provider).await;
            }
            disconnect(requester).await;
            if approved {
                denied(&complete(&mut provider, id, Some(vec!["too-late".into()])).await);
            } else {
                assert_eq!(ended(&mut observer, id).await, ApprovalOutcome::Withdrawn);
            }
            assert!(metadata(&mut approver, &id.record_key()).await.is_none());
            assert_eq!(next(&mut provider, PROVIDER).await, CommandResult::Ok);
        }
        server.stop().await;
    });
}

#[test]
fn approval_ttl_expires_before_approval_and_after_claim() {
    run_local(async {
        let server = Server::start(1, None);
        let mut requester = server.connect("requester").await;
        let mut provider = server.connect("provider").await;
        let mut approver = server.connect("approver").await;
        let mut observer = server.observer().await;
        assert_eq!(next(&mut provider, PROVIDER).await, CommandResult::Ok);
        for approved in [false, true] {
            begin(&mut requester).await;
            let id = opened(&mut observer).await;
            if approved {
                decide(&mut approver, id, true).await;
                let _ = claim(&mut provider).await;
            }
            denied(&answer(&mut requester).await);
            assert_eq!(
                ended(&mut observer, id).await,
                if approved {
                    ApprovalOutcome::Approved
                } else {
                    ApprovalOutcome::Expired
                }
            );
            assert!(metadata(&mut approver, &id.record_key()).await.is_none());
            assert_eq!(next(&mut provider, PROVIDER).await, CommandResult::Ok);
            denied(&complete(&mut provider, id, Some(vec!["expired".into()])).await);
        }
        server.stop().await;
    });
}

#[test]
fn full_exchange_is_capped_at_120_seconds_even_after_approval() {
    run_local(async {
        let server = Server::start(300, None);
        let mut requester = server.connect("requester").await;
        let mut provider = server.connect("provider").await;
        let mut approver = server.connect("approver").await;
        let mut observer = server.observer().await;
        assert_eq!(next(&mut provider, PROVIDER).await, CommandResult::Ok);
        begin(&mut requester).await;
        let id = opened(&mut observer).await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(119)).await;
        tokio::time::resume();
        decide(&mut approver, id, true).await;
        let _ = claim(&mut provider).await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::time::resume();
        denied(&answer(&mut requester).await);
        denied(&complete(&mut provider, id, Some(vec!["expired".into()])).await);
        server.stop().await;
    });
}

/// Use the public HELLO policy seam to exercise the production command guard.
#[derive(Debug)]
struct Scoped(TerminalScopeSet);

impl PolicyEngine for Scoped {
    fn authorize_hello<'a>(
        &'a self,
        _peer: &'a PeerIdentity,
        _credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a> {
        Box::pin(async { Ok(ConnectionGrant::scoped(self.0.clone(), None).unwrap()) })
    }
}

#[test]
fn credentials_require_both_bind_and_input_at_global_for_every_operation() {
    run_local(async {
        for scope in ["bind@global", "input@global", "bind,input@terminal:1"] {
            let policy = Scoped(TerminalScopeSet::parse_all(&[scope]).unwrap());
            let server = Server::start(300, Some(Arc::new(policy)));
            let mut peer = server.connect("scoped-agent").await;
            for operation in [
                CredentialOp::Request(request()),
                CredentialOp::Next {
                    provider: PROVIDER.to_owned(),
                },
                CredentialOp::Complete {
                    id: "01010101010101010101010101010101".to_owned(),
                    values: None,
                },
            ] {
                denied(&op(&mut peer, 9, operation).await);
            }
            server.stop().await;
        }
    });
}

#[test]
fn a_real_delegated_agent_socket_cannot_request_or_serve_credentials() {
    run_local(async {
        let server = Server::start(300, None);
        let mut owner = server.connect("owner").await;
        let snapshot = command(
            &mut owner,
            1,
            Command::GetState {
                scope: StateScope::Server,
            },
        )
        .await;
        let CommandResult::OkWith(CommandValue::State(snapshot)) = snapshot else {
            panic!("missing server snapshot");
        };
        let delegated = command(
            &mut owner,
            2,
            Command::DelegateAgent {
                operation: OperationId::new([1; 16]).unwrap(),
                session: snapshot.sessions[0].key.unwrap(),
                ttl_secs: 60,
            },
        )
        .await;
        let CommandResult::OkWith(CommandValue::Delegation(delegated)) = delegated else {
            panic!("missing delegated agent credential");
        };
        let mut agent = wait_for_socket(
            std::path::Path::new(&delegated.socket_path),
            SOCKET_CONNECT_DEADLINE,
        )
        .await;
        for operation in [
            CredentialOp::Request(request()),
            CredentialOp::Next {
                provider: PROVIDER.to_owned(),
            },
            CredentialOp::Complete {
                id: "01010101010101010101010101010101".to_owned(),
                values: None,
            },
        ] {
            denied(&op(&mut agent, 9, operation).await);
        }
        assert_eq!(
            command(
                &mut owner,
                3,
                Command::RevokeDelegation {
                    operation: OperationId::new([2; 16]).unwrap(),
                    credential_id: delegated.credential_id,
                },
            )
            .await,
            CommandResult::Ok
        );
        server.stop().await;
    });
}

#[test]
fn malformed_secret_values_close_the_provider_without_echoing_payloads() {
    run_local(async {
        for value in [
            "invalid-value-canary\0suffix".to_owned(),
            "x".repeat(phux_protocol::credentials::MAX_SECRET_BYTES + 1),
        ] {
            let server = Server::start(300, None);
            let mut requester = server.connect("requester").await;
            let mut provider = server.connect("provider").await;
            let mut approver = server.connect("approver").await;
            let mut observer = server.observer().await;
            assert_eq!(next(&mut provider, PROVIDER).await, CommandResult::Ok);
            begin(&mut requester).await;
            let id = opened(&mut observer).await;
            decide(&mut approver, id, true).await;
            let _ = claim(&mut provider).await;
            let frame = FrameKind::Command {
                request_id: 8,
                command: Command::Credentials {
                    op: CredentialOp::Complete {
                        id: id.to_string(),
                        values: Some(SecretValues(vec![value])),
                    },
                },
            };
            assert!(!format!("{frame:?}").contains("invalid-value-canary"));
            send_frame(&mut provider, &frame).await;
            let refusal = recv_until(&mut provider, |_, frame| match frame {
                FrameKind::Error { code, message, .. } => Some((code, message)),
                _ => None,
            })
            .await;
            assert_eq!(refusal.0, ErrorCode::MalformedMessage);
            assert!(!refusal.1.contains("invalid-value-canary"));
            denied(&answer(&mut requester).await);
            server.stop().await;
        }
    });
}
