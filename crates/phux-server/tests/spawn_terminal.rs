//! `phux-4li.11` — Server-side SPAWN_TERMINAL handler + TERMINAL_CLOSED
//! emit + TERMINAL_RESIZE TIOCSWINSZ.
//!
//! Four scenarios pin the behavior:
//!
//! 1. **Spawn into the default Group.** A client sends
//!    `SPAWN_TERMINAL { group: DEFAULT, command: Some(/bin/cat) }`.
//!    The server replies `TERMINAL_SPAWNED { result: Ok(new_id) }`.
//!    A subsequent `INPUT_KEY { terminal_id: new_id, … }` round-trips
//!    via the freshly-spawned PTY's stdin → stdout, observable as
//!    `TERMINAL_OUTPUT { terminal_id: new_id, … }`.
//!
//! 2. **Spawn into an unknown Group.** A client sends
//!    `SPAWN_TERMINAL { group: GroupId::new(99999), … }`.
//!    The server replies `TERMINAL_SPAWNED { result:
//!    Err(GroupNotFound) }`.
//!
//! 3. **TERMINAL_CLOSED on PTY exit.** Spawn a Terminal running
//!    `sh -c 'exit 42'`. The PTY exits; the server emits
//!    `TERMINAL_CLOSED { terminal_id, exit_status: Some(42) }` to the
//!    subscribed (spawning) client.
//!
//! 4. **TERMINAL_RESIZE.** Spawn a Terminal, send `TERMINAL_RESIZE {
//!    terminal_id, cols: 120, rows: 40 }`. Detach + reattach via a
//!    second connection; the new `TERMINAL_SNAPSHOT` for the same
//!    pane reports the post-resize dims. Verifying via re-attach
//!    rather than waiting for an inline snapshot keeps the test on
//!    the public wire surface (the registry's `dims` field is what
//!    the snapshot pipeline reads).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(
    clippy::doc_markdown,
    reason = "test-only file; the module/comment narrative uses bare wire-frame names (SPAWN_TERMINAL, TERMINAL_CLOSED, …) the way the integration tests above do for symmetry"
)]

mod common;

use std::time::Duration;

use phux_protocol::ids::GroupId;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, Scope, SpawnError, SpawnResult, StateScope,
    TERMINAL_AGENT_SESSION_KEY, TYPE_COMMAND_RESULT, TYPE_METADATA_VALUE, TYPE_TERMINAL_CLOSED,
    TYPE_TERMINAL_OUTPUT, TYPE_TERMINAL_SPAWNED,
};
use phux_server::DEFAULT_GROUP_ID;
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use crate::common::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server, spawn_server_seed_pty_no_cmd, spawn_server_with_seed_cmd,
    spawn_server_with_seed_cmd_and_cwd_mode, wait_for_socket,
};

