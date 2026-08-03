//! `phux-k61.3` — `AttachTarget::CreateIfMissing` wire integration.
//!
//! Three scenarios pin the behavior added by phux-k61.3:
//!
//! 1. **Create on miss.** Server starts with no sessions. A client
//!    sends `ATTACH { CreateIfMissing { name: "foo", … } }`. The
//!    server creates `foo` (one window, one pane) and replies with
//!    `ATTACHED` whose `SessionSnapshot` carries that single session,
//!    then a `TERMINAL_SNAPSHOT` for the seed pane.
//!
//! 2. **Reuse on hit.** Server starts with `existing` pre-seeded
//!    (one window, one pane). The same `CreateIfMissing { name:
//!    "existing", … }` attaches to the pre-existing session and does
//!    not create a duplicate — the snapshot still lists exactly one
//!    session named `existing`.
//!
//! 3. **`CreateIfMissing` with empty name == empty.** The wire schema
//!    pins `name: String` (not `Option<String>`), so the "default
//!    name" semantics live on the *client* (naked `phux` chooses
//!    `"default"` per `crates/phux/src/main.rs:46`). Exercising
//!    "default name" at the server boundary means sending the literal
//!    string `"default"`; we cover that under (1) by parameterising
//!    one extra subcase.
//!
//! All three scenarios run in the same test binary because they share
//! the helper plumbing and the wire `recv` shape — see
//! `byc_6_6_attach_unknown_session_error.rs` for the precedent of
//! one-binary-per-ticket with multiple `#[test]` functions.
//!
//! phux-3mtf adds two PTY-backed scenarios for the wire `cwd` field:
//!
//! 4. **Seed honors the wire cwd.** Under `seed_with_pty` with no
//!    server-wide override command, `CreateIfMissing { cwd: Some(dir) }`
//!    spawns the seed shell *in* `dir`, and the `ATTACHED` snapshot's
//!    pane carries it (spawn-time stamp + attach-time kernel refresh
//!    both resolve to `dir`).
//!
//! 5. **Invalid cwd falls back without failing the attach.** A wire
//!    `cwd` that does not name an existing directory is dropped: the
//!    attach still succeeds and the seed pane lands wherever a
//!    `cwd: None` spawn would have (the pre-3mtf behavior).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

mod common;

use phux_protocol::wire::frame::{
    AttachTarget, FrameKind, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN, ViewportInfo,
};
use tempfile::TempDir;

use crate::common::{
    SOCKET_CONNECT_DEADLINE, recv_typed, run_local, send_frame, spawn_server,
    spawn_server_seed_pty_no_cmd, wait_for_socket,
};

/// Canonical 80x24 viewport, matching the byc.6.* fixtures so snapshot
/// dimensions line up with the no-PTY seed actor's defaults.
fn create_if_missing_frame(name: &str) -> FrameKind {
    FrameKind::Attach {
        attach_id: 1,
        target: AttachTarget::CreateIfMissing {
            name: name.to_owned(),
            command: None,
            cwd: None,
        },
        viewport: ViewportInfo::new(80, 24),
        request_scrollback: false,
        scrollback_limit_lines: 0,
    }
}

/// `CreateIfMissing` carrying a wire `cwd` and a deterministic blocked
/// shell (`read _` is a builtin, so the child stays alive on the PTY in
/// whatever directory it was spawned in). Used by the phux-3mtf tests
/// against a PTY-backed server with no override command.
fn create_if_missing_with_cwd_frame(name: &str, cwd: &str) -> FrameKind {
    FrameKind::Attach {
        attach_id: 1,
        target: AttachTarget::CreateIfMissing {
            name: name.to_owned(),
            command: Some(vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "read _".to_owned(),
            ]),
            cwd: Some(cwd.to_owned()),
        },
        viewport: ViewportInfo::new(80, 24),
        request_scrollback: false,
        scrollback_limit_lines: 0,
    }
}

