//! Wire-level integration tests for `AttachTarget::Last`.
//!
//! These tests exercise the L1 §8 server-resolved `Last` semantics
//! end-to-end:
//!
//! 1. `last_resolves_to_prior_attach`: ATTACH by name → success;
//!    then on a fresh connection, ATTACH with `Last` resolves to the
//!    same session and returns `ATTACHED`.
//! 2. `last_without_prior_touch_resolves_configured_seed`: a fresh
//!    server resolves `Last` to its configured seed, including a custom
//!    name.
//! 3. `last_with_no_live_session_returns_error`: a seedless server
//!    returns `ERROR { SessionNotFound }`; `Last` never creates.
//!
//! Sibling of `byc_6_1_attach_snapshot.rs`; intentionally a separate
//! binary so 6.1's snapshot assertions stay isolated.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use phux_protocol::input::focus::FocusEvent;
use phux_protocol::wire::frame::{
    AttachTarget, Command, CommandResult, CommandValue, ErrorCode, FrameKind, StateScope,
    TYPE_ATTACHED, TYPE_COMMAND_RESULT, TYPE_ERROR, ViewportInfo,
};
use tempfile::TempDir;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame, spawn_server,
    wait_for_socket,
};

/// Build an `ATTACH { Last }` with the same viewport/scrollback knobs
/// `attach_by_name` uses, so the two are otherwise wire-identical.
const fn attach_last() -> FrameKind {
    FrameKind::Attach {
        attach_id: 1,
        target: AttachTarget::Last,
        viewport: ViewportInfo::new(80, 24),
        request_scrollback: false,
        scrollback_limit_lines: 0,
    }
}

fn attach_create_if_missing(name: &str) -> FrameKind {
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

/// Drain the per-attach response sequence: ATTACHED then one
/// `TERMINAL_SNAPSHOT` per pane in the focused window. The server pre-seeds
/// exactly one pane, so we read both frames and assert types.
async fn drain_successful_attach(
    stream: &mut tokio::net::UnixStream,
    expected_name: &str,
) -> phux_protocol::ids::TerminalId {
    let (type_byte, frame) = recv_typed(stream).await;
    assert_eq!(
        type_byte, TYPE_ATTACHED,
        "first server-to-client frame must be ATTACHED (got 0x{type_byte:02x})",
    );
    let focused_pane = match frame {
        FrameKind::Attached { snapshot, .. } => {
            let focused = snapshot
                .sessions
                .iter()
                .find(|session| session.id == snapshot.focused_session)
                .expect("focused session must be listed in snapshot");
            assert_eq!(focused.name, expected_name, "focused session name");
            snapshot.focused_pane
        }
        other => panic!("expected FrameKind::Attached, got {other:?}"),
    };

    // One TERMINAL_SNAPSHOT follows. We don't inspect its body; the
    // round-trip / dim assertions live in byc_6_1. Just consume it so
    // subsequent reads see a clean stream boundary.
    let (_type_byte, _snap_frame) = recv_typed(stream).await;
    focused_pane
}

/// Poll `GET_STATE` until the server's most-recently-touched session — the
/// exact ordering `AttachTarget::Last` resolves against — is `expected_name`.
///
/// `INPUT_FOCUS` is fire-and-forget and, since ADR-0044, is applied via the
/// dedicated input lane (its own OS thread), so a `PING`/`PONG` round-trip on
/// the command lane does NOT prove the focus touch has landed: under parallel
/// load the `PONG` can overtake the touch and `Last` still resolves to the
/// last-attached session (the phux-mn3b flake). `GET_STATE` reads the touched
/// order itself (`handle_get_state` → `most_recently_touched_session`), so
/// polling it is the race-free flush.
async fn await_focus_touch(stream: &mut tokio::net::UnixStream, expected_name: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut request_id: u32 = 0xF0C0_0000;
    loop {
        request_id += 1;
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
        let focused_name = loop {
            let (type_byte, frame) = recv_typed(stream).await;
            if type_byte != TYPE_COMMAND_RESULT {
                continue;
            }
            if let FrameKind::CommandResult {
                request_id: got,
                result,
            } = frame
                && got == request_id
            {
                match result {
                    CommandResult::OkWith(CommandValue::State(snapshot)) => {
                        break snapshot
                            .sessions
                            .iter()
                            .find(|session| session.id == snapshot.focused_session)
                            .map(|session| session.name.clone());
                    }
                    other => panic!("GET_STATE must return Ok_With(State(..)), got {other:?}"),
                }
            }
        };
        if focused_name.as_deref() == Some(expected_name) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "focus touch for {expected_name:?} not observed via GET_STATE within deadline; \
             last focused: {focused_name:?}",
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

#[test]
fn last_resolves_to_prior_attach() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("default"));

        // First connection: ATTACH by name. This populates the
        // server-side last-touched session order.
        {
            let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
            send_frame(&mut stream, &attach_by_name("default")).await;
            drain_successful_attach(&mut stream, "default").await;
            drop(stream);
        }

        // Second connection from scratch: ATTACH with Last must
        // resolve to the same session and complete successfully.
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_last()).await;
        drain_successful_attach(&mut stream, "default").await;

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}

