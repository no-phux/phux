//! ADR-0105 lifecycle tests: a keep-empty session survives its last window.
//!
//! Every test drives a real server over UDS through the production frame
//! loop. Each server keeps a pre-seeded `anchor` session alive, so removing
//! a test session never drains the server: these assertions are about the
//! session. The server's own last-session self-exit is `server_self_exit.rs`.
//!
//! Reaping is asynchronous (the pane's exit watcher does it), so the tests
//! that wait on a reap poll `GET_STATE` under a deadline rather than sleep.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_protocol::ids::{GroupId, ResourceId};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, DetachReason, FrameKind, SESSION_CREATE_KEY,
    SESSION_CREATE_RESULT_KEY_PREFIX, SESSION_KEEP_EMPTY_KEY, Scope, SpawnResult, StateScope,
    TYPE_ATTACHED, encode_session_keep_empty,
};
use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, join_after_shutdown,
    recv_command_result, recv_typed, run_local, send_frame, spawn_server_seed_pty_no_cmd,
    wait_for_socket,
};

/// The pre-seeded session that keeps every test server up.
const ANCHOR: &str = "anchor";

/// How long a test waits for an asynchronous reap to show in `GET_STATE`.
const REAP_DEADLINE: Duration = Duration::from_secs(10);

/// A seed command that stays alive on its PTY without producing output.
fn blocking_seed_command() -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), "read _".to_owned()]
}

/// Start a server with the anchor session and connect to it.
async fn start(
    tmp: &TempDir,
) -> (
    UnixStream,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<(), phux_server::ServerError>>,
) {
    let socket_path = tmp.path().join("phux.sock");
    let (shutdown_tx, server) = spawn_server_seed_pty_no_cmd(socket_path.clone(), Some(ANCHOR));
    let conn = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
    (conn, shutdown_tx, server)
}

/// `GET_STATE { Server }` on `stream`.
async fn get_state(stream: &mut UnixStream, request_id: u32) -> SessionSnapshot {
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
    match timeout(WIRE_RECV_TIMEOUT, recv_command_result(stream, request_id))
        .await
        .expect("GET_STATE must be answered")
    {
        CommandResult::OkWith(CommandValue::State(snapshot)) => snapshot,
        other => panic!("GET_STATE failed: {other:?}"),
    }
}

fn session<'a>(snapshot: &'a SessionSnapshot, name: &str) -> Option<&'a SessionInfo> {
    snapshot.sessions.iter().find(|s| s.name == name)
}

/// Poll `GET_STATE` until `ready` holds, failing after [`REAP_DEADLINE`].
async fn wait_for_state(
    stream: &mut UnixStream,
    first_request_id: u32,
    what: &str,
    ready: impl Fn(&SessionSnapshot) -> bool,
) -> SessionSnapshot {
    let end = tokio::time::Instant::now() + REAP_DEADLINE;
    let mut request_id = first_request_id;
    loop {
        let snapshot = get_state(stream, request_id).await;
        if ready(&snapshot) {
            return snapshot;
        }
        assert!(
            tokio::time::Instant::now() < end,
            "timed out waiting for {what}"
        );
        request_id += 1;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A request token the server accepts (UUID-shaped), unique per `request_id`.
fn token(request_id: u32) -> String {
    format!("00000000-0000-4000-8000-{request_id:012}")
}

/// Send one `phux.session.create/v1` write carrying a request token.
async fn send_create(stream: &mut UnixStream, mut body: serde_json::Value, request_id: u32) {
    body["request_token"] = serde_json::Value::String(token(request_id));
    send_frame(
        stream,
        &FrameKind::SetMetadata {
            request_id,
            scope: Scope::Global,
            key: SESSION_CREATE_KEY.to_owned(),
            value: serde_json::to_vec(&body).unwrap(),
        },
    )
    .await;
}

/// Read the one-shot result of the create sent with `request_id`.
async fn create_result(stream: &mut UnixStream, request_id: u32) -> Option<serde_json::Value> {
    let read_id = request_id + 10_000;
    send_frame(
        stream,
        &FrameKind::GetMetadata {
            request_id: read_id,
            scope: Scope::Global,
            key: format!("{SESSION_CREATE_RESULT_KEY_PREFIX}{}", token(request_id)),
        },
    )
    .await;
    loop {
        let (_type_byte, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(stream))
            .await
            .expect("the create result read must be answered");
        if let FrameKind::MetadataValue { request_id, value } = frame
            && request_id == read_id
        {
            return value.map(|bytes| serde_json::from_slice(&bytes).unwrap());
        }
    }
}

/// Create a session and return its result document.
async fn create(
    stream: &mut UnixStream,
    body: serde_json::Value,
    request_id: u32,
) -> serde_json::Value {
    send_create(stream, body, request_id).await;
    create_result(stream, request_id)
        .await
        .expect("a successful create publishes its result")
}

/// Create a seeded session running a blocking command and return its pane.
async fn create_seeded(
    stream: &mut UnixStream,
    name: &str,
    keep_empty: bool,
    request_id: u32,
) -> ResourceId {
    let body = serde_json::json!({
        "name": name,
        "command": blocking_seed_command(),
        "keep_empty": keep_empty,
    });
    let result = create(stream, body, request_id).await;
    let id = result["terminal_id"]
        .as_u64()
        .and_then(|id| u32::try_from(id).ok())
        .expect("a seeded create names its seed pane");
    ResourceId::local(id)
}

/// Write `phux.session.keep_empty/v1` for `name`.
async fn set_keep_empty(stream: &mut UnixStream, name: &str, keep: bool, request_id: u32) {
    send_frame(
        stream,
        &FrameKind::SetMetadata {
            request_id,
            scope: Scope::Global,
            key: SESSION_KEEP_EMPTY_KEY.to_owned(),
            value: encode_session_keep_empty(name, keep),
        },
    )
    .await;
}

/// Send one command and require `COMMAND_RESULT { Ok }`.
async fn command_ok(stream: &mut UnixStream, request_id: u32, command: Command) {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command,
        },
    )
    .await;
    let result = timeout(WIRE_RECV_TIMEOUT, recv_command_result(stream, request_id))
        .await
        .expect("the command must be answered");
    assert!(
        matches!(result, CommandResult::Ok),
        "command failed: {result:?}"
    );
}

