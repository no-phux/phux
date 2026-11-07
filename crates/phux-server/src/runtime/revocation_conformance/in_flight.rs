//! Revocation against work already in flight: queued frames, a
//! connection before HELLO, a replay, an input receipt, and hub relays.

use phux_protocol::ids::{InputOperationId, ResourceId as WireResourceId, SatelliteHost};
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::wire::frame::{AgentEvent, Command, DetachReason, FrameKind, Scope};
use tokio::sync::mpsc;

use super::{Fixture, acquire, local, settle, subscribe_metadata};
use crate::hub::relay::{HubRelays, RelayHandle, RelayRequest, Unsubscribe};
use crate::policy::Revocation;
use crate::runtime::apps::terminal::input_lane::spawn_input_lane;
use crate::runtime::revocation::revoke_connection;
use crate::runtime::test_support::{hello, workload_identity};
use crate::state::EventRecord;

#[tokio::test(flavor = "current_thread")]
async fn queued_client_frames_after_revocation_are_not_processed() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let agent = fx.workload().await;
        agent.attach(1).await;

        // Frames the loop has not read yet, then the revocation before it
        // runs again: step 4 closes without processing them.
        agent.send(&FrameKind::Command {
            request_id: 2,
            command: acquire(&fx.alpha),
        });
        agent.send(&subscribe_metadata("queued.probe"));
        revoke_connection(&fx.state, agent.client, Revocation::Revoked);

        agent
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;
        assert_eq!(
            fx.lease_holder(),
            None,
            "the queued ACQUIRE_INPUT never ran"
        );
        assert!(
            !fx.state.with(|s| s.has_metadata_subscription(
                agent.client,
                &Scope::Global,
                "queued.probe"
            )),
            "the queued SUBSCRIBE_METADATA never ran"
        );
        agent.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_revoked_grant_admits_no_frame_even_before_the_connection_closes() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let agent = fx.workload().await;

        // Step 1 alone: the grant is revoked, the token not yet cancelled.
        fx.state
            .with_mut(|s| s.mark_connection_revoked(agent.client, Revocation::Revoked));
        agent.send(&FrameKind::Command {
            request_id: 2,
            command: acquire(&fx.alpha),
        });
        agent.send(&subscribe_metadata("guarded.probe"));
        agent.send(&FrameKind::Ping { nonce: 5 });
        settle().await;

        assert_eq!(fx.lease_holder(), None, "the guard refused ACQUIRE_INPUT");
        assert!(
            !fx.state.with(|s| s.has_metadata_subscription(
                agent.client,
                &Scope::Global,
                "guarded.probe"
            )),
            "the guard refused SUBSCRIBE_METADATA"
        );
        agent
            .assert_goodbye(DetachReason::AuthorizationRevoked)
            .await;
        agent.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_connection_revoked_before_hello_closes_without_a_frame() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let pending = fx.connect(workload_identity(&fx.credential_id, &["*@global"]), None);
        settle().await;

        revoke_connection(&fx.state, pending.client, Revocation::Revoked);
        pending.send(&hello("revocation-conformance"));
        pending.wait_closed().await;
        settle().await;
        assert!(
            pending.frames().is_empty(),
            "no HELLO_OK and no DETACHED before a handshake: {:?}",
            pending.frames()
        );
        pending.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_humans_session_survives_the_agents_revocation() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let human = fx.owner().await;
        human.attach(1).await;
        let agent = fx.workload().await;
        agent.attach(1).await;
        agent.command_ok(2, acquire(&fx.alpha)).await;

        fx.revoke_key();
        fx.sweep();
        agent
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;

        // The human keeps its attachment and can take the wheel the agent
        // lost; nothing about its connection changed.
        human.command_ok(3, acquire(&fx.alpha)).await;
        human.send(&FrameKind::Ping { nonce: 11 });
        human
            .wait_for("PONG", |frame| {
                matches!(frame, FrameKind::Pong { nonce: 11 })
            })
            .await;
        assert!(human.is_open(), "{:?}", human.frames());
        assert_eq!(fx.lease_holder(), Some(human.client));
        fx.state.with(|s| {
            assert!(s.attached().contains_key(&human.client));
            assert!(!s.connection_revoked(human.client));
        });
        agent.finish().await;
        human.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn revoking_mid_subscribe_stops_an_in_flight_replay() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        fx.state.with_mut(|s| {
            for _ in 0..512 {
                let _ =
                    s.record_and_fanout(EventRecord::new(Some(fx.alpha.clone()), AgentEvent::Bell));
            }
        });
        let agent = fx.workload().await;
        agent.send(&FrameKind::SubscribeEvents {
            terminal: None,
            after_seq: Some(0),
        });
        agent
            .wait_for("a replayed EVENT", |frame| {
                matches!(frame, FrameKind::Event { .. })
            })
            .await;

        fx.revoke_key();
        fx.sweep();
        agent
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;
        assert!(
            !fx.state.with(|s| s.has_event_subscription(agent.client)),
            "the replaying subscription is dropped"
        );
        agent.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn revoking_mid_input_drops_the_pending_apply_input_receipt() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let lane = spawn_input_lane(fx.state.clone()).unwrap();
        let agent = fx.workload_with_lane(Some(lane.handle())).await;
        agent.attach(1).await;

        agent.send(&FrameKind::Command {
            request_id: 4,
            command: Command::ApplyInput {
                operation_id: InputOperationId::new([7; 16]).unwrap(),
                terminal_id: fx.alpha.clone(),
                events: vec![InputEvent::Focus(FocusEvent::Gained)],
            },
        });
        settle().await;
        revoke_connection(&fx.state, agent.client, Revocation::Revoked);

        agent
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;
        agent.finish().await;
        drop(lane);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn revoking_a_hub_consumer_tears_down_its_relay_state() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let host = SatelliteHost::new("sat");
        let (relay, mut mailbox) = RelayHandle::new(host.clone());
        let relays = HubRelays::default();
        relays.insert(relay);
        fx.state.with_mut(|s| s.set_hub_relays(relays));

        let agent = fx.workload().await;
        let (lease_tx, _lease_rx) = mpsc::channel(1);
        let proxies_cancel = fx.state.with_mut(|s| {
            let _ = s.set_satellite_lease(host.clone(), 7, agent.client, lease_tx);
            s.client_connection_cancellation(agent.client).unwrap()
        });

        revoke_connection(&fx.state, agent.client, Revocation::Revoked);
        agent
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;

        assert!(
            proxies_cancel.is_cancelled(),
            "every relay proxy keyed on the connection token is torn down"
        );
        assert!(
            matches!(mailbox.unsubscribes.try_recv(), Ok(Unsubscribe::Client(client)) if client == agent.client),
            "the hub drops the consumer's proxy subscriptions"
        );
        assert!(
            matches!(
                mailbox.requests.try_recv(),
                Ok(RelayRequest::Command { command: Command::ReleaseInput { terminal_id }, .. })
                    if terminal_id == WireResourceId::local(7)
            ),
            "the satellite lease is released over the link"
        );
        assert!(fx.state.with(|s| s.satellite_leases_held_by(agent.client).is_empty()));
        agent.finish().await;
    })
    .await;
}
