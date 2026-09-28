//! SPEC §12 merge half: keystrokes from two clients on one session merge
//! into the pane's PTY in arrival order. Sends are serialized (each waits for
//! its `cat` echo) so the wire order is deterministic; the PTY is the merge
//! point.

#![allow(
    clippy::similar_names,
    reason = "client_a / client_b are the test's vocabulary"
)]

use phux_protocol::ResourceId;
use phux_protocol::input::key::PhysicalKey;
use phux_protocol::wire::frame::{
    FrameKind, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN, TYPE_RESOURCE_OUTPUT,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::screen::Screen;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, ascii_key, attach_by_name, join_after_shutdown,
    recv_typed, run_local, send_frame, spawn_server_with_seed_cmd, wait_for_socket,
};

/// Attach to `default`, drain `ATTACHED` + snapshot, return the pane.
async fn attach_default(socket_path: &std::path::Path) -> (UnixStream, ResourceId) {
    let mut stream = wait_for_socket(socket_path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(&mut stream, &attach_by_name("default")).await;

    let (type_byte, attached) = recv_typed(&mut stream).await;
    assert_eq!(type_byte, TYPE_ATTACHED, "first frame must be ATTACHED");
    let terminal_id = match attached {
        FrameKind::Attached { snapshot, .. } => {
            assert_eq!(snapshot.resources.len(), 1, "exactly one pane");
            snapshot.resources[0].id.clone()
        }
        other => panic!("expected Attached, got {other:?}"),
    };

    let (type_byte, _snap) = recv_typed(&mut stream).await;
    assert_eq!(
        type_byte, TYPE_BOOTSTRAP_BEGIN,
        "second frame must be TERMINAL_SNAPSHOT",
    );
    (stream, terminal_id)
}

/// Feed `RESOURCE_OUTPUT` into `screen` until row 0 contains `needle`.
async fn drain_until_row0(stream: &mut UnixStream, screen: &mut Screen, needle: &str) {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if screen.row(0).contains(needle) {
            return;
        }
        let remaining = deadline - tokio::time::Instant::now();
        let Ok((type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            return;
        };
        if type_byte != TYPE_RESOURCE_OUTPUT {
            continue;
        }
        if let FrameKind::ResourceOutput { bytes, .. } = frame {
            screen.write(&bytes);
        }
    }
}

#[test]
fn byc_6_5_keystroke_merge_arrival_order_preserved() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        let cmd = CommandBuilder::new("/bin/cat");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        let (mut client_a, terminal_id) = attach_default(&socket_path).await;
        let (mut client_b, terminal_id_b) = attach_default(&socket_path).await;
        assert_eq!(
            terminal_id, terminal_id_b,
            "both clients must share the same pane",
        );

        let mut screen = Screen::new(80, 24).expect("Screen::new");
        let steps = [
            (true, 'a', PhysicalKey::A, "a"),
            (false, 'b', PhysicalKey::B, "ab"),
            (true, 'c', PhysicalKey::C, "abc"),
            (false, 'd', PhysicalKey::D, "abcd"),
        ];
        for (from_a, ch, key, expect_prefix) in steps {
            let sender = if from_a { &mut client_a } else { &mut client_b };
            send_frame(
                sender,
                &FrameKind::InputKey {
                    terminal_id: terminal_id.clone(),
                    event: ascii_key(ch, key),
                },
            )
            .await;
            drain_until_row0(&mut client_a, &mut screen, expect_prefix).await;
            assert!(
                screen.row(0).contains(expect_prefix),
                "after sending '{ch}' from client {}, merged echo must read \
                 '{expect_prefix}' in order; row0 was {:?}",
                if from_a { "A" } else { "B" },
                screen.row(0),
            );
        }

        drop(client_a);
        drop(client_b);
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}
