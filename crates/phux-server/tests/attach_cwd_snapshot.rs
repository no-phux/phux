//! phux-p4vp — the `ATTACHED` snapshot carries a per-pane `cwd`.
//!
//! Wire-level integration test for the sidebar's VCS-branch data path.
//! The TUI derives each window's git branch client-side from
//! `SessionSnapshot.panes[].cwd` (see `phux-client/src/attach/
//! server_frame.rs`), so a server that ships `cwd: None` for normally
//! spawned panes renders every branch row blank. Two server-side
//! mechanisms populate the field:
//!
//! 1. Spawn-time stamping: `seed_session_with_pty` /
//!    `spawn_pane_with_pty` copy the `CommandBuilder`'s cwd (or the
//!    server process's own CWD when unset) onto the pane's
//!    `TerminalDescriptor.cwd`.
//! 2. Attach-time refresh: `handle_attach` re-queries each live PTY
//!    child's kernel CWD (`refresh_registry_cwds`) right before the
//!    snapshot is built, so a post-spawn `cd` is reflected.
//!
//! This test pins the end-to-end contract: seed a PTY-backed pane in a
//! known temp directory, attach, and assert the `ATTACHED` frame's lone
//! pane carries that directory. Both mechanisms above resolve to the
//! same path here (the child never `cd`s), so the assertion holds even
//! if one of them regresses to a no-op — regressing *both* fails it.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::path::PathBuf;
use std::time::Duration;

use phux_protocol::wire::frame::{FrameKind, TYPE_ATTACHED};
use portable_pty::CommandBuilder;
use tempfile::TempDir;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

#[test]
fn attached_snapshot_carries_pane_cwd() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // Seed pane: a shell blocked on `read _` (a builtin, so the child
        // stays alive on the PTY) whose CommandBuilder cwd is a fresh temp
        // dir. Canonicalize up front so the expectation matches both the
        // stamped builder path and the kernel CWD query's resolved form
        // (macOS resolves /var -> /private/var).
        let cwd_dir = TempDir::new().unwrap();
        let cwd_path = cwd_dir.path().canonicalize().expect("canonicalize cwd");
        let mut seed = CommandBuilder::new("/bin/sh");
        seed.arg("-c");
        seed.arg("read _");
        seed.cwd(&cwd_path);
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "cwd-test", seed);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        send_frame(&mut stream, &attach_by_name("cwd-test")).await;
        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "first server-to-client frame must be ATTACHED (got type 0x{type_byte:02x})",
        );
        match attached {
            FrameKind::Attached { snapshot, .. } => {
                assert_eq!(snapshot.panes.len(), 1, "exactly one pane");
                let wire_cwd = snapshot.panes[0]
                    .cwd
                    .as_deref()
                    .expect("ATTACHED pane must carry a cwd for a PTY-backed pane");
                // Canonicalize the wire value too: the spawn-time stamp is
                // the (already canonical) builder path, while a live kernel
                // refresh may return an equivalent-but-distinct spelling.
                let wire_path = PathBuf::from(wire_cwd);
                let wire_path = wire_path.canonicalize().unwrap_or(wire_path);
                assert_eq!(
                    wire_path, cwd_path,
                    "ATTACHED pane cwd must be the seed pane's working directory",
                );
            }
            other => panic!("expected FrameKind::Attached, got {other:?}"),
        }

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}

/// The attach-time refresh specifically: a pane whose shell `cd`s away
/// from its spawn directory must report the *post-cd* directory in the
/// `ATTACHED` snapshot. The spawn-time stamp alone would report the
/// stale spawn directory, so this pins `refresh_registry_cwds`.
#[test]
fn attached_snapshot_reflects_post_spawn_cd() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // Spawn in `spawn_dir`, immediately `cd` into `live_dir`, then
        // block on the PTY (`read _` is a builtin, so the child stays
        // alive in `live_dir`).
        let spawn_dir = TempDir::new().unwrap();
        let live_dir = TempDir::new().unwrap();
        let live_path = live_dir.path().canonicalize().expect("canonicalize cwd");
        let mut seed = CommandBuilder::new("/bin/sh");
        seed.arg("-c");
        seed.arg(format!("cd '{}' && read _", live_path.display()));
        seed.cwd(spawn_dir.path());
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "cwd-live", seed);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // Give the seed shell a beat to run its `cd` before the attach
        // queries it (same margin the phux-cs6 inherit-focused test uses).
        tokio::time::sleep(Duration::from_millis(150)).await;

        send_frame(&mut stream, &attach_by_name("cwd-live")).await;
        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED, "expected ATTACHED");
        match attached {
            FrameKind::Attached { snapshot, .. } => {
                assert_eq!(snapshot.panes.len(), 1, "exactly one pane");
                let wire_cwd = snapshot.panes[0]
                    .cwd
                    .as_deref()
                    .expect("ATTACHED pane must carry a cwd for a PTY-backed pane");
                let wire_path = PathBuf::from(wire_cwd);
                let wire_path = wire_path.canonicalize().unwrap_or(wire_path);
                assert_eq!(
                    wire_path, live_path,
                    "ATTACHED pane cwd must be the shell's live (post-cd) directory, \
                     not the stale spawn directory",
                );
            }
            other => panic!("expected FrameKind::Attached, got {other:?}"),
        }

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}