/// Pull the lone pane's `cwd` out of an `ATTACHED` frame, canonicalized
/// (the spawn-time stamp is the builder path while a live kernel refresh
/// may return an equivalent-but-distinct spelling, e.g. macOS's
/// /var -> /private/var).
fn attached_pane_cwd(attached: FrameKind) -> std::path::PathBuf {
    match attached {
        FrameKind::Attached { snapshot, .. } => {
            assert_eq!(snapshot.panes.len(), 1, "exactly one seed pane");
            let wire_cwd = snapshot.panes[0]
                .cwd
                .as_deref()
                .expect("ATTACHED pane must carry a cwd for a PTY-backed pane");
            let wire_path = std::path::PathBuf::from(wire_cwd);
            wire_path.canonicalize().unwrap_or(wire_path)
        }
        other => panic!("expected Attached, got {other:?}"),
    }
}

#[test]
fn create_if_missing_creates_session_when_absent() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        // No pre-seeded session: CreateIfMissing must fill the gap.
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &create_if_missing_frame("foo")).await;

        // ---- ATTACHED ----
        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "expected ATTACHED (got 0x{type_byte:02x})",
        );
        match attached {
            FrameKind::Attached {
                attach_id: _,
                snapshot,
                initial_client_id,
            } => {
                assert_eq!(snapshot.sessions.len(), 1, "exactly one session");
                assert_eq!(
                    snapshot.sessions[0].name, "foo",
                    "CreateIfMissing must create the named session",
                );
                assert_eq!(snapshot.windows.len(), 1, "one seed window");
                assert_eq!(snapshot.panes.len(), 1, "one seed pane");
                assert!(
                    initial_client_id.get() >= 1,
                    "initial_client_id must be allocated (monotonic from 1)",
                );
            }
            other => panic!("expected Attached, got {other:?}"),
        }

        // ---- TERMINAL_SNAPSHOT for the seed pane ----
        let (type_byte, snap_frame) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_BOOTSTRAP_BEGIN,
            "expected TERMINAL_SNAPSHOT after ATTACHED (got 0x{type_byte:02x})",
        );
        match snap_frame {
            FrameKind::BootstrapBegin { cols, rows, .. } => {
                // seed_session_with_actor seeds 80x24 — matches the byc.6.1
                // expectation. The dimension assertion is what proves the
                // seed pane actually got created (otherwise we'd never
                // reach this frame; see `prepare_attach` collecting
                // panes_to_snapshot from `session.windows`).
                assert_eq!(cols, 80, "seed pane defaults to 80 cols");
                assert_eq!(rows, 24, "seed pane defaults to 24 rows");
            }
            other => panic!("expected TerminalSnapshot, got {other:?}"),
        }

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}

/// phux-3mtf scenario 4: the wire `cwd` seeds the PTY child's working
/// directory, and the `ATTACHED` snapshot's pane reports it.
#[test]
fn create_if_missing_seeds_pane_in_wire_cwd() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        // PTY mode, no server-wide override command: the wire command
        // and cwd take effect.
        let (shutdown_tx, server_handle) = spawn_server_seed_pty_no_cmd(socket_path.clone(), None);

        // Canonicalize up front so the wire value, the spawn-time stamp,
        // and the kernel-reported cwd all agree in spelling.
        let cwd_dir = TempDir::new().unwrap();
        let cwd_path = cwd_dir.path().canonicalize().expect("canonicalize cwd");

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(
            &mut stream,
            &create_if_missing_with_cwd_frame("cwd-honored", &cwd_path.display().to_string()),
        )
        .await;

        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "expected ATTACHED (got 0x{type_byte:02x})",
        );
        assert_eq!(
            attached_pane_cwd(attached),
            cwd_path,
            "seed pane must start in the wire-supplied cwd",
        );
        // Drain the trailing TERMINAL_SNAPSHOT so the writer task isn't
        // blocked on its mailbox when the server tears down.
        let _ = recv_typed(&mut stream).await;

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}