/// `GET_METADATA` on `stream`, returning the value.
async fn get_metadata(
    stream: &mut UnixStream,
    request_id: u32,
    scope: Scope,
    key: &str,
) -> Option<Vec<u8>> {
    send_frame(
        stream,
        &FrameKind::GetMetadata {
            request_id,
            scope,
            key: key.to_owned(),
        },
    )
    .await;
    loop {
        let (_type_byte, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(stream))
            .await
            .expect("the metadata read must be answered");
        if let FrameKind::MetadataValue {
            request_id: id,
            value,
        } = frame
            && id == request_id
        {
            return value;
        }
    }
}

/// A group kill of a keep-empty session with an attached client broadcasts
/// the released mark and then detaches the client with `SESSION_KILLED`,
/// instead of stranding it on a session that no longer exists.
#[test]
fn group_kill_detaches_clients_attached_to_a_keep_empty_session() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut conn, shutdown_tx, server) = start(&tmp).await;
        let socket_path = tmp.path().join("phux.sock");
        let pane = create_seeded(&mut conn, "kept", true, 1).await;

        let mut client = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut client, &attach_by_name("kept")).await;
        let (type_byte, _attached) = timeout(WIRE_RECV_TIMEOUT, recv_typed(&mut client))
            .await
            .expect("the attach must be answered");
        assert_eq!(type_byte, TYPE_ATTACHED);
        send_frame(
            &mut client,
            &FrameKind::SubscribeMetadata {
                scope: Scope::Global,
                key: SESSION_KEEP_EMPTY_KEY.to_owned(),
            },
        )
        .await;
        // Barrier: the subscribe ran before this answer was produced.
        get_state(&mut client, 50).await;

        command_ok(&mut conn, 2, Command::KillResources { ids: vec![pane] }).await;

        let mut released = false;
        let reason = loop {
            let (_type_byte, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(&mut client))
                .await
                .expect("the attached client must be told the session went");
            match frame {
                FrameKind::MetadataChanged { key, value, .. } if key == SESSION_KEEP_EMPTY_KEY => {
                    assert_eq!(value, Some(encode_session_keep_empty("kept", false)));
                    released = true;
                }
                FrameKind::Detached { reason, .. } => break reason,
                _ => {}
            }
        };
        assert!(released, "the released mark is broadcast before the detach");
        assert_eq!(reason, Some(DetachReason::SessionKilled));
        wait_for_state(&mut conn, 100, "the session to go", |s| {
            session(s, "kept").is_none()
        })
        .await;

        drop(client);
        drop(conn);
        join_after_shutdown(shutdown_tx, server).await;
    });
}