/// Drain frames until a `TERMINAL_SPAWNED` arrives whose `request_id`
/// matches `request_id`. Other frames (TERMINAL_OUTPUT bursts from the
/// fresh PTY, METADATA_CHANGED noise, etc.) are silently consumed.
async fn await_terminal_spawned(stream: &mut UnixStream, request_id: u32) -> SpawnResult {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Ok((type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if type_byte != TYPE_TERMINAL_SPAWNED {
            continue;
        }
        if let FrameKind::TerminalSpawned {
            request_id: got,
            result,
        } = frame
            && got == request_id
        {
            return result;
        }
    }
    panic!("timed out waiting for TERMINAL_SPAWNED request_id={request_id}");
}

async fn await_metadata_value(stream: &mut UnixStream, request_id: u32) -> Option<Vec<u8>> {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Ok((type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if type_byte != TYPE_METADATA_VALUE {
            continue;
        }
        if let FrameKind::MetadataValue {
            request_id: got,
            value,
        } = frame
            && got == request_id
        {
            return value;
        }
    }
    panic!("timed out waiting for METADATA_VALUE request_id={request_id}");
}

/// Drain until the accumulated TERMINAL_OUTPUT bytes for `pane`
/// contain `needle`, or the timeout fires. Mirrors `input_dispatch.rs`'s
/// `await_echo` but pane-scoped so other panes' output is ignored.
async fn await_echo_on(
    stream: &mut UnixStream,
    pane: &phux_protocol::ids::TerminalId,
    needle: u8,
) -> Vec<u8> {
    let mut acc: Vec<u8> = Vec::new();
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Ok((type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if type_byte != TYPE_TERMINAL_OUTPUT {
            continue;
        }
        if let FrameKind::TerminalOutput {
            terminal_id, bytes, ..
        } = frame
            && &terminal_id == pane
        {
            acc.extend_from_slice(&bytes);
            if acc.contains(&needle) {
                return acc;
            }
        }
    }
    acc
}

/// Drain until a `TERMINAL_CLOSED` for `pane` arrives, or the timeout
/// fires. Other frames are ignored.
async fn await_terminal_closed(
    stream: &mut UnixStream,
    pane: &phux_protocol::ids::TerminalId,
) -> Option<i32> {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Ok((type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if type_byte != TYPE_TERMINAL_CLOSED {
            continue;
        }
        if let FrameKind::TerminalClosed {
            terminal_id,
            exit_status,
        } = frame
            && &terminal_id == pane
        {
            return exit_status;
        }
    }
    panic!("timed out waiting for TERMINAL_CLOSED for {pane:?}");
}

/// `KeyEvent` for an ASCII printable. Matches `input_dispatch.rs`'s
/// `ascii_key` fixture so the wire encoding is identical.
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

/// Enter key — no `text`, libghostty's encoder synthesizes the CR.
const fn enter_key() -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::Enter,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    }
}

/// Build an `ATTACH { CreateIfMissing(name) }` frame so the test client
/// gets attached state (and thus an outbound mailbox) before sending
/// SPAWN_TERMINAL. Without an attached slot the auto-subscribe path in
/// `handle_spawn_terminal` skips the new pane, the spawning client
/// would not receive its own TERMINAL_OUTPUT, and the round-trip
/// assertion in scenario 1 could not be made.
fn attach_create_if_missing(name: &str) -> FrameKind {
    use phux_protocol::wire::frame::{AttachTarget, ViewportInfo};
    FrameKind::Attach {
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

/// Spawn a server, attach the client, and consume the initial
/// `ATTACHED` + `TERMINAL_SNAPSHOT` frames so subsequent `recv_typed`
/// calls only see test-driven traffic. Returns the stream + shutdown
/// channel + server handle.
async fn spawn_and_attach(
    tmp: &TempDir,
    session_name: &str,
) -> (
    UnixStream,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<(), phux_server::ServerError>>,
) {
    use phux_protocol::wire::frame::{TYPE_ATTACHED, TYPE_TERMINAL_SNAPSHOT};

    let socket_path = tmp.path().join("phux.sock");
    let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);
    let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(&mut stream, &attach_create_if_missing(session_name)).await;
    // ATTACHED
    let (type_byte, _attached) = recv_typed(&mut stream).await;
    assert_eq!(type_byte, TYPE_ATTACHED, "expected ATTACHED");
    // TERMINAL_SNAPSHOT for the seed pane
    let (type_byte, _snap) = recv_typed(&mut stream).await;
    assert_eq!(
        type_byte, TYPE_TERMINAL_SNAPSHOT,
        "expected TERMINAL_SNAPSHOT",
    );
    (stream, shutdown_tx, server_handle)
}

#[test]
fn spawn_terminal_in_default_group_round_trips_input() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut stream, shutdown_tx, server_handle) = spawn_and_attach(&tmp, "default").await;
        let agent_session =
            br#"{"plugin_id":"com.phux.agents","integration_id":"codex","native_id":"session-42"}"#
                .to_vec();

        // SPAWN_TERMINAL with /bin/cat — cooked-mode echo fixture from
        // input_dispatch.rs. cat echoes the input back through the PTY
        // so we can prove the spawning client is wired to the new pane.
        send_frame(
            &mut stream,
            &FrameKind::SpawnTerminal {
                request_id: 42,
                group: DEFAULT_GROUP_ID,
                command: Some(vec!["/bin/cat".to_owned()]),
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: Some(agent_session.clone()),
            },
        )
        .await;

        // Reply must carry our request_id and an Ok TerminalId.
        let result = await_terminal_spawned(&mut stream, 42).await;
        let new_id = match result {
            SpawnResult::Ok(id) => id,
            SpawnResult::Err(e) => panic!("expected Ok, got Err({e:?})"),
            other => panic!("unexpected SpawnResult variant: {other:?}"),
        };
        assert!(
            new_id.is_local(),
            "freshly spawned TerminalId must be LOCAL (got {new_id:?})",
        );

        send_frame(
            &mut stream,
            &FrameKind::GetMetadata {
                request_id: 43,
                scope: Scope::Terminal(new_id.clone()),
                key: TERMINAL_AGENT_SESSION_KEY.to_owned(),
            },
        )
        .await;
        assert_eq!(
            await_metadata_value(&mut stream, 43).await,
            Some(agent_session),
            "SPAWN_TERMINAL must publish native resume provenance with the new pane",
        );

        // INPUT_KEY('a') + Enter through the new pane.
        send_frame(
            &mut stream,
            &FrameKind::InputKey {
                terminal_id: new_id.clone(),
                event: ascii_key('a', PhysicalKey::A),
            },
        )
        .await;
        send_frame(
            &mut stream,
            &FrameKind::InputKey {
                terminal_id: new_id.clone(),
                event: enter_key(),
            },
        )
        .await;

        // cat echoes the typed byte back through the PTY → broadcast →
        // outbound pump → TERMINAL_OUTPUT for `new_id`.
        let acc = await_echo_on(&mut stream, &new_id, b'a').await;
        assert!(
            acc.contains(&b'a'),
            "INPUT_KEY('a') to spawned pane must round-trip through PTY (got {} bytes: {:?})",
            acc.len(),
            acc,
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

#[test]
fn spawn_terminal_rejects_invalid_agent_session_provenance() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut stream, shutdown_tx, server_handle) =
            spawn_and_attach(&tmp, "invalid-provenance").await;

        for (request_id, agent_session) in [(50, Vec::new()), (51, vec![b'x'; 4097])] {
            send_frame(
                &mut stream,
                &FrameKind::SpawnTerminal {
                    request_id,
                    group: DEFAULT_GROUP_ID,
                    command: Some(vec!["/bin/cat".to_owned()]),
                    cwd: None,
                    env: None,
                    term: None,
                    satellite: None,
                    owner_terminal: None,
                    agent_session: Some(agent_session),
                },
            )
            .await;
            let result = await_terminal_spawned(&mut stream, request_id).await;
            assert!(
                matches!(
                    &result,
                    SpawnResult::Err(SpawnError::SpawnFailed(reason))
                        if reason.contains("1..=4096")
                ),
                "request {request_id} must reject invalid provenance, got {result:?}",
            );
        }

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

#[test]
fn failed_actor_build_reaps_atomic_agent_session_provenance() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut stream, shutdown_tx, server_handle) =
            spawn_and_attach(&tmp, "build-failure").await;
        send_frame(
            &mut stream,
            &FrameKind::SpawnTerminal {
                request_id: 60,
                group: DEFAULT_GROUP_ID,
                command: Some(vec!["/definitely/not/a/phux-test-program".to_owned()]),
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: Some(br#"{"native_id":"never-live"}"#.to_vec()),
            },
        )
        .await;
        assert!(
            matches!(
                await_terminal_spawned(&mut stream, 60).await,
                SpawnResult::Err(SpawnError::SpawnFailed(_))
            ),
            "an unspawnable child must fail the request",
        );

        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 61,
                command: Command::GetState {
                    scope: StateScope::Server,
                },
            },
        )
        .await;
        let snapshot = match await_command_result(&mut stream, 61).await {
            CommandResult::OkWith(CommandValue::State(snapshot)) => snapshot,
            other => panic!("expected state after failed spawn, got {other:?}"),
        };
        assert_eq!(
            snapshot.panes.len(),
            1,
            "failed spawn must not leave an actorless Terminal or its metadata",
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

#[test]
fn explicit_owner_terminal_selects_exact_session_window() {
    run_local(async {
        use phux_protocol::wire::frame::{TYPE_ATTACHED, TYPE_TERMINAL_SNAPSHOT};

        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);

        let mut first = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut first, &attach_create_if_missing("first")).await;
        let (kind, attached) = recv_typed(&mut first).await;
        assert_eq!(kind, TYPE_ATTACHED);
        let owner = match attached {
            FrameKind::Attached { snapshot, .. } => snapshot.focused_pane,
            other => panic!("expected Attached, got {other:?}"),
        };
        assert_eq!(recv_typed(&mut first).await.0, TYPE_TERMINAL_SNAPSHOT);

        let mut second = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut second, &attach_create_if_missing("second")).await;
        assert_eq!(recv_typed(&mut second).await.0, TYPE_ATTACHED);
        assert_eq!(recv_typed(&mut second).await.0, TYPE_TERMINAL_SNAPSHOT);

        let mut headless = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(
            &mut headless,
            &FrameKind::SpawnTerminal {
                request_id: 50,
                group: DEFAULT_GROUP_ID,
                command: Some(vec!["/bin/cat".to_owned()]),
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: Some(owner.clone()),
                agent_session: None,
            },
        )
        .await;
        let spawned = match await_terminal_spawned(&mut headless, 50).await {
            SpawnResult::Ok(id) => id,
            other => panic!("expected Ok, got {other:?}"),
        };
        send_frame(
            &mut headless,
            &FrameKind::Command {
                request_id: 51,
                command: Command::GetState {
                    scope: StateScope::Server,
                },
            },
        )
        .await;
        let snapshot = match await_command_result(&mut headless, 51).await {
            CommandResult::OkWith(CommandValue::State(snapshot)) => snapshot,
            other => panic!("expected state, got {other:?}"),
        };
        let owner_window = snapshot
            .panes
            .iter()
            .find(|p| p.id == owner)
            .unwrap()
            .window_id;
        let spawned_window = snapshot
            .panes
            .iter()
            .find(|p| p.id == spawned)
            .unwrap()
            .window_id;
        assert_eq!(
            spawned_window, owner_window,
            "ownership target must win over the most recently active second session"
        );

        drop((first, second, headless));
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// Drain frames until a `COMMAND_RESULT` matching `request_id` arrives.
async fn await_command_result(stream: &mut UnixStream, request_id: u32) -> CommandResult {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Ok((type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if type_byte != TYPE_COMMAND_RESULT {
            continue;
        }
        if let FrameKind::CommandResult {
            request_id: got,
            result,
        } = frame
            && got == request_id
        {
            return result;
        }
    }
    panic!("no COMMAND_RESULT for request_id {request_id} within deadline");
}

/// phux-i9zl: a split (`SPAWN_TERMINAL` from an attached client) must land
/// the new pane in the client's CURRENT session's window — NOT a fresh
/// `spawn-N` session. Regression guard for the live bug where `phux ls`
/// showed two sessions after one split (and the split pane was orphaned in
/// a session the client never reattached to).
#[test]
fn spawn_terminal_lands_in_attached_session_not_a_new_session() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut stream, shutdown_tx, server_handle) = spawn_and_attach(&tmp, "default").await;

        send_frame(
            &mut stream,
            &FrameKind::SpawnTerminal {
                request_id: 7,
                group: DEFAULT_GROUP_ID,
                command: Some(vec!["/bin/cat".to_owned()]),
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
            },
        )
        .await;
        let new_id = match await_terminal_spawned(&mut stream, 7).await {
            SpawnResult::Ok(id) => id,
            other => panic!("expected Ok, got {other:?}"),
        };

        // Server-scope GET_STATE: exactly ONE session, and the spawned pane
        // lives in it alongside the seed pane.
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 8,
                command: Command::GetState {
                    scope: StateScope::Server,
                },
            },
        )
        .await;
        match await_command_result(&mut stream, 8).await {
            CommandResult::OkWith(CommandValue::State(snapshot)) => {
                let names: Vec<&str> = snapshot.sessions.iter().map(|s| s.name.as_str()).collect();
                assert_eq!(
                    snapshot.sessions.len(),
                    1,
                    "split must NOT create a second session; got {names:?}",
                );
                assert_eq!(names, vec!["default"], "the one session is still 'default'");
                assert_eq!(
                    snapshot.panes.len(),
                    2,
                    "the seed pane + the spawned pane both live in the session",
                );
                assert!(
                    snapshot.panes.iter().any(|p| p.id == new_id),
                    "the spawned pane id must appear in the session snapshot",
                );
            }
            other => panic!("expected Ok_With(State(..)), got {other:?}"),
        }

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// phux-ign Part 2: a `SPAWN_TERMINAL` whose wire `env` carries a `TERM`
/// entry MUST have that value reach the spawned PTY, overriding the
/// server's `defaults.term` baseline. The wire frame is authoritative for
/// the Terminal it creates.
///
/// The spawned command prints `$TERM` and blocks; the value comes back as
/// TERMINAL_OUTPUT. The override is a sentinel string distinct from any
/// real terminfo entry so the assertion can't pass by accident.
#[test]
fn spawn_terminal_env_term_overrides_default() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut stream, shutdown_tx, server_handle) = spawn_and_attach(&tmp, "default").await;

        send_frame(
            &mut stream,
            &FrameKind::SpawnTerminal {
                request_id: 7,
                group: DEFAULT_GROUP_ID,
                command: Some(vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    // `read _` (a builtin) blocks so the pane stays alive
                    // after printing; `exec read _` would die immediately.
                    "printf 'TERMIS=%s\\n' \"$TERM\"; read _".to_owned(),
                ]),
                cwd: None,
                env: Some(vec![("TERM".to_owned(), "phux-spawn-override".to_owned())]),
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
            },
        )
        .await;

        let new_id = match await_terminal_spawned(&mut stream, 7).await {
            SpawnResult::Ok(id) => id,
            other => panic!("SPAWN_TERMINAL did not succeed: {other:?}"),
        };

        let needle = b"TERMIS=phux-spawn-override";
        let acc = await_output_contains(&mut stream, &new_id, needle).await;
        let body = String::from_utf8_lossy(&acc);
        assert!(
            acc.windows(needle.len()).any(|w| w == needle),
            "spawn-supplied TERM must override the default; got output: {body:?}",
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// phux-ign Part 2: a `SPAWN_TERMINAL` whose wire `env` does NOT carry
/// `TERM` (here `env = None`) falls back to the server's `defaults.term`.
/// The test server runs with the schema default, so the spawned pane sees
/// `TERM=xterm-256color` (the safe baseline, phux-7vx). This is the
/// load-bearing assertion that the config default actually flows to the
/// PTY env — a regression here would silently change the advertised
/// terminfo for every spawned pane.
#[test]
fn spawn_terminal_default_term_is_xterm_256color() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut stream, shutdown_tx, server_handle) = spawn_and_attach(&tmp, "default").await;

        send_frame(
            &mut stream,
            &FrameKind::SpawnTerminal {
                request_id: 8,
                group: DEFAULT_GROUP_ID,
                command: Some(vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    "printf 'TERMIS=%s\\n' \"$TERM\"; read _".to_owned(),
                ]),
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
            },
        )
        .await;

        let new_id = match await_terminal_spawned(&mut stream, 8).await {
            SpawnResult::Ok(id) => id,
            other => panic!("SPAWN_TERMINAL did not succeed: {other:?}"),
        };

        let needle = b"TERMIS=xterm-256color";
        let acc = await_output_contains(&mut stream, &new_id, needle).await;
        let body = String::from_utf8_lossy(&acc);
        assert!(
            acc.windows(needle.len()).any(|w| w == needle),
            "spawn with env=None must inherit defaults.term (xterm-256color); \
             got output: {body:?}",
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// phux-ign: the first-class `SPAWN_TERMINAL.term` field overrides the
/// server's `defaults.term` baseline, reaching the spawned PTY's `TERM`.
/// This is the typed per-spawn knob — distinct from hand-rolling a `TERM`
/// env pair. The sentinel value can't match any real terminfo entry, so
/// the assertion can't pass by accident.
#[test]
fn spawn_terminal_term_field_overrides_default() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut stream, shutdown_tx, server_handle) = spawn_and_attach(&tmp, "default").await;

        send_frame(
            &mut stream,
            &FrameKind::SpawnTerminal {
                request_id: 21,
                group: DEFAULT_GROUP_ID,
                command: Some(vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    "printf 'TERMIS=%s\\n' \"$TERM\"; read _".to_owned(),
                ]),
                cwd: None,
                env: None,
                term: Some("phux-term-field".to_owned()),
                satellite: None,
                owner_terminal: None,
                agent_session: None,
            },
        )
        .await;

        let new_id = match await_terminal_spawned(&mut stream, 21).await {
            SpawnResult::Ok(id) => id,
            other => panic!("SPAWN_TERMINAL did not succeed: {other:?}"),
        };

        let needle = b"TERMIS=phux-term-field";
        let acc = await_output_contains(&mut stream, &new_id, needle).await;
        let body = String::from_utf8_lossy(&acc);
        assert!(
            acc.windows(needle.len()).any(|w| w == needle),
            "spawn `term` field must override defaults.term; got output: {body:?}",
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// phux-ign: a bare `env` entry for `TERM` still wins over the first-class
/// `term` field — `env` is the lowest, most explicit tier (applied last on
/// the server). This pins the documented precedence so the field doesn't
/// silently shadow an explicit env override.
#[test]
fn spawn_terminal_env_term_beats_term_field() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut stream, shutdown_tx, server_handle) = spawn_and_attach(&tmp, "default").await;

        send_frame(
            &mut stream,
            &FrameKind::SpawnTerminal {
                request_id: 22,
                group: DEFAULT_GROUP_ID,
                command: Some(vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    "printf 'TERMIS=%s\\n' \"$TERM\"; read _".to_owned(),
                ]),
                cwd: None,
                env: Some(vec![("TERM".to_owned(), "phux-env-wins".to_owned())]),
                term: Some("phux-term-field".to_owned()),
                satellite: None,
                owner_terminal: None,
                agent_session: None,
            },
        )
        .await;

        let new_id = match await_terminal_spawned(&mut stream, 22).await {
            SpawnResult::Ok(id) => id,
            other => panic!("SPAWN_TERMINAL did not succeed: {other:?}"),
        };

        let needle = b"TERMIS=phux-env-wins";
        let acc = await_output_contains(&mut stream, &new_id, needle).await;
        let body = String::from_utf8_lossy(&acc);
        assert!(
            acc.windows(needle.len()).any(|w| w == needle),
            "wire env TERM must beat the `term` field; got output: {body:?}",
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

#[test]
fn spawn_terminal_unknown_group_returns_group_not_found() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut stream, shutdown_tx, server_handle) = spawn_and_attach(&tmp, "default").await;

        // GroupId::new(99999) — any non-default id MUST surface
        // SpawnError::GroupNotFound per SPEC §7.4's L2-dependency
        // note and the wire frame's doc.
        send_frame(
            &mut stream,
            &FrameKind::SpawnTerminal {
                request_id: 7,
                group: GroupId::new(99_999),
                command: None,
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
            },
        )
        .await;

        let result = await_terminal_spawned(&mut stream, 7).await;
        match result {
            SpawnResult::Err(SpawnError::GroupNotFound) => {}
            other => panic!("expected Err(GroupNotFound), got {other:?}"),
        }
        // SAFETY note for the future reader: SpawnError is
        // #[non_exhaustive] so the outer SpawnResult::Err arm above
        // catches both GroupNotFound and any v0.2.x additions —
        // `other` covers both unknown SpawnResult variants and
        // unknown SpawnError variants nested inside Err.

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

