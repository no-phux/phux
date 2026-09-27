//! Two clients attaching concurrently to one session get distinct client ids,
//! the same pane, byte-identical bootstraps, and both see a keystroke from
//! either (per-pane broadcast fanout, SPEC §12 / ADR-0006).

use phux_protocol::ids::ResourceId;
use phux_protocol::input::key::PhysicalKey;
use phux_protocol::wire::frame::FrameKind;
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::screen::Screen;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, join_after_shutdown, recv_typed, run_local,
    send_frame, spawn_server_with_seed_cmd, wait_for_socket,
};

use super::common::{render_until, type_line};

struct Attached {
    stream: UnixStream,
    client_id: u32,
    pane: ResourceId,
    bootstrap: (u16, u16, bytes::Bytes),
}

async fn attach_default(socket: std::path::PathBuf) -> Attached {
    let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
    send_frame(&mut stream, &attach_by_name("default")).await;
    let FrameKind::Attached {
        snapshot,
        initial_client_id,
        ..
    } = recv_typed(&mut stream).await.1
    else {
        panic!("expected ATTACHED first");
    };
    assert_eq!(snapshot.resources.len(), 1);
    let FrameKind::BootstrapBegin { cols, rows, .. } = recv_typed(&mut stream).await.1 else {
        panic!("expected BOOTSTRAP_BEGIN");
    };
    let FrameKind::BootstrapChunk { payload, .. } = recv_typed(&mut stream).await.1 else {
        panic!("expected BOOTSTRAP_CHUNK");
    };
    assert!(matches!(
        recv_typed(&mut stream).await.1,
        FrameKind::BootstrapReady { .. }
    ));
    Attached {
        stream,
        client_id: initial_client_id.get(),
        pane: snapshot.resources[0].id.clone(),
        bootstrap: (cols, rows, payload),
    }
}

#[test]
fn concurrent_attaches_see_identical_state_and_fanout() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        // `cat` stays quiet until typed into, so two concurrent captures of
        // the grid are well-defined and must be byte-identical.
        let (shutdown, server) =
            spawn_server_with_seed_cmd(socket.clone(), "default", CommandBuilder::new("/bin/cat"));

        let a = tokio::task::spawn_local(attach_default(socket.clone()));
        let b = tokio::task::spawn_local(attach_default(socket.clone()));
        let (mut a, mut b) = (a.await.unwrap(), b.await.unwrap());
        assert_ne!(a.client_id, b.client_id, "client ids must not collide");
        assert_eq!(a.pane, b.pane);
        assert_eq!(a.bootstrap, b.bootstrap, "bootstraps must be identical");

        type_line(&mut a.stream, &a.pane, 'm', PhysicalKey::M).await;
        let mut screen_a = Screen::new(80, 24).unwrap();
        let mut screen_b = Screen::new(80, 24).unwrap();
        tokio::join!(
            render_until(&mut a.stream, &mut screen_a, "m"),
            render_until(&mut b.stream, &mut screen_b, "m"),
        );

        drop((a, b));
        join_after_shutdown(shutdown, server).await;
        assert!(!socket.exists(), "socket leaked after shutdown");
    });
}