/// A keep-empty session that loses its last window drops its stored TUI
/// layout: the tree names only dead panes, and a later attach must see the
/// empty state rather than adopt it.
#[test]
fn losing_the_last_window_deletes_the_stored_layout() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut conn, shutdown_tx, server) = start(&tmp).await;

        let pane = create_seeded(&mut conn, "kept", true, 1).await;
        let wire_session = session(&get_state(&mut conn, 2).await, "kept")
            .expect("listed")
            .id;
        let key = format!("phux.tui.layout/v1/{}", wire_session.get());
        let layout_scope = Scope::Group(GroupId::new(1));
        send_frame(
            &mut conn,
            &FrameKind::SetMetadata {
                request_id: 3,
                scope: layout_scope.clone(),
                key: key.clone(),
                value: b"stale tree".to_vec(),
            },
        )
        .await;
        assert!(
            get_metadata(&mut conn, 4, layout_scope.clone(), &key)
                .await
                .is_some()
        );

        command_ok(&mut conn, 5, Command::KillResource { terminal_id: pane }).await;
        wait_for_state(&mut conn, 100, "the last window to go", |s| {
            session(s, "kept").is_some_and(SessionInfo::is_empty)
        })
        .await;
        assert!(
            get_metadata(&mut conn, 6, layout_scope, &key)
                .await
                .is_none(),
            "the dead layout must be deleted with the last window"
        );

        drop(conn);
        join_after_shutdown(shutdown_tx, server).await;
    });
}

/// `empty: true` creates a keep-empty session with zero windows, answers
/// with a `null` terminal id, and lists it as keep-empty and empty.
#[test]
fn empty_create_makes_a_windowless_keep_empty_session() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut conn, shutdown_tx, server) = start(&tmp).await;

        let result = create(
            &mut conn,
            serde_json::json!({ "name": "parked", "empty": true }),
            1,
        )
        .await;
        assert_eq!(result["name"], "parked");
        assert!(result["terminal_id"].is_null());
        assert_eq!(result["empty"], true);

        let snapshot = get_state(&mut conn, 2).await;
        let parked = session(&snapshot, "parked").expect("the empty session is listed");
        assert_eq!(parked.window_count, 0);
        assert!(parked.keep_empty);
        assert!(parked.is_empty());
        assert!(
            !session(&snapshot, ANCHOR).unwrap().keep_empty,
            "a default session is not keep-empty"
        );

        drop(conn);
        join_after_shutdown(shutdown_tx, server).await;
    });
}

/// An empty session has no terminal to run a command in: `empty` with a
/// `command` is refused and creates nothing.
#[test]
fn empty_create_with_a_command_is_refused() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut conn, shutdown_tx, server) = start(&tmp).await;

        let body = serde_json::json!({
            "name": "confused",
            "empty": true,
            "command": blocking_seed_command(),
        });
        send_create(&mut conn, body, 1).await;
        assert!(create_result(&mut conn, 1).await.is_none());
        assert!(session(&get_state(&mut conn, 2).await, "confused").is_none());

        drop(conn);
        join_after_shutdown(shutdown_tx, server).await;
    });
}

/// The reap cascade stops at a keep-empty session: its last pane's death
/// removes the window and keeps the session. A default session beside it
/// still goes with its pane.
#[test]
fn cascade_stops_at_a_keep_empty_session() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut conn, shutdown_tx, server) = start(&tmp).await;

        let kept = create_seeded(&mut conn, "kept", true, 1).await;
        let plain = create_seeded(&mut conn, "plain", false, 2).await;
        command_ok(&mut conn, 3, Command::KillResource { terminal_id: kept }).await;
        command_ok(&mut conn, 4, Command::KillResource { terminal_id: plain }).await;

        let snapshot = wait_for_state(&mut conn, 100, "both reaps", |s| {
            session(s, "plain").is_none() && session(s, "kept").is_some_and(SessionInfo::is_empty)
        })
        .await;
        let kept = session(&snapshot, "kept").expect("the keep-empty session survives");
        assert!(kept.keep_empty);
        assert_eq!(kept.window_count, 0);

        drop(conn);
        join_after_shutdown(shutdown_tx, server).await;
    });
}

/// A `KILL_RESOURCES` naming every pane of a keep-empty session is a group
/// teardown: the session goes with its panes.
#[test]
fn kill_resources_naming_every_pane_removes_a_keep_empty_session() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut conn, shutdown_tx, server) = start(&tmp).await;

        let pane = create_seeded(&mut conn, "kept", true, 1).await;
        command_ok(&mut conn, 2, Command::KillResources { ids: vec![pane] }).await;
        wait_for_state(&mut conn, 100, "the session to go", |s| {
            session(s, "kept").is_none()
        })
        .await;

        drop(conn);
        join_after_shutdown(shutdown_tx, server).await;
    });
}

