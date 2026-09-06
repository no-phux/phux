//! `phux-8uly` — a new session's **seed pane** announces itself with
//! `pane_spawned` (SPEC §7.1 event sourcing, ADR-0022 'events').
//!
//! Both `AgentEvent::PaneSpawned` broadcasts used to live inside
//! `handle_spawn_terminal`, which only ever runs for a pane *added to an
//! existing session*. The **first** pane of a *new* session is seeded by
//! `seed_session_with_*` and announced nothing, on either of the two paths
//! that create a session:
//!
//! * the attach `CreateIfMissing` path (`runtime/attach.rs`), and
//! * the headless `phux.session.create/v1` L3 write (`runtime/commands.rs`)
//!   — which is exactly how `phux new`, `phux worktree`, and an
//!   orchestrator create an agent session.
//!
//! The consequence is a hole in *push* coverage of the session lifecycle,
//! not merely a missing nicety. Death is covered (`pane_closed` reaches
//! every server-wide subscriber) and rename is covered (`METADATA_CHANGED`,
//! phux-q7ks); creation was not. A server-wide follower — ADR-0089's
//! fleet-inbox roster today, `phux agent wait --any` next — therefore could
//! not see a session that appeared *after* it subscribed, until the new
//! pane happened to emit something else (a `dirty`, a title) or the
//! follower re-polled.
//!
//! Both tests here drive the **production read loop** over a real UDS
//! connection rather than poking an actor mailbox, so what they pin is the
//! wire-observable contract a follower actually consumes.
//!
//! ## Why two connections, and why the barrier is a command round-trip
//!
//! Event fanout resolves its subscribers *at emit time*: an event emitted
//! before the subscription is installed is legitimately dropped, so a test
//! that raced the subscribe against the create would fail permanently
//! rather than flakily. The watcher therefore subscribes on its **own**
//! connection and the session is created on a **second** one, and the
//! watcher proves its subscription landed before the creating connection is
//! ever opened.
//!
//! That proof is a `GET_STATE` round-trip, never a sleep.
//! `SUBSCRIBE_EVENTS` carries no request id and is answered with no frame,
//! so `send_frame` returning proves only that the bytes left this end. The
//! per-connection frame loop handles frames in order, so a `COMMAND_RESULT`
//! for a request sent *after* the subscribe is proof the subscribe ahead of
//! it already ran — see the `agent_events.rs` module docs for the flake
//! family that motivated this shape.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{
    AgentEvent, AttachTarget, Command, CommandResult, FrameKind, SESSION_CREATE_KEY,
    SESSION_CREATE_RESULT_KEY, Scope, StateScope, TYPE_ATTACHED, ViewportInfo,
};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, recv_typed, run_local, send_frame,
    spawn_server_seed_pty_no_cmd, wait_for_socket,
};

/// How long a watcher waits for its `pane_spawned` before declaring the
/// announcement missing.
///
/// Deliberately *shorter* than [`WIRE_RECV_TIMEOUT`]: the testkit readers
/// panic with a generic "timed out waiting for frame" when that ambient
/// timeout expires, which would mask this test's own diagnosis of what went
/// wrong. Nothing on the happy path is load-bearing on this value — the
/// wait returns the instant the event lands — it only decides which message
/// a genuine regression prints.
const EVENT_WAIT_DEADLINE: Duration = Duration::from_secs(10);

/// A seed command that stays alive on its PTY without producing output.
///
/// `read _` is a POSIX shell builtin, so the child blocks on stdin
/// indefinitely. A pane that exited instead would reap the only session,
/// self-exit the server, and fail these tests with a socket error rather
/// than with anything about `pane_spawned`.
fn blocking_seed_command() -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), "read _".to_owned()]
}

/// Drain frames until the `COMMAND_RESULT` for `request_id` arrives.
async fn recv_command_result(stream: &mut UnixStream, request_id: u32) -> CommandResult {
    loop {
        let (_type_byte, frame) = recv_typed(stream).await;
        if let FrameKind::CommandResult {
            request_id: got,
            result,
        } = frame
            && got == request_id
        {
            return result;
        }
    }
}

/// `SUBSCRIBE_EVENTS { terminal: None }` plus the barrier proving the
/// server installed it.
///
/// `GET_STATE { scope: Server }` is the barrier command: read-only, so
/// waiting on it changes nothing else, and it needs neither an ATTACH nor a
/// terminal id — this watcher never attaches, exactly like `phux watch`.
/// Its `COMMAND_RESULT` cannot be produced before the frame loop has run
/// `handle_subscribe_events`, which registers the subscription
/// synchronously in `ServerState`.
async fn subscribe_server_wide(stream: &mut UnixStream, request_id: u32) {
    send_frame(stream, &FrameKind::SubscribeEvents { terminal: None }).await;
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
    let barrier = timeout(WIRE_RECV_TIMEOUT, recv_command_result(stream, request_id))
        .await
        .expect("the server must answer GET_STATE before the create is issued");
    assert!(
        !matches!(barrier, CommandResult::Error { .. }),
        "the subscribe barrier must succeed, got {barrier:?}",
    );
}

