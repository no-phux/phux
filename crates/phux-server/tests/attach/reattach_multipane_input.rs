//! phux-fysb: re-attaching to a multi-pane session subscribes the client to
//! every pane, so `INPUT_KEY` into a non-active pane reaches its PTY (the
//! input gate drops keys for unsubscribed panes). An unattached observer
//! reads each pane's mirror via `GET_SCREEN`, isolating delivery to the PTY
//! from the typing client's own output stream.

use std::time::Duration;

use phux_protocol::ids::ResourceId;
use phux_protocol::input::key::PhysicalKey;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, SpawnResult, StateScope};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, Spawn, attach_by_name, command, run_local, send_frame, spawn_resource,
    spawn_server_with_seed_cmd, wait_for_socket,
};

use super::common::{attached, type_line};

async fn panes_and_focus(
    stream: &mut UnixStream,
    request_id: u32,
) -> (Vec<ResourceId>, ResourceId) {
    let get_state = Command::GetState {
        scope: StateScope::Server,
    };
    let CommandResult::OkWith(CommandValue::State(snap)) =
        command(stream, request_id, get_state).await
    else {
        panic!("GET_STATE failed");
    };
    (
        snap.resources.iter().map(|p| p.id.clone()).collect(),
        snap.focused_resource,
    )
}

/// Poll `GET_SCREEN` on `pane` until it shows `needle`.
async fn assert_echo(stream: &mut UnixStream, pane: &ResourceId, needle: char) {
    let mut last = String::new();
    for request_id in 3000..3040 {
        let get_screen = Command::GetScreen {
            terminal_id: pane.clone(),
            request_scrollback: None,
            cells: false,
            format: 0,
        };
        let CommandResult::OkWith(CommandValue::Json(json)) =
            command(stream, request_id, get_screen).await
        else {
            panic!("GET_SCREEN failed");
        };
        let screen: phux_core::screen::ScreenState = serde_json::from_str(&json).unwrap();
        last = screen.lines.join("");
        if last.contains(needle) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("{needle:?} never reached {pane:?}; screen {last:?}");
}

#[test]
fn reattach_to_multipane_session_can_type_into_non_active_pane() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (_shutdown, _server) =
            spawn_server_with_seed_cmd(socket.clone(), "default", CommandBuilder::new("cat"));
        let mut observer = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;

        // A builds a two-pane session, then leaves.
        let mut a = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut a, &attach_by_name("default")).await;
        attached(&mut a).await;
        let spawned = spawn_resource(&mut a, 1, Spawn::command(&["cat"])).await;
        assert!(
            matches!(spawned, SpawnResult::Ok(ref id) if id.is_local()),
            "{spawned:?}"
        );
        assert_eq!(panes_and_focus(&mut observer, 10).await.0.len(), 2);
        drop(a);

        let mut b = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut b, &attach_by_name("default")).await;
        attached(&mut b).await;
        let (panes, active) = panes_and_focus(&mut observer, 11).await;
        assert_eq!(panes.len(), 2);
        let other = panes.into_iter().find(|p| *p != active).unwrap();

        type_line(&mut b, &active, 'q', PhysicalKey::Q).await;
        assert_echo(&mut observer, &active, 'q').await;
        type_line(&mut b, &other, 'z', PhysicalKey::Z).await;
        assert_echo(&mut observer, &other, 'z').await;
    });
}
