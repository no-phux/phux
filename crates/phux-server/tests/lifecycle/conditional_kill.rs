//! ADR-0109: `KILL_RESOURCE_IF` against one server.
//!
//! A late kill must not land on a pane that is no longer the caller's to
//! kill. Three scenarios pin what "still the caller's" means:
//!
//! 1. A bound pane nobody but its spawner attached is killed, even after
//!    the spawner's own `ATTACH_RESOURCE`.
//! 2. A pane another client attached (here by attaching the session it was
//!    placed in) is refused with `PRECONDITION_FAILED` and survives, even
//!    after that client left.
//! 3. After a cold restart the server reissues the same pane id to a new
//!    pane under a new instance token. A kill carrying the old id and old
//!    token is refused and the new pane survives.
//! 4. A pane another client drove with send-keys-style `ROUTE_INPUT`, never
//!    attaching, is refused like an attached one.
//! 5. `UNATTACHED_SINCE_SPAWN` without an instance token is refused.
//! 6. A pane with a child resource, which the kill would close too, is
//!    refused.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_protocol::ids::{GroupId, ResourceId, ServerInstance};
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, KillPrecondition, SpawnResource, SpawnResult,
};
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, await_command_result,
    join_after_shutdown, recv_typed, run_local, send_frame, spawn_server, wait_for_socket,
};
use tempfile::TempDir;
use tokio::net::UnixStream;

const SESSION: &str = "main";

/// Spawn `/bin/cat` asking for binding; return the bound id and token.
async fn spawn_bound(stream: &mut UnixStream, request_id: u32) -> (ResourceId, ServerInstance) {
    send_frame(
        stream,
        &FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(1),
            command: Some(vec!["/bin/cat".to_owned()]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: Some(Box::new(SpawnResource::default().with_bind_instance(true))),
        },
    )
    .await;
    let result = tokio::time::timeout(WIRE_RECV_TIMEOUT, async {
        loop {
            if let (
                _,
                FrameKind::ResourceSpawned {
                    request_id: got,
                    result,
                },
            ) = recv_typed(stream).await
                && got == request_id
            {
                return result;
            }
        }
    })
    .await
    .expect("RESOURCE_SPAWNED");
    let SpawnResult::OkBound { id, instance } = result else {
        panic!("a bind request must be answered bound: {result:?}");
    };
    (id, instance)
}

/// Send one command and await its correlated result.
async fn command(stream: &mut UnixStream, request_id: u32, command: Command) -> CommandResult {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command,
        },
    )
    .await;
    await_command_result(stream, request_id).await
}

/// The late kill a client sends for a pane it spawned and bound.
async fn kill_if(
    stream: &mut UnixStream,
    request_id: u32,
    pane: &ResourceId,
    instance: ServerInstance,
) -> CommandResult {
    let kill = Command::KillResourceIf {
        terminal_id: pane.clone(),
        precondition: KillPrecondition::spawned_and_unattached(instance),
    };
    command(stream, request_id, kill).await
}

async fn get_screen(stream: &mut UnixStream, request_id: u32, pane: &ResourceId) -> CommandResult {
    let screen = Command::GetScreen {
        terminal_id: pane.clone(),
        request_scrollback: None,
        cells: false,
    };
    command(stream, request_id, screen).await
}

fn assert_refused(result: &CommandResult) {
    assert!(
        matches!(
            result,
            CommandResult::Error {
                code: ErrorCode::PreconditionFailed,
                ..
            }
        ),
        "expected PRECONDITION_FAILED, got {result:?}"
    );
}

fn assert_alive(result: &CommandResult) {
    assert!(
        matches!(result, CommandResult::OkWith(_)),
        "the pane must survive a refused kill: {result:?}"
    );
}