/// phux-3mtf scenario 5: a wire `cwd` that is not an existing directory
/// is dropped — the attach still succeeds and the seed pane lands
/// wherever a `cwd: None` spawn would have (the pre-3mtf fallback).
#[test]
fn create_if_missing_invalid_cwd_falls_back_without_failing_attach() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server_seed_pty_no_cmd(socket_path.clone(), None);

        // A path guaranteed absent: inside a fresh tempdir, never created.
        let bogus = tmp.path().join("does-not-exist");
        assert!(!bogus.exists(), "fixture path must not exist");

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(
            &mut stream,
            &create_if_missing_with_cwd_frame("cwd-fallback", &bogus.display().to_string()),
        )
        .await;

        // The attach must succeed: ATTACHED, not ERROR. (Without the
        // validation gate, portable_pty's spawn fails on a nonexistent
        // cwd and the server replies with an ERROR frame instead.)
        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "invalid cwd must not fail the attach (got 0x{type_byte:02x})",
        );
        // The fallback directory is whatever the PTY spawn defaults to
        // when the builder carries no cwd (portable_pty picks the user's
        // home directory) — the contract under test is only that the
        // bogus path was NOT honored and the pane still landed in a real
        // directory, exactly as if the wire had sent `cwd: None`.
        let pane_cwd = attached_pane_cwd(attached);
        assert_ne!(pane_cwd, bogus, "the bogus cwd must not be honored");
        assert!(
            pane_cwd.is_dir(),
            "fallback cwd must be a real directory, got {}",
            pane_cwd.display(),
        );
        let _ = recv_typed(&mut stream).await;

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}

#[test]
fn create_if_missing_uses_default_name_string() {
    // The naked-phux dispatcher (`crates/phux/src/main.rs:46
    // DEFAULT_SESSION_NAME = "default"`) sends the literal string
    // `"default"` when no `-s NAME` is given. Pin the server's
    // round-trip on that exact value so a refactor of either side
    // notices the contract change.
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &create_if_missing_frame("default")).await;

        let (_type_byte, attached) = recv_typed(&mut stream).await;
        match attached {
            FrameKind::Attached { snapshot, .. } => {
                assert_eq!(snapshot.sessions.len(), 1);
                assert_eq!(
                    snapshot.sessions[0].name, "default",
                    "CreateIfMissing with the naked-phux default name must round-trip",
                );
            }
            other => panic!("expected Attached, got {other:?}"),
        }
        // Drain the trailing TERMINAL_SNAPSHOT so the writer task isn't
        // blocked on its mailbox when the server tears down.
        let _ = recv_typed(&mut stream).await;

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}

#[test]
fn create_if_missing_attaches_to_existing_session_without_duplicating() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        // Pre-seed `existing` so CreateIfMissing hits the fast path.
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("existing"));

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &create_if_missing_frame("existing")).await;

        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "expected ATTACHED (got 0x{type_byte:02x})",
        );
        match attached {
            FrameKind::Attached { snapshot, .. } => {
                // Critical: exactly ONE session named "existing". If the
                // server had created a second "existing" alongside the
                // pre-seed, the snapshot would carry two SessionInfo
                // entries (the registry has no name-uniqueness gate of
                // its own).
                assert_eq!(
                    snapshot.sessions.len(),
                    1,
                    "CreateIfMissing must not duplicate an existing session",
                );
                assert_eq!(snapshot.sessions[0].name, "existing");
                // And exactly the pre-seed's single window+pane — no
                // extra resources allocated.
                assert_eq!(snapshot.windows.len(), 1, "one window");
                assert_eq!(snapshot.panes.len(), 1, "one pane");
            }
            other => panic!("expected Attached, got {other:?}"),
        }
        let _ = recv_typed(&mut stream).await;

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}
