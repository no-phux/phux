//! `KILL_RESOURCE_IF` through the hub (ADR-0109, `docs/spec/L1.md` §9.1).
//! The hub relays the precondition and the satellite evaluates it; because
//! every hub consumer is one connection on the satellite, the hub itself
//! refuses a kill after a second hub consumer attached or typed, or whose
//! token its spawn record does not carry.

use super::*;
use phux_protocol::ids::ServerInstance;
use phux_protocol::wire::frame::{KillConditions, KillPrecondition};
use phux_server_testkit::command;

/// Spawn a bound `/bin/cat`, through the hub to `satellite` or locally.
async fn spawn_bound(
    stream: &mut UnixStream,
    request_id: u32,
    satellite: Option<&str>,
) -> (ResourceId, ServerInstance) {
    let spawn = Spawn {
        satellite: satellite.map(SatelliteHost::new),
        resource: Some(Box::new(SpawnResource::default().with_bind_instance(true))),
        ..Spawn::command(&["/bin/cat"])
    };
    let result = spawn_resource(stream, request_id, spawn).await;
    let SpawnResult::OkBound { id, instance } = result else {
        panic!("a bind request must be answered bound: {result:?}");
    };
    (id, instance)
}

async fn kill_if(
    stream: &mut UnixStream,
    request_id: u32,
    pane: &ResourceId,
    instance: ServerInstance,
) -> CommandResult {
    let kill = Command::KillResourceIf {
        terminal_id: pane.clone(),
        precondition: KillPrecondition::spawned_and_unattached(instance),
        operation_id: None,
    };
    command(stream, request_id, kill).await
}

/// The `PRECONDITION_FAILED` refusal's message.
fn refusal_message(result: CommandResult) -> String {
    let CommandResult::Error {
        code: ErrorCode::PreconditionFailed,
        message,
    } = result
    else {
        panic!("expected PRECONDITION_FAILED, got {result:?}");
    };
    message
}

#[test]
fn a_relayed_conditional_kill_is_evaluated_on_the_satellite() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot().await;
        let mut satellite = fed.satellite().await;
        let mut hub = fed.linked_hub().await;

        // The hub hands back the satellite's own token, unchanged.
        let (_direct, sat_instance) = spawn_bound(&mut satellite, 8000, None).await;
        let (pane, instance) = spawn_bound(&mut hub, 8001, Some("sat")).await;
        assert!(matches!(pane, ResourceId::Satellite { .. }), "{pane:?}");
        assert_eq!(instance, sat_instance);
        let killed = kill_if(&mut hub, 8002, &pane, instance).await;
        assert!(matches!(killed, CommandResult::Ok), "{killed:?}");

        // A stale token with the attachment condition: the hub refuses.
        let (second, _) = spawn_bound(&mut hub, 8003, Some("sat")).await;
        let mut stale = *instance.as_bytes();
        stale[0] ^= 0xFF;
        let stale = ServerInstance::new(stale);
        let message = refusal_message(kill_if(&mut hub, 8004, &second, stale).await);
        assert!(message.contains("this hub"), "{message}");
        // Without the condition only the satellite knows its token: it refuses.
        let instance_only = Command::KillResourceIf {
            terminal_id: second.clone(),
            precondition: KillPrecondition {
                instance: Some(stale),
                conditions: KillConditions::NONE,
            },
            operation_id: None,
        };
        let message = refusal_message(command(&mut hub, 8009, instance_only).await);
        assert!(message.contains("instance token"), "{message}");

        // A satellite-local attach the hub never saw: the satellite refuses.
        send_frame(
            &mut satellite,
            &phux_server_testkit::attach_by_name("sat-session"),
        )
        .await;
        phux_server_testkit::recv_until(&mut satellite, |_, frame| {
            matches!(frame, FrameKind::Attached { .. }).then_some(())
        })
        .await;
        let message = refusal_message(kill_if(&mut hub, 8005, &second, instance).await);
        assert!(message.contains("spawner"), "{message}");
        assert!(matches!(
            get_screen_via_hub(&mut hub, 8006, second).await,
            CommandResult::OkWith(_)
        ));

        drop((hub, satellite));
        fed.shutdown().await;
    });
}

#[test]
fn another_hub_consumer_attaching_or_typing_refuses_the_kill_at_the_hub() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot().await;
        let mut hub = fed.linked_hub().await;
        let mut other = fed.hub().await;

        let (pane, instance) = spawn_bound(&mut hub, 8100, Some("sat")).await;
        let attach = Command::AttachResource {
            terminal_id: pane.clone(),
            role_policy: None,
        };
        let attached = command(&mut other, 8101, attach).await;
        assert!(
            matches!(attached, CommandResult::Ok | CommandResult::OkWith(_)),
            "{attached:?}"
        );
        let message = refusal_message(kill_if(&mut hub, 8102, &pane, instance).await);
        assert!(message.contains("hub consumer"), "{message}");
        assert!(matches!(
            get_screen_via_hub(&mut hub, 8103, pane).await,
            CommandResult::OkWith(_)
        ));

        // Send-keys-style input without attaching counts the same.
        let (typed_into, typed_instance) = spawn_bound(&mut hub, 8104, Some("sat")).await;
        let send_keys = Command::RouteInput {
            terminal_id: typed_into.clone(),
            event: InputEvent::Key(ascii_key('a', PhysicalKey::A)),
        };
        let typed = command(&mut other, 8105, send_keys).await;
        assert!(matches!(typed, CommandResult::Ok), "{typed:?}");
        let message = refusal_message(kill_if(&mut hub, 8106, &typed_into, typed_instance).await);
        assert!(message.contains("hub consumer"), "{message}");

        drop((hub, other));
        fed.shutdown().await;
    });
}
