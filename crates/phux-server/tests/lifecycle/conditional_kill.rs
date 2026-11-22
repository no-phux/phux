//! ADR-0109 `KILL_RESOURCE_IF` across a cold restart: a late kill lands only
//! when its id still names the same incarnation. The other preconditions are
//! unit-tested in `state::conditional_kill`.

use phux_protocol::ids::{ResourceId, ServerInstance};
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, KillPrecondition, SpawnResource, SpawnResult,
};
use phux_server_testkit::{Spawn, command, spawn_resource};
use tokio::net::UnixStream;

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
        "{what}: {result:?}"
    );
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
