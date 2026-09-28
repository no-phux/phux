//! Detach then reattach (SPEC §7.3 / §13): a fresh client gets a new client
//! id, a reset-preamble bootstrap of the same live pane, and a working output
//! stream; the socket is unlinked on shutdown.

use phux_protocol::input::key::PhysicalKey;
use phux_protocol::wire::frame::{DetachReason, FrameKind};
use portable_pty::CommandBuilder;
use tempfile::TempDir;

use phux_server_testkit::screen::Screen;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, join_after_shutdown, recv_typed, recv_until_detached,
    run_local, send_frame, spawn_server_with_seed_cmd, wait_for_socket,
};

use super::common::{render_until, type_line};

#[test]
fn reconnect_after_detach_replays_snapshot_and_resumes_output() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) =
            spawn_server_with_seed_cmd(socket.clone(), "default", CommandBuilder::new("/bin/cat"));

        let mut a = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut a, &attach_by_name("default")).await;
        let FrameKind::Attached {
            snapshot,
            initial_client_id,
            ..
        } = recv_typed(&mut a).await.1
        else {
            panic!("A: expected ATTACHED");
        };
        let (pane, a_id) = (snapshot.resources[0].id.clone(), initial_client_id.get());
        assert!(a_id >= 1);
        type_line(&mut a, &pane, 'a', PhysicalKey::A).await;
        render_until(&mut a, &mut Screen::new(80, 24).unwrap(), "a").await;
        send_frame(&mut a, &FrameKind::Detach).await;
        assert!(matches!(
            recv_until_detached(&mut a).await,
            FrameKind::Detached {
                reason: Some(DetachReason::Requested),
                ..
            }
        ));
        drop(a);

        let mut b = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut b, &attach_by_name("default")).await;
        let FrameKind::Attached {
            snapshot,
            initial_client_id,
            ..
        } = recv_typed(&mut b).await.1
        else {
            panic!("B: expected ATTACHED");
        };
        assert_eq!(snapshot.resources.len(), 1, "the pane survived the detach");
        assert_eq!(snapshot.resources[0].id, pane, "terminal id is stable");
        assert!(initial_client_id.get() > a_id, "client ids are monotonic");
        assert!(matches!(
            recv_typed(&mut b).await.1,
            FrameKind::BootstrapBegin {
                cols: 80,
                rows: 24,
                ..
            }
        ));
        let FrameKind::BootstrapChunk { payload, .. } = recv_typed(&mut b).await.1 else {
            panic!("B: expected BOOTSTRAP_CHUNK");
        };
        assert!(
            payload.starts_with(b"\x1b[!p\x1b[2J\x1b[H"),
            "reset preamble"
        );
        assert!(matches!(
            recv_typed(&mut b).await.1,
            FrameKind::BootstrapReady {
                history_cursor: None,
                ..
            }
        ));

        let mut screen = Screen::new(80, 24).unwrap();
        screen.write(&payload);
        type_line(&mut b, &pane, 'z', PhysicalKey::Z).await;
        render_until(&mut b, &mut screen, "z").await;

        drop(b);
        join_after_shutdown(shutdown, server).await;
        assert!(!socket.exists(), "socket leaked after shutdown");
    });
}
