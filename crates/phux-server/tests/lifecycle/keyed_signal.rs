//! `docs/spec/L1.md` §5.1.1 — a keyed kill never runs twice.
//!
//! Every case drives the production read loop over a real UDS connection.
//! A keyed `KILL_RESOURCE` repeated after it killed answers the first `OK`,
//! where the same kill unkeyed now finds nothing; the same key naming another
//! target is refused and kills nothing; and the key rides the `pane_closed`
//! the kill causes. The dedupe bounds are pinned in `runtime::keyed_ops` and
//! `runtime::operation_dedupe`, and the hub's forwarding and incarnation
//! fence in `hub::relay`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::path::Path;

use phux_protocol::ids::{IdempotencyKey, ResourceId};
use phux_protocol::wire::frame::{
    AgentEvent, AttachTarget, Command, CommandResult, CommandValue, ErrorCode, FrameKind,
    SpawnResult, StateScope, TYPE_ATTACHED, TYPE_RESOURCE_SPAWNED, ViewportInfo,
};
use phux_server::DEFAULT_GROUP_ID;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, await_command_result, join_after_shutdown,
    recv_typed, run_local, send_frame, spawn_server, wait_for_socket,
};

const fn key(byte: u8) -> Option<IdempotencyKey> {
    IdempotencyKey::new([byte; 16])
}

/// Connect and attach to `main`, creating it on first use, so the
/// connection is negotiated and hosts spawns.
async fn attach_main(socket_path: &Path) -> UnixStream {
    let mut stream = wait_for_socket(socket_path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut stream,
        &FrameKind::Attach {
            attach_id: 1,
            target: AttachTarget::CreateIfMissing {
                name: "main".to_owned(),
                command: None,
                cwd: None,
            },
            viewport: ViewportInfo::new(80, 24),
            request_scrollback: false,
            scrollback_limit_lines: 0,
            role_policy: None,
        },
    )
    .await;
    let (type_byte, _) = recv_typed(&mut stream).await;
    assert_eq!(type_byte, TYPE_ATTACHED, "expected ATTACHED");
    stream
}

/// Spawn a pane that stays alive on its PTY without producing output.
async fn spawn_pane(stream: &mut UnixStream, request_id: u32) -> ResourceId {
    send_frame(
        stream,
        &FrameKind::SpawnResource {
            request_id,
            group: DEFAULT_GROUP_ID,
            command: Some(vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "read _".to_owned(),
            ]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: None,
        },
    )
    .await;
    loop {
        let (type_byte, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(stream))
            .await
            .expect("RESOURCE_SPAWNED");
        if type_byte != TYPE_RESOURCE_SPAWNED {
            continue;
        }
        if let FrameKind::ResourceSpawned {
            request_id: got,
            result: SpawnResult::Ok(id),
        } = frame
            && got == request_id
        {
            return id;
        }
    }
}

async fn kill(
    stream: &mut UnixStream,
    request_id: u32,
    terminal_id: &ResourceId,
    operation_id: Option<IdempotencyKey>,
) -> CommandResult {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::KillResource {
                terminal_id: terminal_id.clone(),
                operation_id,
            },
        },
    )
    .await;
    await_command_result(stream, request_id).await
}

async fn resource_ids(stream: &mut UnixStream, request_id: u32) -> Vec<ResourceId> {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::GetState {
                scope: StateScope::Server,
            },
        },
    )
    .await;
    let CommandResult::OkWith(CommandValue::State(snapshot)) =
        await_command_result(stream, request_id).await
    else {
        panic!("GET_STATE must succeed");
    };
    snapshot.resources.into_iter().map(|pane| pane.id).collect()
}

/// Wait until `id` is gone from the inventory: the kill has been reaped.
async fn await_gone(stream: &mut UnixStream, first_request_id: u32, id: &ResourceId) {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    let mut request_id = first_request_id;
    while tokio::time::Instant::now() < deadline {
        if !resource_ids(stream, request_id).await.contains(id) {
            return;
        }
        request_id += 1;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("{id:?} was not reaped");
}

#[test]
fn keyed_kill_repeated_returns_the_cached_result_and_kills_once() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);
        let mut stream = attach_main(&socket_path).await;
        let target = spawn_pane(&mut stream, 1).await;
        let bystander = spawn_pane(&mut stream, 2).await;

        assert_eq!(
            kill(&mut stream, 3, &target, key(1)).await,
            CommandResult::Ok
        );
        await_gone(&mut stream, 100, &target).await;

        assert!(
            matches!(
                kill(&mut stream, 4, &target, None).await,
                CommandResult::Error {
                    code: ErrorCode::TerminalNotFound,
                    ..
                }
            ),
            "run again, the kill would find nothing"
        );
        assert_eq!(
            kill(&mut stream, 5, &target, key(1)).await,
            CommandResult::Ok,
            "the keyed repeat answers the first result and runs nothing"
        );
        assert!(
            resource_ids(&mut stream, 200).await.contains(&bystander),
            "the repeat killed nothing else"
        );

        drop(stream);
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}

#[test]
fn keyed_kill_with_a_different_target_is_idempotency_conflict() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);
        let mut stream = attach_main(&socket_path).await;
        let first = spawn_pane(&mut stream, 1).await;
        let second = spawn_pane(&mut stream, 2).await;

        assert_eq!(
            kill(&mut stream, 3, &first, key(2)).await,
            CommandResult::Ok
        );
        let CommandResult::Error { code, message } = kill(&mut stream, 4, &second, key(2)).await
        else {
            panic!("the same key naming another target must be refused");
        };
        assert_eq!(code, ErrorCode::InvalidCommand);
        assert!(message.contains("idempotency conflict"), "{message}");
        assert!(
            resource_ids(&mut stream, 100).await.contains(&second),
            "the refused kill killed nothing"
        );

        drop(stream);
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}

#[test]
fn pane_closed_from_a_keyed_kill_carries_the_operation_id() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);
        let mut watcher = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(
            &mut watcher,
            &FrameKind::SubscribeEvents {
                terminal: None,
                after_seq: Some(u64::MAX),
            },
        )
        .await;
        let _ = resource_ids(&mut watcher, 1).await;
        let mut killer = attach_main(&socket_path).await;
        let target = spawn_pane(&mut killer, 1).await;

        assert_eq!(
            kill(&mut killer, 2, &target, key(3)).await,
            CommandResult::Ok
        );
        let stamp = loop {
            let (_, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(&mut watcher))
                .await
                .expect("pane_closed");
            if let FrameKind::Event {
                terminal: Some(terminal),
                event: AgentEvent::ResourceClosed { .. },
                stamp,
            } = frame
                && terminal == target
            {
                break stamp.expect("journaled");
            }
        };
        assert_eq!(stamp.operation_id, key(3), "the kill's key rides its close");
        assert!(
            stamp.actor.is_some(),
            "and names the connection that killed"
        );

        drop(killer);
        drop(watcher);
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}