/// Clearing the mark on an empty session removes it: the kill path for a
/// session with no pane to name.
#[test]
fn clearing_the_mark_on_an_empty_session_removes_it() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut conn, shutdown_tx, server) = start(&tmp).await;

        create(
            &mut conn,
            serde_json::json!({ "name": "parked", "empty": true }),
            1,
        )
        .await;
        set_keep_empty(&mut conn, "parked", false, 2).await;
        // Frames are handled in order, so this GET_STATE sees the write.
        let snapshot = get_state(&mut conn, 3).await;
        assert!(session(&snapshot, "parked").is_none());
        assert!(session(&snapshot, ANCHOR).is_some());

        drop(conn);
        join_after_shutdown(shutdown_tx, server).await;
    });
}

/// `phux.session.keep_empty/v1` sets the mark, broadcasts the applied value
/// only when it changes, and is not stored.
#[test]
fn keep_empty_mark_is_applied_and_broadcast_on_change() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut writer, shutdown_tx, server) = start(&tmp).await;
        let socket_path = tmp.path().join("phux.sock");
        let mut watcher = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(
            &mut watcher,
            &FrameKind::SubscribeMetadata {
                scope: Scope::Global,
                key: SESSION_KEEP_EMPTY_KEY.to_owned(),
            },
        )
        .await;
        // Barrier: the subscribe ran before this answer was produced.
        get_state(&mut watcher, 1).await;

        create_seeded(&mut writer, "work", false, 1).await;
        set_keep_empty(&mut writer, "work", true, 2).await;
        // A repeat of the same mark changes nothing and must not broadcast,
        // so the next broadcast the watcher sees is the clear below.
        set_keep_empty(&mut writer, "work", true, 3).await;
        let snapshot = get_state(&mut writer, 4).await;
        assert!(session(&snapshot, "work").unwrap().keep_empty);
        set_keep_empty(&mut writer, "work", false, 5).await;

        let mut seen = Vec::new();
        while seen.len() < 2 {
            let (_type_byte, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(&mut watcher))
                .await
                .expect("the watcher must hear both changes");
            if let FrameKind::MetadataChanged { key, value, .. } = frame
                && key == SESSION_KEEP_EMPTY_KEY
            {
                seen.push(value);
            }
        }
        assert_eq!(
            seen,
            vec![
                Some(encode_session_keep_empty("work", true)),
                Some(encode_session_keep_empty("work", false)),
            ]
        );

        send_frame(
            &mut writer,
            &FrameKind::GetMetadata {
                request_id: 6,
                scope: Scope::Global,
                key: SESSION_KEEP_EMPTY_KEY.to_owned(),
            },
        )
        .await;
        let stored = loop {
            let (_type_byte, frame) = recv_typed(&mut writer).await;
            if let FrameKind::MetadataValue {
                request_id: 6,
                value,
            } = frame
            {
                break value;
            }
        };
        assert!(stored.is_none(), "the mark is applied, never stored");

        drop(watcher);
        drop(writer);
        join_after_shutdown(shutdown_tx, server).await;
    });
}

/// The server accepts an attach to an empty session (sentinel focus ids),
/// and a spawn from that attach creates the session's first window.
#[test]
fn attach_to_an_empty_session_then_spawn_opens_a_window() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut conn, shutdown_tx, server) = start(&tmp).await;
        let socket_path = tmp.path().join("phux.sock");

        create(
            &mut conn,
            serde_json::json!({ "name": "parked", "empty": true }),
            1,
        )
        .await;

        let mut client = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut client, &attach_by_name("parked")).await;
        let (type_byte, attached) = timeout(WIRE_RECV_TIMEOUT, recv_typed(&mut client))
            .await
            .expect("the attach must be answered");
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "an empty session must be attachable"
        );
        let FrameKind::Attached { snapshot, .. } = attached else {
            panic!("expected ATTACHED");
        };
        assert_eq!(snapshot.focused_resource, ResourceId::local(0));
        let parked = session(&snapshot, "parked").unwrap();
        assert!(parked.keep_empty && parked.is_empty());
        assert_eq!(snapshot.focused_session, parked.id);

        send_frame(
            &mut client,
            &FrameKind::SpawnResource {
                request_id: 7,
                group: GroupId::new(1),
                command: Some(blocking_seed_command()),
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
                initial_size: None,
                resource: None,
            },
        )
        .await;
        let spawned = loop {
            let (_type_byte, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(&mut client))
                .await
                .expect("the spawn must be answered");
            if let FrameKind::ResourceSpawned {
                request_id: 7,
                result,
            } = frame
            {
                break result;
            }
        };
        assert!(
            matches!(spawned, SpawnResult::Ok(_)),
            "spawn into an empty session failed: {spawned:?}"
        );
        let snapshot = get_state(&mut conn, 2).await;
        assert_eq!(session(&snapshot, "parked").unwrap().window_count, 1);

        drop(client);
        drop(conn);
        join_after_shutdown(shutdown_tx, server).await;
    });
}