/// Poll `GET_SCREEN` until the killed pane is reaped.
async fn await_gone(stream: &mut UnixStream, pane: &ResourceId, first_request_id: u32) {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    let mut request_id = first_request_id;
    loop {
        let result = get_screen(stream, request_id, pane).await;
        if matches!(
            result,
            CommandResult::Error {
                code: ErrorCode::TerminalNotFound,
                ..
            }
        ) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "pane survived its kill: {result:?}"
        );
        request_id += 1;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Read until the session `ATTACHED` arrives.
async fn await_attached(stream: &mut UnixStream) {
    tokio::time::timeout(WIRE_RECV_TIMEOUT, async {
        loop {
            if let (_, FrameKind::Attached { .. }) = recv_typed(stream).await {
                return;
            }
        }
    })
    .await
    .expect("ATTACHED");
}

#[test]
fn conditional_kill_takes_a_pane_only_its_spawner_attached() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server(path.clone(), Some(SESSION));
        let mut spawner = wait_for_socket(&path, SOCKET_CONNECT_DEADLINE).await;

        let (pane, instance) = spawn_bound(&mut spawner, 1).await;
        let attach = Command::AttachResource {
            terminal_id: pane.clone(),
        };
        let attached = command(&mut spawner, 2, attach).await;
        assert!(
            matches!(attached, CommandResult::Ok | CommandResult::OkWith(_)),
            "the spawner attaches its own pane: {attached:?}"
        );

        let killed = kill_if(&mut spawner, 3, &pane, instance).await;
        assert!(matches!(killed, CommandResult::Ok), "{killed:?}");
        await_gone(&mut spawner, &pane, 100).await;

        drop(spawner);
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn conditional_kill_refuses_a_pane_another_client_attached() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server(path.clone(), Some(SESSION));
        let mut spawner = wait_for_socket(&path, SOCKET_CONNECT_DEADLINE).await;
        let (pane, instance) = spawn_bound(&mut spawner, 1).await;

        // Another client attaches the session the pane was placed in, then
        // leaves. Having attached it at all is what counts.
        let mut other = wait_for_socket(&path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut other, &attach_by_name(SESSION)).await;
        await_attached(&mut other).await;
        drop(other);

        let refused = kill_if(&mut spawner, 2, &pane, instance).await;
        assert_refused(&refused);
        assert_alive(&get_screen(&mut spawner, 3, &pane).await);

        drop(spawner);
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn conditional_kill_refuses_a_reissued_id_after_a_cold_restart() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("phux.sock");

        let (shutdown, server) = spawn_server(path.clone(), Some(SESSION));
        let mut before = wait_for_socket(&path, SOCKET_CONNECT_DEADLINE).await;
        let (old_pane, old_instance) = spawn_bound(&mut before, 1).await;
        drop(before);
        join_after_shutdown(shutdown, server).await;

        let (shutdown, server) = spawn_server(path.clone(), Some(SESSION));
        let mut after = wait_for_socket(&path, SOCKET_CONNECT_DEADLINE).await;
        let (new_pane, new_instance) = spawn_bound(&mut after, 1).await;
        assert_eq!(
            new_pane, old_pane,
            "a cold start reissues pane ids from the start"
        );
        assert_ne!(
            new_instance, old_instance,
            "and names the new id space with a new token"
        );

        let refused = kill_if(&mut after, 2, &old_pane, old_instance).await;
        assert_refused(&refused);
        assert_alive(&get_screen(&mut after, 3, &new_pane).await);

        let killed = kill_if(&mut after, 4, &new_pane, new_instance).await;
        assert!(matches!(killed, CommandResult::Ok), "{killed:?}");

        drop(after);
        join_after_shutdown(shutdown, server).await;
    });
}

/// What `phux send-keys` sends: input with no subscription and no lease.
fn send_keys(pane: &ResourceId) -> Command {
    Command::RouteInput {
        terminal_id: pane.clone(),
        event: phux_protocol::input::InputEvent::Key(phux_server_testkit::ascii_key(
            'a',
            phux_protocol::input::key::PhysicalKey::A,
        )),
    }
}

#[test]
fn conditional_kill_refuses_a_pane_another_client_drove_without_attaching() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server(path.clone(), Some(SESSION));
        let mut spawner = wait_for_socket(&path, SOCKET_CONNECT_DEADLINE).await;
        let (pane, instance) = spawn_bound(&mut spawner, 1).await;

        // An agent starts work in the pane with send-keys: no attach at all.
        let mut agent = wait_for_socket(&path, SOCKET_CONNECT_DEADLINE).await;
        let typed = command(&mut agent, 1, send_keys(&pane)).await;
        assert!(matches!(typed, CommandResult::Ok), "{typed:?}");
        drop(agent);

        assert_refused(&kill_if(&mut spawner, 2, &pane, instance).await);
        assert_alive(&get_screen(&mut spawner, 3, &pane).await);

        drop(spawner);
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn the_attachment_condition_without_an_instance_is_refused() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server(path.clone(), Some(SESSION));
        let mut spawner = wait_for_socket(&path, SOCKET_CONNECT_DEADLINE).await;
        let (pane, _instance) = spawn_bound(&mut spawner, 1).await;

        let no_token = Command::KillResourceIf {
            terminal_id: pane.clone(),
            precondition: KillPrecondition {
                instance: None,
                conditions: phux_protocol::wire::frame::KillConditions::UNATTACHED_SINCE_SPAWN,
            },
        };
        assert_refused(&command(&mut spawner, 2, no_token).await);
        assert_alive(&get_screen(&mut spawner, 3, &pane).await);

        drop(spawner);
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn conditional_kill_refuses_a_pane_with_a_child_resource() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server(path.clone(), Some(SESSION));
        let mut spawner = wait_for_socket(&path, SOCKET_CONNECT_DEADLINE).await;
        let (pane, instance) = spawn_bound(&mut spawner, 1).await;

        // An agent session bound to the pane: the kill would close it too.
        send_frame(
            &mut spawner,
            &FrameKind::SpawnResource {
                request_id: 2,
                group: GroupId::new(1),
                command: None,
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
                initial_size: None,
                resource: Some(Box::new(SpawnResource::agent_session(
                    pane.clone(),
                    "claude",
                ))),
            },
        )
        .await;
        let child = tokio::time::timeout(WIRE_RECV_TIMEOUT, async {
            loop {
                if let (
                    _,
                    FrameKind::ResourceSpawned {
                        request_id: 2,
                        result,
                    },
                ) = recv_typed(&mut spawner).await
                {
                    return result;
                }
            }
        })
        .await
        .expect("child RESOURCE_SPAWNED");
        assert!(matches!(child, SpawnResult::Ok(_)), "{child:?}");

        assert_refused(&kill_if(&mut spawner, 3, &pane, instance).await);
        assert_alive(&get_screen(&mut spawner, 4, &pane).await);

        drop(spawner);
        join_after_shutdown(shutdown, server).await;
    });
}
