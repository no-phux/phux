//! Actor conformance, continued.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "shares the parent's namespace"
)]
use super::*;

#[tokio::test(flavor = "current_thread")]
async fn delegate_agent_over_the_wire_mints_once_per_operation_and_revokes() {
    local(async {
        let fx = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        fx.state
            .with_mut(|s| s.set_server_socket_path(dir.path().join("phux.sock")));
        let session = fx
            .state
            .with(|s| s.registry().session_key(fx.alpha_session))
            .and_then(crate::org_wire::session_key)
            .unwrap();
        let operation = phux_protocol::ids::OperationId::new([7; 16]).unwrap();
        let delegate = Command::DelegateAgent {
            operation,
            session,
            ttl_secs: 600,
        };
        let human = fx.human().await;

        let CommandResult::OkWith(phux_protocol::wire::frame::CommandValue::Delegation(minted)) =
            human.command(1, delegate.clone()).await
        else {
            panic!("DELEGATE_AGENT did not answer a delegation");
        };
        assert_eq!(minted.session, session);
        assert!(std::path::Path::new(&minted.socket_path).exists());
        let lifetime_ms = minted
            .expires_at_ms
            .saturating_sub(u64::try_from(Utc::now().timestamp_millis()).unwrap());
        assert!(lifetime_ms <= 600_000, "{lifetime_ms}");
        assert_eq!(
            human.command(2, delegate).await,
            CommandResult::OkWith(phux_protocol::wire::frame::CommandValue::Delegation(
                minted.clone()
            )),
            "a retried operation answers the credential it minted"
        );

        // An agent cannot mint or withdraw delegations, its own included.
        let (agent, _) = fx.delegated_agent(&human).await;
        for (request_id, command) in [
            (
                1,
                Command::DelegateAgent {
                    operation: phux_protocol::ids::OperationId::new([8; 16]).unwrap(),
                    session,
                    ttl_secs: 0,
                },
            ),
            (
                2,
                Command::RevokeDelegation {
                    operation: phux_protocol::ids::OperationId::new([9; 16]).unwrap(),
                    credential_id: minted.credential_id.clone(),
                },
            ),
        ] {
            let refused = agent.command(request_id, command).await;
            assert!(
                matches!(
                    refused,
                    CommandResult::Error {
                        code: ErrorCode::PermissionDenied,
                        ..
                    }
                ),
                "{refused:?}"
            );
        }

        let revoke = Command::RevokeDelegation {
            operation: phux_protocol::ids::OperationId::new([10; 16]).unwrap(),
            credential_id: minted.credential_id.clone(),
        };
        assert_eq!(human.command(3, revoke.clone()).await, CommandResult::Ok);
        let deadline = Instant::now() + PATIENCE;
        while std::path::Path::new(&minted.socket_path).exists() {
            assert!(
                Instant::now() < deadline,
                "the socket file was never removed"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            human.command(4, revoke).await,
            CommandResult::Ok,
            "withdrawal is idempotent"
        );
        agent.finish().await;
        human.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn no_agent_takes_a_paired_devices_lease_and_the_devices_takeover_beats_them() {
    local(async {
        let fx = Fixture::new();
        let phone = fx.phone().await;
        phone.attach().await;
        assert_eq!(
            phone
                .command(1, acquire(&fx.alpha, InputMode::Cooperative))
                .await,
            CommandResult::Ok
        );

        let owner = fx.human().await;
        let (delegated, _) = fx.delegated_agent(&owner).await;
        let automation = fx.automation().await;
        for agent in [&delegated, &automation] {
            agent.attach().await;
            let seized = agent.command(1, acquire(&fx.alpha, InputMode::Seize)).await;
            assert!(
                matches!(
                    seized,
                    CommandResult::Error {
                        code: ErrorCode::InputLeaseHeld,
                        ..
                    }
                ),
                "{seized:?}"
            );
            assert_eq!(fx.lease_holder(), Some(phone.client));
        }

        assert_eq!(
            phone
                .command(
                    2,
                    Command::ReleaseInput {
                        terminal_id: fx.alpha.clone(),
                    },
                )
                .await,
            CommandResult::Ok
        );
        assert_eq!(
            delegated
                .command(2, acquire(&fx.alpha, InputMode::Cooperative))
                .await,
            CommandResult::Ok,
            "a free lease is the agent's to take"
        );
        assert_eq!(
            phone.command(3, acquire(&fx.alpha, InputMode::Seize)).await,
            CommandResult::Ok,
            "the device's deliberate takeover beats the agent"
        );
        assert_eq!(fx.lease_holder(), Some(phone.client));
        for connection in [delegated, automation, owner, phone] {
            connection.finish().await;
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn deleting_a_session_revokes_its_delegated_credentials_at_once() {
    local(async {
        let fx = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        fx.state
            .with_mut(|s| s.set_server_socket_path(dir.path().join("phux.sock")));
        let human = fx.human().await;
        let beta = fx.state.with_mut(|s| s.seed_empty_session("beta")).unwrap();
        let on_beta = delegate_agent(
            &fx.state,
            human.client,
            beta,
            DelegationRequest::default(),
            None,
            &fx.root,
        )
        .unwrap();
        let on_alpha = delegate_agent(
            &fx.state,
            human.client,
            fx.alpha_session,
            DelegationRequest::default(),
            None,
            &fx.root,
        )
        .unwrap();
        let agent = fx.connect(stamped_identity(owner_peer(), &on_beta.credential));
        agent.hello().await;
        let bystander = fx.connect(stamped_identity(owner_peer(), &on_alpha.credential));
        bystander.hello().await;

        let deleted = fx
            .state
            .with_mut(|s| s.delete_emptied_sessions(&[beta], &phux_core::Actor::owner()));
        assert_eq!(deleted, vec!["beta".to_owned()]);

        let now = Utc::now();
        assert!(
            fx.state
                .with(|s| s.delegation(&on_beta.credential.id, now).is_none()),
            "the credential is gone with its session"
        );
        assert!(fx.state.with(|s| s.connection_revoked(agent.client)));
        agent
            .wait_for("DETACHED { AUTHORIZATION_REVOKED }", |frame| {
                matches!(
                    frame,
                    FrameKind::Detached {
                        reason: Some(
                            phux_protocol::wire::frame::DetachReason::AuthorizationRevoked
                        ),
                        ..
                    }
                )
            })
            .await;
        let deadline = Instant::now() + PATIENCE;
        while on_beta.socket_path.exists() {
            assert!(
                Instant::now() < deadline,
                "the socket file was never removed"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        assert!(
            fx.state
                .with(|s| s.delegation(&on_alpha.credential.id, now).is_some()),
            "another session's credential stands"
        );
        assert!(!fx.state.with(|s| s.connection_revoked(bystander.client)));
        assert!(on_alpha.socket_path.exists());
        for connection in [agent, bystander, human] {
            connection.finish().await;
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_minted_credentials_socket_admits_connections_as_it_until_withdrawn() {
    local(async {
        let fx = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        fx.state
            .with_mut(|s| s.set_server_socket_path(dir.path().join("phux.sock")));
        let human = fx.human().await;

        let delegated = delegate_agent(
            &fx.state,
            human.client,
            fx.alpha_session,
            DelegationRequest::default(),
            None,
            &fx.root,
        )
        .unwrap();
        assert!(delegated.socket_path.starts_with(dir.path()));
        let mode = std::fs::metadata(&delegated.socket_path).unwrap();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o777,
            0o600
        );

        let mut stream = tokio::net::UnixStream::connect(&delegated.socket_path)
            .await
            .unwrap();
        stream
            .write_all(&encode(&hello("delegated-agent")))
            .await
            .unwrap();
        let principal = delegated.credential.principal();
        let deadline = Instant::now() + PATIENCE;
        let agent = loop {
            let found = fx.state.with(|s| {
                s.watched_connections()
                    .into_iter()
                    .find(|watched| watched.grant.principal() == &principal)
                    .map(|watched| watched.client)
            });
            if let Some(client) = found {
                break client;
            }
            assert!(Instant::now() < deadline, "the agent never got its grant");
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(
            fx.state.with(|s| s.actor_for(agent)).on_behalf_of,
            Some(PrincipalId::Owner)
        );

        assert!(revoke_delegated_agent(&fx.state, &delegated.credential.id));
        assert!(fx.state.with(|s| s.connection_revoked(agent)));
        let deadline = Instant::now() + PATIENCE;
        while delegated.socket_path.exists() {
            assert!(
                Instant::now() < deadline,
                "the socket file was never removed"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            tokio::net::UnixStream::connect(&delegated.socket_path)
                .await
                .is_err()
        );
        assert!(!revoke_delegated_agent(&fx.state, &delegated.credential.id));
        drop(stream);
        human.finish().await;
    })
    .await;
}
