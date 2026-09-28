//! ADR-0109 `KILL_RESOURCE_IF` on one server: a late kill lands only on a
//! bound pane that is still the caller's, meaning nobody else attached or
//! drove it, it has no child the kill would take too, and its id still names
//! the same incarnation.

use phux_protocol::ids::{ResourceId, ServerInstance};
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::PhysicalKey;
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, KillConditions, KillPrecondition, SpawnResource, SpawnResult,
};
use phux_server_testkit::{Spawn, ascii_key, command, spawn_resource};
use tokio::net::UnixStream;

use crate::common::{Server, attach, wait_for_state};

const SESSION: &str = "main";

async fn spawn_bound(stream: &mut UnixStream, request_id: u32) -> (ResourceId, ServerInstance) {
    let spawn = Spawn {
        resource: Some(Box::new(SpawnResource::default().with_bind_instance(true))),
        ..Spawn::command(&["/bin/cat"])
    };
    match spawn_resource(stream, request_id, spawn).await {
        SpawnResult::OkBound { id, instance } => (id, instance),
        other => panic!("a bind request must be answered bound: {other:?}"),
    }
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

async fn is_alive(stream: &mut UnixStream, request_id: u32, pane: &ResourceId) -> bool {
    let snapshot = crate::common::state(stream, request_id).await;
    crate::common::find(&snapshot, pane).is_some()
}

fn assert_refused(result: &CommandResult, what: &str) {
    assert!(
        matches!(
            result,
            CommandResult::Error {
                code: ErrorCode::PreconditionFailed,
                ..
            }
        ),
        "{what}: expected PRECONDITION_FAILED, got {result:?}"
    );
}

#[test]
fn conditional_kill_takes_only_panes_still_the_callers() {
    phux_server_testkit::run_local(async {
        let server = Server::start(Some(SESSION), |_| {});
        let mut spawner = server.connect().await;

        // Killed: only its spawner ever attached it.
        let (own, instance) = spawn_bound(&mut spawner, 1).await;
        let attach_own = Command::AttachResource {
            terminal_id: own.clone(),
            role_policy: None,
        };
        assert!(matches!(
            command(&mut spawner, 2, attach_own).await,
            CommandResult::Ok | CommandResult::OkWith(_)
        ));
        assert_eq!(
            kill_if(&mut spawner, 3, &own, instance).await,
            CommandResult::Ok
        );
        wait_for_state(&mut spawner, 100, "the pane to be reaped", |s| {
            crate::common::find(s, &own).is_none()
        })
        .await;

        // Refused: another client drove it with send-keys-style input.
        let (driven, driven_instance) = spawn_bound(&mut spawner, 6).await;
        let mut agent = server.connect().await;
        let send_keys = Command::RouteInput {
            terminal_id: driven.clone(),
            event: InputEvent::Key(ascii_key('a', PhysicalKey::A)),
        };
        assert_eq!(command(&mut agent, 1, send_keys).await, CommandResult::Ok);
        drop(agent);
        assert_refused(
            &kill_if(&mut spawner, 7, &driven, driven_instance).await,
            "driven by another",
        );

        // Refused: the attachment condition without an instance token.
        let (untokened, _) = spawn_bound(&mut spawner, 8).await;
        let no_token = Command::KillResourceIf {
            terminal_id: untokened.clone(),
            precondition: KillPrecondition {
                instance: None,
                conditions: KillConditions::UNATTACHED_SINCE_SPAWN,
            },
            operation_id: None,
        };
        assert_refused(&command(&mut spawner, 9, no_token).await, "no instance");

        // Refused: a child resource the kill would close too.
        let (parent, parent_instance) = spawn_bound(&mut spawner, 10).await;
        let child = Spawn {
            resource: Some(Box::new(SpawnResource::agent_session(
                parent.clone(),
                "claude",
            ))),
            ..Spawn::default()
        };
        assert!(matches!(
            spawn_resource(&mut spawner, 11, child).await,
            SpawnResult::Ok(_)
        ));
        assert_refused(
            &kill_if(&mut spawner, 12, &parent, parent_instance).await,
            "has a child",
        );

        // Refused: another client attached the session it sits in, then left.
        // Last, because a session attach counts against every pane in it.
        let (seen, seen_instance) = spawn_bound(&mut spawner, 4).await;
        let mut other = server.connect().await;
        attach(&mut other, SESSION).await;
        drop(other);
        assert_refused(
            &kill_if(&mut spawner, 5, &seen, seen_instance).await,
            "attached by another",
        );

        for (request_id, pane) in (20..).zip([&seen, &driven, &untokened, &parent]) {
            assert!(
                is_alive(&mut spawner, request_id, pane).await,
                "{pane:?} survives a refused kill"
            );
        }

        drop(spawner);
        server.stop().await;
    });
}

/// A cold restart reissues pane ids under a new instance token, so a kill
/// carrying the old id and old token is refused and the new pane survives.
#[test]
fn conditional_kill_refuses_a_reissued_id_after_a_cold_restart() {
    use phux_server_testkit::{
        SOCKET_CONNECT_DEADLINE, join_after_shutdown, spawn_server, wait_for_socket,
    };
    phux_server_testkit::run_local(async {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, handle) = spawn_server(socket.clone(), Some(SESSION));
        let mut before = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let (old_pane, old_instance) = spawn_bound(&mut before, 1).await;
        drop(before);
        join_after_shutdown(shutdown, handle).await;

        let (shutdown, handle) = spawn_server(socket.clone(), Some(SESSION));
        let mut after = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let (new_pane, new_instance) = spawn_bound(&mut after, 1).await;
        assert_eq!(new_pane, old_pane, "a cold start reissues pane ids");
        assert_ne!(new_instance, old_instance, "under a new token");
        assert_refused(
            &kill_if(&mut after, 2, &old_pane, old_instance).await,
            "stale incarnation",
        );
        assert!(is_alive(&mut after, 3, &new_pane).await);
        assert_eq!(
            kill_if(&mut after, 4, &new_pane, new_instance).await,
            CommandResult::Ok
        );

        drop(after);
        join_after_shutdown(shutdown, handle).await;
    });
}
