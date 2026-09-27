//! A pipelined spawn storm then kill storm (plus a double kill racing the
//! reap) must get a typed reply for every request and leave the server
//! responsive.

use phux_protocol::wire::frame::{Command, CommandResult, FrameKind, SpawnResult, StateScope};
use tokio::net::UnixStream;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, Spawn, attach_by_name, command, join_after_shutdown, recv_until,
    run_local, send_frame, spawn_server_seed_pty_no_cmd, wait_for_socket,
};

async fn next_command_result(stream: &mut UnixStream) -> CommandResult {
    recv_until(stream, |_, frame| match frame {
        FrameKind::CommandResult { result, .. } => Some(result),
        _ => None,
    })
    .await
}

const fn kill(request_id: u32, terminal_id: phux_protocol::ResourceId) -> FrameKind {
    FrameKind::Command {
        request_id,
        command: Command::KillResource {
            terminal_id,
            operation_id: None,
        },
    }
}

#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just stress`."]
#[test]
fn spawn_storm_then_kill_storm_does_not_panic() {
    run_local(async {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_seed_pty_no_cmd(socket_path.clone(), Some("default"));
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("default")).await;

        for request_id in 0..24 {
            send_frame(
                &mut stream,
                &Spawn::command(&["/bin/sh", "-c", "sleep 30"]).frame(request_id),
            )
            .await;
        }
        let mut replies = 0;
        let mut spawned = Vec::new();
        while replies < 24 {
            let result = recv_until(&mut stream, |_, frame| match frame {
                FrameKind::ResourceSpawned { result, .. } => Some(result),
                _ => None,
            })
            .await;
            replies += 1;
            if let SpawnResult::Ok(id) = result {
                spawned.push(id);
            }
        }
        assert!(!spawned.is_empty(), "spawn storm produced no live panes");

        for (request_id, id) in (100..).zip(&spawned) {
            send_frame(&mut stream, &kill(request_id, id.clone())).await;
        }
        for _ in &spawned {
            let _ = next_command_result(&mut stream).await;
        }
        send_frame(&mut stream, &kill(99, spawned[0].clone())).await;
        let _ = next_command_result(&mut stream).await;

        let state = command(
            &mut stream,
            98,
            Command::GetState {
                scope: StateScope::Server,
            },
        )
        .await;
        assert!(
            matches!(state, CommandResult::OkWith(_)),
            "unresponsive: {state:?}"
        );

        drop(stream);
        join_after_shutdown(shutdown, server).await;
    });
}