/// Drain `EVENT` frames until a `pane_spawned` arrives, returning the
/// Terminal id from its envelope, or `None` if `deadline` elapses first.
///
/// Non-`EVENT` frames are skipped: this asserts on the event stream
/// specifically, and a server-wide watcher legitimately sees other traffic.
/// `pane_spawned` carries an empty body, so the envelope id is the whole
/// payload — a follower that cannot tell *which* pane appeared has learned
/// nothing actionable, which is why the id is asserted rather than the tag
/// alone.
async fn await_pane_spawned(
    stream: &mut UnixStream,
    deadline: Duration,
) -> Option<Option<TerminalId>> {
    let end = tokio::time::Instant::now() + deadline;
    loop {
        let remaining = end.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let Ok((_type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            return None;
        };
        if let FrameKind::Event {
            terminal,
            event: AgentEvent::PaneSpawned,
        } = frame
        {
            return Some(terminal);
        }
    }
}

/// A session created **headlessly** — no ATTACH anywhere in the flow —
/// announces its seed pane to a server-wide event subscriber.
///
/// This is the `phux new` / `phux worktree` / orchestrator path: a
/// `SET_METADATA` write of `phux.session.create/v1` under `Scope::Global`.
/// The watcher is a separate, never-attached connection, so what this pins
/// is precisely the fleet-follower case — learning that a session exists
/// without having asked about it.
///
/// The event's envelope id is cross-checked against the seed-pane id the
/// server publishes under `phux.session.created/v1`, so the assertion is
/// that the *right* pane was announced, not merely that some event fired.
#[test]
fn headless_session_create_announces_its_seed_pane() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        // PTY-backed seeding with no server-wide override command, so the
        // wire `command` below takes effect — the shape a real `phux new`
        // meets.
        let (shutdown_tx, server_handle) = spawn_server_seed_pty_no_cmd(socket_path.clone(), None);

        // ---- watcher: subscribe server-wide, never attach ----
        let mut watcher = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        subscribe_server_wide(&mut watcher, 1).await;

        // ---- creator: a second connection creates the session ----
        let mut creator = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        let value = serde_json::to_vec(&serde_json::json!({
            "name": "scratch",
            "command": blocking_seed_command(),
            "cwd": serde_json::Value::Null,
        }))
        .unwrap();
        send_frame(
            &mut creator,
            &FrameKind::SetMetadata {
                request_id: 1,
                scope: Scope::Global,
                key: SESSION_CREATE_KEY.to_owned(),
                value,
            },
        )
        .await;

        // ---- the watcher must learn the session exists ----
        let announced = await_pane_spawned(&mut watcher, EVENT_WAIT_DEADLINE)
            .await
            .expect(
                "a server-wide subscriber must receive pane_spawned for a headlessly-created \
                 session's seed pane",
            );

        // Read the authoritative seed-pane id back and confirm the event
        // named that pane.
        send_frame(
            &mut creator,
            &FrameKind::GetMetadata {
                request_id: 2,
                scope: Scope::Global,
                key: SESSION_CREATE_RESULT_KEY.to_owned(),
            },
        )
        .await;
        let result_bytes = loop {
            let (_type_byte, frame) = recv_typed(&mut creator).await;
            if let FrameKind::MetadataValue {
                request_id: 2,
                value,
            } = frame
            {
                break value
                    .expect("the create result key must be present after a successful create");
            }
        };
        let json: serde_json::Value = serde_json::from_slice(&result_bytes).unwrap();
        let seed_pane = json
            .get("terminal_id")
            .and_then(serde_json::Value::as_u64)
            .and_then(|id| u32::try_from(id).ok())
            .map(TerminalId::local)
            .expect("the create result must carry a local terminal_id");
        assert_eq!(
            announced,
            Some(seed_pane),
            "pane_spawned must name the seed pane the create published",
        );

        drop(watcher);
        drop(creator);
        shutdown_tx.send(()).ok();
        timeout(phux_server_testkit::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// The attach `CreateIfMissing` path announces its seed pane too.
///
/// Same gap, second entrance: a client attaching to a session that does not
/// exist yet creates it, and a *different* client watching server-wide has
/// to hear about it. The attaching client's own `ATTACHED` snapshot tells it
/// what it created; nothing told anybody else.
#[test]
fn attach_create_if_missing_announces_its_seed_pane() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server_seed_pty_no_cmd(socket_path.clone(), None);

        let mut watcher = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        subscribe_server_wide(&mut watcher, 1).await;

        // ---- a second client attaches to a session that does not exist ----
        let mut joiner = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(
            &mut joiner,
            &FrameKind::Attach {
                attach_id: 1,
                target: AttachTarget::CreateIfMissing {
                    name: "made-on-attach".to_owned(),
                    command: Some(blocking_seed_command()),
                    cwd: None,
                },
                viewport: ViewportInfo::new(80, 24),
                request_scrollback: false,
                scrollback_limit_lines: 0,
            },
        )
        .await;
        let (type_byte, attached) = recv_typed(&mut joiner).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "first server-to-client frame must be ATTACHED",
        );
        let FrameKind::Attached { snapshot, .. } = attached else {
            panic!("expected Attached")
        };
        assert_eq!(snapshot.panes.len(), 1, "exactly one seed pane");
        let seed_pane = snapshot.panes[0].id.clone();

        let announced = await_pane_spawned(&mut watcher, EVENT_WAIT_DEADLINE)
            .await
            .expect(
                "a server-wide subscriber must receive pane_spawned when an attach creates a \
                 session",
            );
        assert_eq!(
            announced,
            Some(seed_pane),
            "pane_spawned must name the pane the attach seeded",
        );

        drop(watcher);
        drop(joiner);
        shutdown_tx.send(()).ok();
        timeout(phux_server_testkit::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}