#[test]
fn spawn_terminal_emits_terminal_closed_on_pty_exit() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut stream, shutdown_tx, server_handle) = spawn_and_attach(&tmp, "default").await;

        // sh -c 'exit 42' is portable (BSD, Linux, macOS) and produces
        // a deterministic exit code we can assert on the wire.
        send_frame(
            &mut stream,
            &FrameKind::SpawnTerminal {
                request_id: 1,
                group: DEFAULT_GROUP_ID,
                command: Some(vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    "exit 42".to_owned(),
                ]),
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
            },
        )
        .await;

        let result = await_terminal_spawned(&mut stream, 1).await;
        let new_id = match result {
            SpawnResult::Ok(id) => id,
            SpawnResult::Err(e) => panic!("expected Ok, got Err({e:?})"),
            other => panic!("unexpected SpawnResult variant: {other:?}"),
        };

        // The child exits almost immediately; the PTY EOF watcher
        // fires the exit_notify oneshot which drives the
        // TERMINAL_CLOSED broadcast. WIRE_RECV_TIMEOUT (5s) is the
        // budget.
        let exit_status = await_terminal_closed(&mut stream, &new_id).await;
        assert_eq!(
            exit_status,
            Some(42),
            "TERMINAL_CLOSED exit_status must be Some(42) for `sh -c 'exit 42'`",
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

#[test]
fn terminal_resize_updates_pane_dims_observable_on_reattach() {
    run_local(async {
        use phux_protocol::wire::frame::{
            AttachTarget, TYPE_ATTACHED, TYPE_TERMINAL_SNAPSHOT, ViewportInfo,
        };

        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);

        // First client: create the session, spawn a terminal, resize it.
        let mut stream_a = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream_a, &attach_create_if_missing("resize-test")).await;
        let (type_byte, _attached) = recv_typed(&mut stream_a).await;
        assert_eq!(type_byte, TYPE_ATTACHED);
        let (type_byte, _snap) = recv_typed(&mut stream_a).await;
        assert_eq!(type_byte, TYPE_TERMINAL_SNAPSHOT);

        // Use /bin/cat so the actor stays alive for the duration of the
        // resize round-trip. A short-lived command would race with the
        // resize ioctl (the actor could already be tearing down by the
        // time TERMINAL_RESIZE arrives).
        send_frame(
            &mut stream_a,
            &FrameKind::SpawnTerminal {
                request_id: 99,
                group: DEFAULT_GROUP_ID,
                command: Some(vec!["/bin/cat".to_owned()]),
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
            },
        )
        .await;
        let result = await_terminal_spawned(&mut stream_a, 99).await;
        let new_id = match result {
            SpawnResult::Ok(id) => id,
            SpawnResult::Err(e) => panic!("expected Ok, got Err({e:?})"),
            other => panic!("unexpected SpawnResult variant: {other:?}"),
        };

        // Send TERMINAL_RESIZE with non-default dims (the spawn defaults
        // to 80x24; assert the resize is observable as the *changed*
        // dims, not the original).
        send_frame(
            &mut stream_a,
            &FrameKind::TerminalResize {
                terminal_id: new_id.clone(),
                cols: 120,
                rows: 40,
            },
        )
        .await;

        // The resize is fire-and-forget (no reply on the wire). Give
        // the actor a tick to process the mailbox before we observe
        // the effect via re-attach. The resize handler calls
        // `try_send` synchronously inside a `with_mut`, so the only
        // async hop is the actor pulling from its resize_rx.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Open a second connection and ATTACH ByName to the same
        // session. The new TERMINAL_SNAPSHOT frames will report the
        // post-resize dims (the registry's `dims` field is what
        // `build_session_snapshot` reads — see state.rs).
        let mut stream_b = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(
            &mut stream_b,
            &FrameKind::Attach {
                target: AttachTarget::ByName("resize-test".to_owned()),
                viewport: ViewportInfo::new(80, 24),
                request_scrollback: false,
                scrollback_limit_lines: 0,
            },
        )
        .await;
        // ATTACHED
        let (type_byte, attached) = recv_typed(&mut stream_b).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "second client must see ATTACHED for resize-test session",
        );
        // `SessionSnapshot.panes` aggregates panes across ALL sessions
        // (resize-test session in this test), so filter by the
        // spawned terminal id rather than asserting on the slice's
        // length. The id is what the client correlates across the wire
        // in any case.
        let panes = match attached {
            FrameKind::Attached { snapshot, .. } => snapshot.panes,
            other => panic!("expected Attached, got {other:?}"),
        };
        let spawned = panes.iter().find(|p| p.id == new_id).unwrap_or_else(|| {
            panic!(
                "the spawned pane (id={new_id:?}) missing from re-attach snapshot \
                     (got {} panes: ids={:?})",
                panes.len(),
                panes.iter().map(|p| p.id.clone()).collect::<Vec<_>>(),
            )
        });
        assert_eq!(
            spawned.cols, 120,
            "the spawned pane must report post-resize cols (120), got {}",
            spawned.cols,
        );
        assert_eq!(
            spawned.rows, 40,
            "the spawned pane must report post-resize rows (40), got {}",
            spawned.rows,
        );

        drop(stream_a);
        drop(stream_b);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// Drain until the accumulated TERMINAL_OUTPUT bytes for `pane` contain
/// `needle` (a byte sequence), or the timeout fires. Mirrors
/// `await_echo_on` but matches a multi-byte subsequence.
async fn await_output_contains(
    stream: &mut UnixStream,
    pane: &phux_protocol::ids::TerminalId,
    needle: &[u8],
) -> Vec<u8> {
    let mut acc: Vec<u8> = Vec::new();
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Ok((type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if type_byte != TYPE_TERMINAL_OUTPUT {
            continue;
        }
        if let FrameKind::TerminalOutput {
            terminal_id, bytes, ..
        } = frame
            && &terminal_id == pane
        {
            acc.extend_from_slice(&bytes);
            if acc.windows(needle.len()).any(|w| w == needle) {
                return acc;
            }
        }
    }
    acc
}

/// Drain until the accumulated bytes for `pane` contain `needle`, scanning
/// BOTH `TERMINAL_OUTPUT` (live deltas) and `TERMINAL_SNAPSHOT`
/// (`vt_replay_bytes` — the rendered grid). A seed pane that printed before
/// the client attached surfaces its output in the snapshot replay rather
/// than a live delta, so a test observing a pre-attach print must read both.
async fn await_snapshot_or_output_contains(
    stream: &mut UnixStream,
    pane: &phux_protocol::ids::TerminalId,
    needle: &[u8],
) -> Vec<u8> {
    let mut acc: Vec<u8> = Vec::new();
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Ok((_type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        match frame {
            FrameKind::TerminalOutput {
                terminal_id, bytes, ..
            } if &terminal_id == pane => acc.extend_from_slice(&bytes),
            FrameKind::TerminalSnapshot {
                terminal_id,
                vt_replay_bytes,
                ..
            } if &terminal_id == pane => acc.extend_from_slice(&vt_replay_bytes),
            _ => continue,
        }
        if acc.windows(needle.len()).any(|w| w == needle) {
            return acc;
        }
    }
    acc
}

/// phux-w7mj: the server injects `PHUX_TERMINAL_ID` (the pane's own local
/// wire id) into every SPAWN_TERMINAL child, so an in-pane process — e.g.
/// the agent-record wrapper — self-targets on the wire with zero config.
/// A child that echoes `$PHUX_TERMINAL_ID` MUST report exactly the local id
/// the `TERMINAL_SPAWNED` reply carried. This is the split-into-session
/// (`spawn_pane_with_pty`) path.
#[test]
fn spawn_terminal_injects_matching_terminal_id_env() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut stream, shutdown_tx, server_handle) = spawn_and_attach(&tmp, "default").await;

        send_frame(
            &mut stream,
            &FrameKind::SpawnTerminal {
                request_id: 55,
                group: DEFAULT_GROUP_ID,
                command: Some(vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    // Trailing `.` so `PTID=1.` can't match a prefix of a
                    // longer id (`PTID=12`). `read _` blocks so the pane
                    // stays alive after printing.
                    "printf 'PTID=%s.\\n' \"$PHUX_TERMINAL_ID\"; read _".to_owned(),
                ]),
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
            },
        )
        .await;

        let new_id = match await_terminal_spawned(&mut stream, 55).await {
            SpawnResult::Ok(id) => id,
            other => panic!("SPAWN_TERMINAL did not succeed: {other:?}"),
        };
        let local = new_id
            .local_id()
            .expect("a freshly spawned id must be LOCAL");
        let needle = format!("PTID={local}.");
        let acc = await_output_contains(&mut stream, &new_id, needle.as_bytes()).await;
        let body = String::from_utf8_lossy(&acc);
        assert!(
            acc.windows(needle.len()).any(|w| w == needle.as_bytes()),
            "spawned child must see PHUX_TERMINAL_ID={local} matching its wire id; \
             got output: {body:?}",
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// phux-w7mj: the same `PHUX_TERMINAL_ID` injection covers the seed /
/// attach-create path (`seed_session_with_pty`). A `CreateIfMissing` whose
/// wire `command` echoes `$PHUX_TERMINAL_ID` MUST report the seed pane's own
/// wire id — the id the ATTACHED snapshot advertises for that pane.
#[test]
fn attach_create_seed_pane_injects_matching_terminal_id_env() {
    use phux_protocol::wire::frame::{AttachTarget, TYPE_ATTACHED, ViewportInfo};

    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        // Seed attach-create panes with a real PTY and NO server-wide
        // override command, so the wire `command` below runs in the
        // freshly-created seed pane.
        let (shutdown_tx, server_handle) = spawn_server_seed_pty_no_cmd(socket_path.clone(), None);
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        send_frame(
            &mut stream,
            &FrameKind::Attach {
                target: AttachTarget::CreateIfMissing {
                    name: "seed-id".to_owned(),
                    command: Some(vec![
                        "/bin/sh".to_owned(),
                        "-c".to_owned(),
                        "printf 'PTID=%s.\\n' \"$PHUX_TERMINAL_ID\"; read _".to_owned(),
                    ]),
                    cwd: None,
                },
                viewport: ViewportInfo::new(80, 24),
                request_scrollback: false,
                scrollback_limit_lines: 0,
            },
        )
        .await;

        // ATTACHED carries the seed pane's wire id.
        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED, "expected ATTACHED");
        let seed_id = match attached {
            FrameKind::Attached { snapshot, .. } => {
                assert_eq!(snapshot.panes.len(), 1, "attach-create seeds one pane");
                snapshot.panes[0].id.clone()
            }
            other => panic!("expected Attached, got {other:?}"),
        };
        let local = seed_id.local_id().expect("the seed pane id must be LOCAL");
        let needle = format!("PTID={local}.");
        // The seed child may print before or after the snapshot arrives, so
        // scan both the snapshot replay and any live output.
        let acc = await_snapshot_or_output_contains(&mut stream, &seed_id, needle.as_bytes()).await;
        let body = String::from_utf8_lossy(&acc);
        assert!(
            acc.windows(needle.len()).any(|w| w == needle.as_bytes()),
            "attach-create seed pane must see PHUX_TERMINAL_ID={local} matching its wire id; \
             got: {body:?}",
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// phux-cufw: the server injects `PHUX_SOCKET` (its own listening UDS
/// path) into every SPAWN_TERMINAL child, so an in-pane `phux` verb
/// targets the pane's own server rather than resolving the default
/// socket path. The child compares `$PHUX_SOCKET` against the server's
/// actual socket and prints a short verdict token — comparing in-pane
/// avoids the 80-column soft-wrap that would break a needle match on
/// the full echoed path.
#[test]
fn spawn_terminal_injects_server_socket_env() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (mut stream, shutdown_tx, server_handle) = spawn_and_attach(&tmp, "default").await;

        let probe = format!(
            "if [ \"$PHUX_SOCKET\" = '{}' ]; then printf 'SOCKOK.\\n'; \
             else printf 'SOCKBAD=%s.\\n' \"$PHUX_SOCKET\"; fi; read _",
            socket_path.display()
        );
        send_frame(
            &mut stream,
            &FrameKind::SpawnTerminal {
                request_id: 56,
                group: DEFAULT_GROUP_ID,
                command: Some(vec!["/bin/sh".to_owned(), "-c".to_owned(), probe]),
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
            },
        )
        .await;

        let new_id = match await_terminal_spawned(&mut stream, 56).await {
            SpawnResult::Ok(id) => id,
            other => panic!("SPAWN_TERMINAL did not succeed: {other:?}"),
        };
        let acc = await_output_contains(&mut stream, &new_id, b"SOCK").await;
        let body = String::from_utf8_lossy(&acc);
        assert!(
            acc.windows(b"SOCKOK.".len()).any(|w| w == b"SOCKOK."),
            "spawned child must see PHUX_SOCKET={}; got output: {body:?}",
            socket_path.display(),
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// phux-cufw: the same `PHUX_SOCKET` injection covers the seed /
/// attach-create path (`seed_session_with_pty`), pairing with the
/// `PHUX_TERMINAL_ID` the seed pane already receives.
#[test]
fn attach_create_seed_pane_injects_server_socket_env() {
    use phux_protocol::wire::frame::{AttachTarget, TYPE_ATTACHED, ViewportInfo};

    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server_seed_pty_no_cmd(socket_path.clone(), None);
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        let probe = format!(
            "if [ \"$PHUX_SOCKET\" = '{}' ]; then printf 'SOCKOK.\\n'; \
             else printf 'SOCKBAD=%s.\\n' \"$PHUX_SOCKET\"; fi; read _",
            socket_path.display()
        );
        send_frame(
            &mut stream,
            &FrameKind::Attach {
                target: AttachTarget::CreateIfMissing {
                    name: "seed-sock".to_owned(),
                    command: Some(vec!["/bin/sh".to_owned(), "-c".to_owned(), probe]),
                    cwd: None,
                },
                viewport: ViewportInfo::new(80, 24),
                request_scrollback: false,
                scrollback_limit_lines: 0,
            },
        )
        .await;

        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED, "expected ATTACHED");
        let seed_id = match attached {
            FrameKind::Attached { snapshot, .. } => {
                assert_eq!(snapshot.panes.len(), 1, "attach-create seeds one pane");
                snapshot.panes[0].id.clone()
            }
            other => panic!("expected Attached, got {other:?}"),
        };
        let acc = await_snapshot_or_output_contains(&mut stream, &seed_id, b"SOCK").await;
        let body = String::from_utf8_lossy(&acc);
        assert!(
            acc.windows(b"SOCKOK.".len()).any(|w| w == b"SOCKOK."),
            "attach-create seed pane must see PHUX_SOCKET={}; got: {body:?}",
            socket_path.display(),
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// phux-cs6 acceptance: with `defaults.cwd-inheritance = inherit-focused`
/// (the schema default the test server runs with), a `SPAWN_TERMINAL`
/// that leaves `cwd` unset opens the new pane in the *focused* pane's
/// live working directory.
///
/// The focused (pre-seeded) pane is a shell that `cd`s into a fresh temp
/// dir and then blocks. The spawned pane runs `pwd`, whose stdout — the
/// inherited directory — comes back as TERMINAL_OUTPUT. This is the wire-
/// level proof of the `C-a |` cd-to-/tmp scenario in the bead.
#[test]
fn spawn_terminal_inherits_focused_pane_live_cwd() {
    use phux_protocol::wire::frame::TYPE_ATTACHED;

    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // Focused pane: a shell sitting in a known temp dir. Canonicalize
        // so the expected path matches what the kernel CWD query returns
        // (macOS resolves /var → /private/var).
        let cwd_dir = TempDir::new().unwrap();
        let cwd_path = cwd_dir.path().canonicalize().expect("canonicalize cwd");
        let mut seed = CommandBuilder::new("/bin/sh");
        seed.arg("-c");
        // `read _` (a builtin) blocks the shell on the PTY, keeping the pane
        // alive in its cwd. NOT `exec read _` — `exec` needs an external
        // program, so it dies immediately (status 1), which raced the ATTACH
        // and flaked on fast CI.
        seed.arg(format!("cd '{}' && read _", cwd_path.display()));
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "focused", seed);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // ATTACH ByName focuses the pre-seeded pane.
        send_frame(&mut stream, &attach_by_name("focused")).await;
        let (type_byte, _attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED, "expected ATTACHED");
        // Drain the seed pane's TERMINAL_SNAPSHOT.
        let (type_byte, _snap) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte,
            phux_protocol::wire::frame::TYPE_TERMINAL_SNAPSHOT,
            "expected TERMINAL_SNAPSHOT",
        );

        // Give the seed shell a beat to run its `cd` before we query it.
        // 75ms is plenty: the shell is already spawned by the time ATTACH
        // returns, and the cd is its first command.
        tokio::time::sleep(Duration::from_millis(75)).await;

        // SPAWN_TERMINAL with cwd UNSET and a command that prints its
        // CWD. With inherit-focused, the server seeds the new pane's
        // CommandBuilder.cwd from the focused pane's live directory.
        send_frame(
            &mut stream,
            &FrameKind::SpawnTerminal {
                request_id: 1,
                group: DEFAULT_GROUP_ID,
                command: Some(vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    // `read _` blocks (a builtin); `exec read _` would die.
                    "pwd; read _".to_owned(),
                ]),
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
            },
        )
        .await;

        let new_id = match await_terminal_spawned(&mut stream, 1).await {
            SpawnResult::Ok(id) => id,
            other => panic!("SPAWN_TERMINAL did not succeed: {other:?}"),
        };

        let needle = cwd_path.to_str().expect("utf8 cwd").as_bytes();
        let acc = await_output_contains(&mut stream, &new_id, needle).await;
        let body = String::from_utf8_lossy(&acc);
        assert!(
            acc.windows(needle.len()).any(|w| w == needle),
            "spawned pane must inherit the focused pane's live CWD ({}); got output: {body:?}",
            cwd_path.display(),
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// phux-0db acceptance: a `CreateIfMissing` attach that carries a `cwd`
/// seeds the pane in *that* directory, not the daemon's CWD. This is the
/// wire-level proof that a `claude` session launched in a phux shell lands
/// in the user's project dir, so its transcript is keyed under the right
/// path hash and `claude --resume` finds it. Before the fix the seed pane
/// inherited the server process's CWD and the wire `cwd` was dropped.
#[test]
fn create_if_missing_seeds_pane_in_wire_cwd() {
    use phux_protocol::wire::frame::{AttachTarget, TYPE_ATTACHED, ViewportInfo};

    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // The "project" dir the client is sitting in. Canonicalize so the
        // expected path matches what `pwd` prints (macOS resolves /var →
        // /private/var).
        let cwd_dir = TempDir::new().unwrap();
        let cwd_path = cwd_dir.path().canonicalize().expect("canonicalize cwd");

        // PTY seed with no override command → the wire command + cwd apply.
        let (shutdown_tx, server_handle) = spawn_server_seed_pty_no_cmd(socket_path.clone(), None);
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // CreateIfMissing carrying the project cwd and a command that prints
        // its working directory. `read _` (a builtin) blocks the shell so the
        // pane stays alive; `exec read _` would die and race the attach.
        send_frame(
            &mut stream,
            &FrameKind::Attach {
                target: AttachTarget::CreateIfMissing {
                    name: "proj".to_owned(),
                    command: Some(vec![
                        "/bin/sh".to_owned(),
                        "-c".to_owned(),
                        "pwd; read _".to_owned(),
                    ]),
                    cwd: Some(cwd_path.to_string_lossy().into_owned()),
                },
                viewport: ViewportInfo::new(80, 24),
                request_scrollback: false,
                scrollback_limit_lines: 0,
            },
        )
        .await;

        // ATTACHED carries the seed pane's wire id.
        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED, "expected ATTACHED");
        let seed_id = match attached {
            FrameKind::Attached { snapshot, .. } => {
                assert_eq!(snapshot.panes.len(), 1, "attach-create seeds one pane");
                snapshot.panes[0].id.clone()
            }
            other => panic!("expected Attached, got {other:?}"),
        };

        let needle = cwd_path.to_str().expect("utf8 cwd").as_bytes();
        // The seed child may print `pwd` before or after the snapshot is
        // captured, so scan both the snapshot replay and any live output.
        let acc = await_snapshot_or_output_contains(&mut stream, &seed_id, needle).await;
        let body = String::from_utf8_lossy(&acc);
        assert!(
            acc.windows(needle.len()).any(|w| w == needle),
            "seed pane must run in the wire cwd ({}); got output: {body:?}",
            cwd_path.display(),
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// phux-nyx acceptance: with `defaults.cwd-inheritance = session-root`, a
/// `SPAWN_TERMINAL` with `cwd` unset opens the new pane in the *session's
/// seed-pane* directory.
///
/// The seed pane sits in `root_dir` and blocks. A spawn under session-root
/// reads the seed pane's live CWD (`root_dir`), freezes it as the session
/// root, and seeds the new pane there. The decisive assertion is that the
/// new pane reports `root_dir`, not `$HOME` and not the server's CWD.
#[test]
fn spawn_terminal_session_root_inherits_seed_pane_dir() {
    use phux_protocol::wire::frame::TYPE_ATTACHED;

    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // Seed pane: a shell sitting in a known temp dir (the session root).
        // Canonicalize so the expected path matches the kernel CWD query
        // (macOS resolves /var → /private/var).
        let root_dir = TempDir::new().unwrap();
        let root_path = root_dir.path().canonicalize().expect("canonicalize root");
        let mut seed = CommandBuilder::new("/bin/sh");
        seed.arg("-c");
        seed.arg(format!("cd '{}' && read _", root_path.display()));
        let (shutdown_tx, server_handle) = spawn_server_with_seed_cmd_and_cwd_mode(
            socket_path.clone(),
            "rooted",
            seed,
            phux_config::CwdInheritance::SessionRoot,
        );

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("rooted")).await;
        let (type_byte, _attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED, "expected ATTACHED");
        let (type_byte, _snap) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte,
            phux_protocol::wire::frame::TYPE_TERMINAL_SNAPSHOT,
            "expected TERMINAL_SNAPSHOT",
        );

        // Give the seed shell a beat to run its `cd` (75ms: see above).
        tokio::time::sleep(Duration::from_millis(75)).await;

        // SPAWN_TERMINAL, cwd unset, command prints its CWD. Under
        // session-root the server seeds the new pane from the seed pane's
        // directory (root_path).
        send_frame(
            &mut stream,
            &FrameKind::SpawnTerminal {
                request_id: 1,
                group: DEFAULT_GROUP_ID,
                command: Some(vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    "pwd; read _".to_owned(),
                ]),
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
            },
        )
        .await;

        let new_id = match await_terminal_spawned(&mut stream, 1).await {
            SpawnResult::Ok(id) => id,
            other => panic!("SPAWN_TERMINAL did not succeed: {other:?}"),
        };

        let needle = root_path.to_str().expect("utf8 root").as_bytes();
        let acc = await_output_contains(&mut stream, &new_id, needle).await;
        let body = String::from_utf8_lossy(&acc);
        assert!(
            acc.windows(needle.len()).any(|w| w == needle),
            "session-root spawn must inherit the seed pane's dir ({}); got output: {body:?}",
            root_path.display(),
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// phux-nyx acceptance: with `defaults.cwd-inheritance = last-cwd-per-window`,
/// a `SPAWN_TERMINAL` with `cwd` unset opens the new pane in the most-recent
/// working directory observed for the spawning client's active window — the
/// active pane's live CWD.
///
/// The seed pane sits in `win_dir` and blocks. A spawn under
/// last-cwd-per-window resolves the active pane's live CWD (`win_dir`),
/// records it against the window, and seeds the new pane there. The
/// decisive assertion is that the new pane reports `win_dir`.
#[test]
fn spawn_terminal_last_cwd_per_window_inherits_active_pane_dir() {
    use phux_protocol::wire::frame::TYPE_ATTACHED;

    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        let win_dir = TempDir::new().unwrap();
        let win_path = win_dir.path().canonicalize().expect("canonicalize win");
        let mut seed = CommandBuilder::new("/bin/sh");
        seed.arg("-c");
        seed.arg(format!("cd '{}' && read _", win_path.display()));
        let (shutdown_tx, server_handle) = spawn_server_with_seed_cmd_and_cwd_mode(
            socket_path.clone(),
            "windowed",
            seed,
            phux_config::CwdInheritance::LastCwdPerWindow,
        );

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("windowed")).await;
        let (type_byte, _attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED, "expected ATTACHED");
        let (type_byte, _snap) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte,
            phux_protocol::wire::frame::TYPE_TERMINAL_SNAPSHOT,
            "expected TERMINAL_SNAPSHOT",
        );

        // Give the seed shell a beat to run its `cd` (75ms: see above).
        tokio::time::sleep(Duration::from_millis(75)).await;

        send_frame(
            &mut stream,
            &FrameKind::SpawnTerminal {
                request_id: 1,
                group: DEFAULT_GROUP_ID,
                command: Some(vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    "pwd; read _".to_owned(),
                ]),
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
            },
        )
        .await;

        let new_id = match await_terminal_spawned(&mut stream, 1).await {
            SpawnResult::Ok(id) => id,
            other => panic!("SPAWN_TERMINAL did not succeed: {other:?}"),
        };

        let needle = win_path.to_str().expect("utf8 win").as_bytes();
        let acc = await_output_contains(&mut stream, &new_id, needle).await;
        let body = String::from_utf8_lossy(&acc);
        assert!(
            acc.windows(needle.len()).any(|w| w == needle),
            "last-cwd-per-window spawn must inherit the active pane's live dir ({}); \
             got output: {body:?}",
            win_path.display(),
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}