#[test]
fn last_resolves_to_most_recently_focused_not_last_attached() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("default"));

        let mut default_stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut default_stream, &attach_by_name("default")).await;
        let default_pane = drain_successful_attach(&mut default_stream, "default").await;

        let mut other_stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut other_stream, &attach_create_if_missing("other")).await;
        drain_successful_attach(&mut other_stream, "other").await;

        send_frame(
            &mut default_stream,
            &FrameKind::InputFocus {
                terminal_id: default_pane,
                event: FocusEvent::Gained,
            },
        )
        .await;
        await_focus_touch(&mut default_stream, "default").await;

        let mut last_stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut last_stream, &attach_last()).await;
        drain_successful_attach(&mut last_stream, "default").await;

        drop(last_stream);
        drop(other_stream);
        drop(default_stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}

#[test]
fn last_without_prior_touch_resolves_configured_seed() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        // This is the already-rendered result of a custom
        // `defaults.session-name-template`. The server config owns the
        // identity; the attaching client sends only `Last`.
        let configured_seed = "phux-custom-project";
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some(configured_seed));

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_last()).await;
        drain_successful_attach(&mut stream, configured_seed).await;

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}

#[test]
fn last_with_no_live_session_returns_error() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_last()).await;

        let (type_byte, frame) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_ERROR,
            "first frame must be ERROR (got 0x{type_byte:02x})",
        );
        match frame {
            FrameKind::Error {
                request_id,
                code,
                message,
            } => {
                assert!(
                    request_id.is_none(),
                    "ATTACH errors must not carry a request_id",
                );
                assert_eq!(
                    code,
                    ErrorCode::SessionNotFound,
                    "AttachTarget::Last with no live session must surface SessionNotFound",
                );
                assert!(
                    message.to_lowercase().contains("no live session"),
                    "error must diagnose the missing live session, got: {message:?}",
                );
            }
            other => panic!("expected FrameKind::Error, got {other:?}"),
        }

        // The refused lookup must leave the registry empty: `Last` carries no
        // creation authority, even when no session is available.
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 9,
                command: Command::GetState {
                    scope: StateScope::Server,
                },
            },
        )
        .await;
        let (type_byte, frame) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_COMMAND_RESULT);
        match frame {
            FrameKind::CommandResult {
                request_id: 9,
                result: CommandResult::OkWith(CommandValue::State(snapshot)),
            } => assert!(
                snapshot.sessions.is_empty(),
                "a refused Last attach must not create a fallback session",
            ),
            other => panic!("expected empty GET_STATE result, got {other:?}"),
        }

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}
