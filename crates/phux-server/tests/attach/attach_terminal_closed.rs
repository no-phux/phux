//! phux-w7z2.56: a consumer subscribed through `ATTACH_RESOURCE` alone (no
//! session `ATTACH`, the shape of agents and federation proxies) receives
//! `RESOURCE_CLOSED` when the pane dies (L1 §3.1), and the session-attached
//! consumer still receives it exactly once. No sleeps: the pane is killed on
//! request, and "exactly once" is anchored on a `GET_STATE` barrier reply,
//! which the in-order frame loop queues behind any duplicate.

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{Command, CommandResult, FrameKind, SpawnResult, StateScope};
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, Spawn, WIRE_RECV_TIMEOUT, attach_by_name, command, recv_until,
    recv_until_deadline, run_local, send_frame, spawn_resource, spawn_server_with_seed_cmd,
    wait_for_socket,
};

use super::common::{attached, sh};

const IMMORTAL: &str = "while :; do sleep 3600; done";

/// Wait for `RESOURCE_CLOSED` for `victim`, then count further ones up to a
/// `GET_STATE` barrier. Returns the exit status.
async fn closed_exactly_once(
    stream: &mut UnixStream,
    victim: &ResourceId,
    barrier: u32,
) -> Option<i32> {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    let status = recv_until_deadline(stream, deadline, |_, frame| match frame {
        FrameKind::ResourceClosed {
            terminal_id,
            exit_status,
            ..
        } if terminal_id == *victim => Some(exit_status),
        _ => None,
    })
    .await
    .expect("RESOURCE_CLOSED never arrived");
    let get_state = FrameKind::Command {
        request_id: barrier,
        command: Command::GetState {
            scope: StateScope::Server,
        },
    };
    send_frame(stream, &get_state).await;
    let mut extra = 0;
    recv_until(stream, |_, frame| match frame {
        FrameKind::ResourceClosed { terminal_id, .. } if terminal_id == *victim => {
            extra += 1;
            None
        }
        FrameKind::CommandResult { request_id, .. } if request_id == barrier => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(extra, 0, "RESOURCE_CLOSED must arrive exactly once");
    status
}

#[test]
fn attach_terminal_only_consumer_receives_terminal_closed() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (_shutdown, _server) = spawn_server_with_seed_cmd(socket.clone(), "demo", sh(IMMORTAL));

        let mut owner = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut owner, &attach_by_name("demo")).await;
        attached(&mut owner).await;
        // A second pane, so its death cannot trip last-pane self-exit.
        let SpawnResult::Ok(victim) =
            spawn_resource(&mut owner, 1, Spawn::command(&["/bin/sh", "-c", IMMORTAL])).await
        else {
            panic!("victim spawn failed");
        };

        let mut watcher = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let subscribe = Command::AttachResource {
            terminal_id: victim.clone(),
            role_policy: None,
        };
        // The reply proves the subscription is installed before the kill.
        assert_eq!(
            command(&mut watcher, 100, subscribe).await,
            CommandResult::Ok
        );
        let kill = FrameKind::Command {
            request_id: 2,
            command: Command::KillResource {
                terminal_id: victim.clone(),
                operation_id: None,
            },
        };
        send_frame(&mut owner, &kill).await;

        let watcher_status = closed_exactly_once(&mut watcher, &victim, 101).await;
        let owner_status = closed_exactly_once(&mut owner, &victim, 102).await;
        assert_eq!(owner_status, watcher_status, "same lifecycle fact for both");
    });
}
