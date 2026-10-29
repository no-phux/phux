//! Each way a grant is withdrawn, and what the withdrawal releases.

use std::time::Duration;

use phux_protocol::wire::frame::{
    AgentEvent, ControlAction, DetachReason, ErrorCode, FrameKind, Scope,
};
use tokio::sync::mpsc;

use super::{
    Fixture, KEY, PATIENCE, TEST_GRACE, acquire, local, settle, strings, subscribe_metadata,
    wait_until, write_owner_only,
};
use crate::runtime::revocation::spawn_revocation_watcher;
use crate::runtime::test_support::{hello, workload_identity};
use crate::state::{Outbound, ServerState};
use crate::workload::WorkloadRegistry;

#[tokio::test(flavor = "current_thread")]
async fn revoking_a_key_terminates_its_live_connections_with_authorization_revoked() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let attached = fx.workload().await;
        attached.attach(1).await;
        let idle = fx.workload().await;

        fx.revoke_key();
        fx.sweep();

        for connection in [&attached, &idle] {
            connection
                .assert_ended_with(DetachReason::AuthorizationRevoked)
                .await;
            assert!(fx.state.with(|s| s.connection_revoked(connection.client)));
        }
        assert!(
            fx.state
                .with(|s| !s.attached().contains_key(&attached.client)),
            "the attachment is released"
        );
        attached.finish().await;
        idle.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn expiry_terminates_with_authorization_expired_and_releases_leases() {
    local(async {
        let fx = Fixture::expiring(chrono::Duration::milliseconds(750));
        spawn_revocation_watcher(&fx.state, &fx.root);
        let agent = fx.workload().await;
        agent.attach(1).await;
        agent.command_ok(2, acquire(&fx.alpha)).await;
        assert_eq!(fx.lease_holder(), Some(agent.client));

        // The watcher wakes at the expiry itself: nothing else happens here.
        agent
            .assert_ended_with(DetachReason::AuthorizationExpired)
            .await;
        assert_eq!(
            fx.lease_holder(),
            None,
            "the expired grant's lease is released"
        );
        agent.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ceiling_reduction_that_no_longer_contains_a_minted_clause_revokes() {
    local(async {
        let fx = Fixture::paired(&["observe,bind,input@global"]);
        let agent = fx.workload().await;
        let generation = |client| {
            fx.state
                .with(|s| s.connection_grant(client).unwrap().registry_generation)
        };
        let minted = generation(agent.client);

        // A wider ceiling still contains every minted clause: the grant
        // stays as minted and adopts the new generation.
        fx.rewrite_ceiling(&["observe,bind,input,signal@global"]);
        fx.sweep();
        settle().await;
        assert!(
            agent.is_open(),
            "a wider ceiling ended the connection: {:?}",
            agent.frames()
        );
        assert!(
            generation(agent.client) > minted,
            "the grant adopts the new generation"
        );

        // Losing `bind` and `input` no longer contains the minted clause.
        fx.rewrite_ceiling(&["observe@global"]);
        fx.sweep();
        agent
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;
        agent.finish().await;
    })
    .await;
}

/// ADR-0128: adding a hold to a live credential is a narrowing. The open
/// connection minted the verb un-held, so it is revoked, and its reconnect
/// gets the hold; removing a hold is a widening and never revokes.
#[tokio::test(flavor = "current_thread")]
async fn adding_a_hold_to_a_live_credential_revokes_its_connections() {
    local(async {
        let fx = Fixture::paired(&["observe,bind,signal@global"]);
        let agent = fx.workload().await;
        fx.rewrite_ceiling(&["observe,bind,signal,input@global"]);
        fx.sweep();
        settle().await;
        assert!(agent.is_open(), "a wider ceiling ended the connection");

        fx.rewrite_ceiling(&["observe,bind,?signal,input@global"]);
        fx.sweep();
        agent
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;
        agent.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_reload_revokes_every_workload_session_until_a_valid_generation_loads() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let first = fx.workload().await;
        let second = fx.workload().await;

        write_owner_only(&fx.registry, b"{ this is not a registry");
        fx.sweep();
        settle().await;
        assert!(
            first.is_open() && second.is_open(),
            "a broken registry is no verdict inside its grace window"
        );

        // The last known-good generation is not preserved: HELLO is refused.
        let refused = fx.connect(workload_identity(&fx.credential_id, &["*@global"]), None);
        refused.send(&hello("revocation-conformance"));
        refused
            .wait_for("the HELLO refusal", |frame| {
                matches!(
                    frame,
                    FrameKind::Error {
                        code: ErrorCode::PermissionDenied,
                        ..
                    }
                )
            })
            .await;

        // The same broken state past the grace window applies the empty
        // snapshot: every workload connection it stood behind ends.
        tokio::time::sleep(TEST_GRACE + Duration::from_millis(50)).await;
        fx.sweep();
        for connection in [&first, &second] {
            connection
                .assert_ended_with(DetachReason::AuthorizationRevoked)
                .await;
        }

        // A valid generation admits the key again.
        std::fs::remove_file(&fx.registry).unwrap();
        WorkloadRegistry::register(&fx.registry, KEY, strings(&["*@global"]), None).unwrap();
        let admitted = fx.workload().await;
        assert!(admitted.is_open());

        for connection in [first, second, refused, admitted] {
            connection.finish().await;
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn revocation_releases_input_leases_and_subscriptions_and_journals_them() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let observer = fx.state.with_mut(ServerState::new_client_id);
        let (observer_tx, mut observer_rx) = mpsc::channel(256);
        fx.state
            .with_mut(|s| s.subscribe_events(observer, None, observer_tx));

        let agent = fx.workload().await;
        agent.attach(1).await;
        agent.command_ok(2, acquire(&fx.alpha)).await;
        agent.send(&FrameKind::SubscribeEvents {
            terminal: None,
            after_seq: None,
        });
        agent.send(&subscribe_metadata("conformance.probe"));
        wait_until("the subscriptions are registered", || {
            fx.state.with(|s| {
                s.has_event_subscription(agent.client)
                    && s.has_metadata_subscription(
                        agent.client,
                        &Scope::Global,
                        "conformance.probe",
                    )
            })
        })
        .await;

        fx.revoke_key();
        fx.sweep();
        agent
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;

        assert_eq!(fx.lease_holder(), None, "the input lease is released");
        fx.state.with(|s| {
            assert!(
                !s.has_event_subscription(agent.client),
                "event subscription dropped"
            );
            assert!(
                !s.has_metadata_subscription(agent.client, &Scope::Global, "conformance.probe"),
                "metadata subscription dropped"
            );
            assert!(
                !s.attached().contains_key(&agent.client),
                "attachment dropped"
            );
        });

        // The pane's actor journals the server's release, with no actor.
        let released = tokio::time::timeout(PATIENCE, async {
            loop {
                match observer_rx.recv().await {
                    Some(Outbound::Frame(FrameKind::Event {
                        terminal,
                        event:
                            AgentEvent::TerminalControl {
                                action: ControlAction::Released,
                                actor,
                                ..
                            },
                        ..
                    })) => return (terminal, actor),
                    Some(_) => {}
                    None => panic!("the observer's mailbox closed"),
                }
            }
        })
        .await
        .expect("the release is journaled");
        assert_eq!(released, (Some(fx.alpha.clone()), None));
        agent.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn revocation_does_not_kill_the_workloads_terminals() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let agent = fx.workload().await;
        agent.attach(1).await;

        fx.revoke_key();
        fx.sweep();
        agent
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;

        fx.state.with(|s| {
            assert!(
                s.resource_handle(fx.alpha_core).is_some(),
                "the Terminal survives"
            );
            assert!(
                s.find_session_by_name("alpha").is_some(),
                "its session survives"
            );
        });
        let owner = fx.owner().await;
        owner.attach(1).await;
        assert!(owner.is_open());
        agent.finish().await;
        owner.finish().await;
    })
    .await;
}
