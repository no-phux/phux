//! `phux-byc.6.5` — keystroke merge arrival-order preserved.
//!
//! Wire-level integration test for the merge half of SPEC §12: when two
//! clients attached to the same session send input, those keystrokes
//! merge into the single pane's PTY in arrival order. We prove it
//! end-to-end through the real wire path (`handle_client` →
//! `InputKey` dispatch → pane actor → PTY), observing the order via the
//! tty's own echo (a `/bin/cat` seed echoes each byte as it arrives).
//!
//! Race tolerance (per the byc.6.5 design note): plain interleaved sends
//! from two independent tasks race on the wire, so the *global* order is
//! not deterministic. We make it deterministic by serializing: send one
//! key, wait until its echo lands on the shared stream, then send the
//! next from the other client. The single PTY is the merge point; if it
//! preserved arrival order, the echoed line reads back in send order.
//!
//! Complements `multi_client_scenario` (byc.6.4, fanout): that test
//! proves both clients *see* a keystroke; this one proves the *order* of
//! keystrokes from *different* clients is preserved at the pane.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]
#![allow(
    clippy::similar_names,
    reason = "client_a / client_b are the test's vocabulary"
)]

mod common;

use phux_protocol::TerminalId;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    FrameKind, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN, TYPE_TERMINAL_OUTPUT,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use crate::common::screen::Screen;
use crate::common::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

fn ascii_key(c: char, key: PhysicalKey) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: Some(c.to_string()),
        unshifted_codepoint: Some(c as u32),
    }
}

/// Attach a fresh socket to `default` and drain the opening
/// `ATTACHED + TERMINAL_SNAPSHOT` pair. Returns the stream and the pane's
/// `terminal_id`.
async fn attach_default(socket_path: &std::path::Path) -> (UnixStream, TerminalId) {
    let mut stream = wait_for_socket(socket_path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(&mut stream, &attach_by_name("default")).await;

    let (type_byte, attached) = recv_typed(&mut stream).await;
    assert_eq!(type_byte, TYPE_ATTACHED, "first frame must be ATTACHED");
    let terminal_id = match attached {
        FrameKind::Attached { snapshot, .. } => {
            assert_eq!(snapshot.panes.len(), 1, "exactly one pane");
            snapshot.panes[0].id.clone()
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

/// Drain `TERMINAL_OUTPUT` from `stream` into `screen` until the merged
/// echo line (row 0) contains `needle`, or `WIRE_RECV_TIMEOUT` elapses.
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
        if type_byte != TYPE_TERMINAL_OUTPUT {
            continue;
        }
        if let FrameKind::TerminalOutput { bytes, .. } = frame {
            screen.write(&bytes);
        }
    }
}

#[test]
fn byc_6_5_keystroke_merge_arrival_order_preserved() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // `cat` in a PTY: the tty driver echoes each input byte as it
        // arrives, so the merged input order is observable as the echoed
        // line without needing a newline flush.
        let cmd = CommandBuilder::new("/bin/cat");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        let (mut client_a, terminal_id) = attach_default(&socket_path).await;
        let (mut client_b, terminal_id_b) = attach_default(&socket_path).await;
        assert_eq!(
            terminal_id, terminal_id_b,
            "both clients must share the same pane",
        );

        // We observe the merged stream on client A. Serialize alternating
        // sends A,B,A,B and wait for each echo before the next so the
        // wire order is deterministic — the single PTY is the merge point.
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

        // Final invariant: the four keystrokes from the two clients merged
        // into the pane in exact arrival order.
        assert!(
            screen.row(0).contains("abcd"),
            "keystroke merge order not preserved; row0 = {:?}",
            screen.row(0),
        );

        drop(client_a);
        drop(client_b);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server didn't shut down")
            .expect("server join")
            .expect("server run_async ok");
    });
}
