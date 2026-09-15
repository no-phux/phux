//! ADR-0126 — a keyed spawn or session create never runs twice.
//!
//! Every case drives the production read loop over a real UDS connection.
//! A keyed `SPAWN_RESOURCE` repeated inside the horizon answers the original
//! id marked replayed and creates nothing; the same key with another payload
//! is `IDEMPOTENCY_CONFLICT`; a keyed `phux.session.create/v1` repeated with
//! its `request_token` publishes the original result again instead of
//! failing on the duplicate name. The keys ride the resulting
//! `pane_spawned` as its `operation_id`.
//!
//! The horizon itself (a repeat past ten minutes spawns again) is pinned
//! against an injected clock in `runtime::operation_dedupe`, and the hub's
//! refusal toward a satellite without `SPAWN_IDEMPOTENCY` in `hub::relay`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::path::Path;

use phux_protocol::ids::{IdempotencyKey, ResourceId};
use phux_protocol::wire::frame::{
    AgentEvent, AttachTarget, Command, CommandResult, CommandValue, FrameKind, SESSION_CREATE_KEY,
    SESSION_CREATE_RESULT_KEY_PREFIX, Scope, SpawnError, SpawnResource, SpawnResult, StateScope,
    TYPE_ATTACHED, TYPE_METADATA_VALUE, TYPE_RESOURCE_SPAWNED, ViewportInfo,
};
use phux_server::DEFAULT_GROUP_ID;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, await_command_result, join_after_shutdown,
    recv_typed, run_local, send_frame, spawn_server, wait_for_socket,
};

/// A request token whose 16 bytes are `1..=16`.
const TOKEN: &str = "01020304-0506-0708-090a-0b0c0d0e0f10";

const fn key(byte: u8) -> Option<IdempotencyKey> {
    IdempotencyKey::new([byte; 16])
}

fn token_key() -> Option<IdempotencyKey> {
    let mut bytes = [0; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::try_from(index + 1).unwrap();
    }
    IdempotencyKey::new(bytes)
}

/// A child that stays alive on its PTY without producing output.
fn spawn(request_id: u32, script: &str, resource: Option<SpawnResource>) -> FrameKind {
    FrameKind::SpawnResource {
        request_id,
        group: DEFAULT_GROUP_ID,
        command: Some(vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            script.to_owned(),
        ]),
        cwd: None,
        env: None,
        term: None,
        satellite: None,
        owner_terminal: None,
        agent_session: None,
        initial_size: None,
        resource: resource.map(Box::new),
    }
}

fn keyed(request_id: u32, script: &str, key: Option<IdempotencyKey>) -> FrameKind {
    spawn(
        request_id,
        script,
        Some(SpawnResource::default().with_idempotency_key(key)),
    )
}

/// Connect and attach to `main`, creating it on first use, so the
/// connection is negotiated and hosts spawns.
async fn attach_main(socket_path: &Path) -> UnixStream {
    let mut stream = wait_for_socket(socket_path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut stream,
        &FrameKind::Attach {
            attach_id: 1,
            target: AttachTarget::CreateIfMissing {
                name: "main".to_owned(),
                command: None,
                cwd: None,
            },
            viewport: ViewportInfo::new(80, 24),
            request_scrollback: false,
            scrollback_limit_lines: 0,
        },
    )
    .await;
    let (type_byte, _) = recv_typed(&mut stream).await;
    assert_eq!(type_byte, TYPE_ATTACHED, "expected ATTACHED");
    stream
}

async fn await_spawned(stream: &mut UnixStream, request_id: u32) -> SpawnResult {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Ok((type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if type_byte != TYPE_RESOURCE_SPAWNED {
            continue;
        }
        if let FrameKind::ResourceSpawned {
            request_id: got,
            result,
        } = frame
            && got == request_id
        {
            return result;
        }
    }
    panic!("timed out waiting for RESOURCE_SPAWNED request_id={request_id}");
}

async fn resource_count(stream: &mut UnixStream, request_id: u32) -> usize {
    resource_ids(stream, request_id).await.len()
}

async fn resource_ids(stream: &mut UnixStream, request_id: u32) -> Vec<ResourceId> {
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
    let CommandResult::OkWith(CommandValue::State(snapshot)) =
        await_command_result(stream, request_id).await
    else {
        panic!("GET_STATE must succeed");
    };
    snapshot.resources.into_iter().map(|pane| pane.id).collect()
}

fn expect_fresh(result: SpawnResult) -> ResourceId {
    match result {
        SpawnResult::Ok(id) => id,
        other => panic!("expected a fresh OK, got {other:?}"),
    }
}

#[test]
fn keyed_spawn_repeated_returns_the_same_id_with_replayed_set_and_spawns_once() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);
        let mut stream = attach_main(&socket_path).await;
        let before = resource_count(&mut stream, 100).await;

        send_frame(&mut stream, &keyed(1, "read _", key(1))).await;
        let id = expect_fresh(await_spawned(&mut stream, 1).await);
        send_frame(&mut stream, &keyed(2, "read _", key(1))).await;
        assert_eq!(
            await_spawned(&mut stream, 2).await,
            SpawnResult::Replayed {
                id: id.clone(),
                instance: None
            },
            "the repeat answers the original id, re-correlated and marked replayed"
        );
        assert_eq!(
            resource_count(&mut stream, 101).await,
            before + 1,
            "the repeat spawned nothing"
        );

        // A bound spawn replays its original instance token too.
        let bound = SpawnResource::default()
            .with_bind_instance(true)
            .with_idempotency_key(key(2));
        send_frame(&mut stream, &spawn(3, "read _", Some(bound.clone()))).await;
        let SpawnResult::OkBound { id, instance } = await_spawned(&mut stream, 3).await else {
            panic!("a bound spawn answers its instance");
        };
        send_frame(&mut stream, &spawn(4, "read _", Some(bound))).await;
        assert_eq!(
            await_spawned(&mut stream, 4).await,
            SpawnResult::Replayed {
                id,
                instance: Some(instance)
            }
        );

        drop(stream);
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}

#[test]
fn keyed_spawn_with_a_different_payload_is_idempotency_conflict() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);
        let mut stream = attach_main(&socket_path).await;

        send_frame(&mut stream, &keyed(1, "read _", key(3))).await;
        let _ = expect_fresh(await_spawned(&mut stream, 1).await);
        let before = resource_count(&mut stream, 100).await;
        send_frame(&mut stream, &keyed(2, "read other", key(3))).await;
        assert_eq!(
            await_spawned(&mut stream, 2).await,
            SpawnResult::Err(SpawnError::IdempotencyConflict)
        );
        assert_eq!(resource_count(&mut stream, 101).await, before);

        drop(stream);
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}

#[test]
fn keyed_spawn_replay_survives_reconnect_within_horizon() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);

        let mut first = attach_main(&socket_path).await;
        send_frame(&mut first, &keyed(1, "read _", key(4))).await;
        let id = expect_fresh(await_spawned(&mut first, 1).await);
        // The reply's connection is gone: the case a key exists for.
        drop(first);

        let mut second = attach_main(&socket_path).await;
        send_frame(&mut second, &keyed(9, "read _", key(4))).await;
        assert_eq!(
            await_spawned(&mut second, 9).await,
            SpawnResult::Replayed { id, instance: None }
        );

        drop(second);
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}

fn create_value(name: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "name": name,
        "request_token": TOKEN,
        "empty": true,
    }))
    .unwrap()
}

async fn create_session(stream: &mut UnixStream, request_id: u32, name: &str) {
    send_frame(
        stream,
        &FrameKind::SetMetadata {
            request_id,
            scope: Scope::Global,
            key: SESSION_CREATE_KEY.to_owned(),
            value: create_value(name),
        },
    )
    .await;
}

/// Read (and so consume) this connection's result for [`TOKEN`].
async fn read_create_result(stream: &mut UnixStream, request_id: u32) -> Option<Vec<u8>> {
    read_create_result_at(stream, request_id, TOKEN).await
}

/// Read (and so consume) this connection's result under `token`'s spelling.
async fn read_create_result_at(
    stream: &mut UnixStream,
    request_id: u32,
    token: &str,
) -> Option<Vec<u8>> {
    send_frame(
        stream,
        &FrameKind::GetMetadata {
            request_id,
            scope: Scope::Global,
            key: format!("{SESSION_CREATE_RESULT_KEY_PREFIX}{token}"),
        },
    )
    .await;
    loop {
        let (type_byte, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(stream))
            .await
            .expect("METADATA_VALUE");
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
}

#[test]
fn session_create_with_a_repeated_request_token_returns_the_same_result() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("main"));

        let mut first = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        create_session(&mut first, 1, "idem").await;
        let original = read_create_result(&mut first, 2)
            .await
            .expect("the create publishes its result");
        let json: serde_json::Value = serde_json::from_slice(&original).unwrap();
        assert_eq!(json["name"], "idem");

        // Repeated after the result was read: the same result, not a
        // duplicate-name refusal.
        create_session(&mut first, 3, "idem").await;
        assert_eq!(
            read_create_result(&mut first, 4).await,
            Some(original.clone())
        );

        // Repeated from another connection, as after a reconnect.
        let mut second = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        create_session(&mut second, 1, "idem").await;
        assert_eq!(read_create_result(&mut second, 2).await, Some(original));

        // The same token with another request creates and publishes nothing.
        create_session(&mut second, 3, "other").await;
        assert_eq!(read_create_result(&mut second, 4).await, None);

        drop(first);
        drop(second);
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}

/// Subscribe server-wide with a journal cursor, then prove the subscription
/// landed with a `GET_STATE` round-trip.
async fn subscribe_server_wide(stream: &mut UnixStream, request_id: u32) {
    send_frame(
        stream,
        &FrameKind::SubscribeEvents {
            terminal: None,
            after_seq: Some(u64::MAX),
        },
    )
    .await;
    let _ = resource_count(stream, request_id).await;
}

/// The next `pane_spawned`: its Terminal and its `operation_id`.
async fn next_pane_spawned(
    stream: &mut UnixStream,
) -> (Option<ResourceId>, Option<IdempotencyKey>) {
    loop {
        let (_, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(stream))
            .await
            .expect("pane_spawned");
        if let FrameKind::Event {
            terminal,
            event: AgentEvent::ResourceSpawned { .. },
            stamp,
        } = frame
        {
            return (terminal, stamp.and_then(|stamp| stamp.operation_id));
        }
    }
}

async fn pane_spawned_for(
    stream: &mut UnixStream,
    id: &ResourceId,
) -> (Option<ResourceId>, Option<IdempotencyKey>) {
    loop {
        let spawned = next_pane_spawned(stream).await;
        if spawned.0.as_ref() == Some(id) {
            return spawned;
        }
    }
}

#[test]
fn pane_spawned_event_carries_the_operation_id() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);
        let mut watcher = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        subscribe_server_wide(&mut watcher, 1).await;
        let mut spawner = attach_main(&socket_path).await;

        send_frame(&mut spawner, &keyed(1, "read _", key(5))).await;
        let id = expect_fresh(await_spawned(&mut spawner, 1).await);
        assert_eq!(
            pane_spawned_for(&mut watcher, &id).await,
            (Some(id.clone()), key(5)),
            "the keyed spawn's pane_spawned carries its key"
        );

        // The replay journals nothing: the next pane_spawned the watcher
        // sees is the unkeyed spawn that follows it.
        send_frame(&mut spawner, &keyed(2, "read _", key(5))).await;
        assert!(matches!(
            await_spawned(&mut spawner, 2).await,
            SpawnResult::Replayed { .. }
        ));
        send_frame(&mut spawner, &spawn(3, "read _", None)).await;
        let unkeyed = expect_fresh(await_spawned(&mut spawner, 3).await);
        assert_eq!(
            next_pane_spawned(&mut watcher).await,
            (Some(unkeyed), None),
            "a replay must not journal a second pane_spawned"
        );

        // A keyed session create's seed pane carries the token.
        send_frame(
            &mut spawner,
            &FrameKind::SetMetadata {
                request_id: 4,
                scope: Scope::Global,
                key: SESSION_CREATE_KEY.to_owned(),
                value: serde_json::to_vec(&serde_json::json!({
                    "name": "keyed",
                    "command": ["/bin/sh", "-c", "read _"],
                    "request_token": TOKEN,
                }))
                .unwrap(),
            },
        )
        .await;
        assert_eq!(next_pane_spawned(&mut watcher).await.1, token_key());

        drop(watcher);
        drop(spawner);
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}

/// The spawner drops its socket right after sending a keyed spawn: its
/// reply may be lost, and its pane is reaped if publication could not reach
/// it. The retry must then either replay a pane that still exists or spawn
/// fresh, never replay a reaped one (SPEC L1 §3.1: a refused spawn binds
/// nothing). The deterministic reap path is pinned in
/// `runtime::idempotent_create`.
#[test]
fn a_retry_after_the_spawner_vanished_never_replays_a_reaped_pane() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);

        let mut first = attach_main(&socket_path).await;
        send_frame(&mut first, &keyed(1, "read _", key(6))).await;
        drop(first);

        let mut second = attach_main(&socket_path).await;
        send_frame(&mut second, &keyed(2, "read _", key(6))).await;
        match await_spawned(&mut second, 2).await {
            SpawnResult::Ok(_) => {}
            SpawnResult::Replayed { id, .. } => assert!(
                resource_ids(&mut second, 100).await.contains(&id),
                "a replay names a live pane"
            ),
            other => panic!("expected a fresh or replayed spawn, got {other:?}"),
        }

        drop(second);
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}

/// Tokens are case-insensitive for dedupe, but each request reads its own
/// spelling: a repeat of a lowercase create in upper case is a replay,
/// published under, and echoing, the upper-case spelling.
#[test]
fn request_tokens_are_case_insensitive() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("main"));
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        create_session(&mut stream, 1, "idem").await;
        let original: serde_json::Value = serde_json::from_slice(
            &read_create_result(&mut stream, 2)
                .await
                .expect("the lowercase create publishes its result"),
        )
        .unwrap();
        assert_eq!(original["request_token"], TOKEN);

        let upper_token = TOKEN.to_ascii_uppercase();
        let upper = serde_json::to_vec(&serde_json::json!({
            "name": "idem",
            "request_token": upper_token,
            "empty": true,
        }))
        .unwrap();
        send_frame(
            &mut stream,
            &FrameKind::SetMetadata {
                request_id: 3,
                scope: Scope::Global,
                key: SESSION_CREATE_KEY.to_owned(),
                value: upper,
            },
        )
        .await;
        let replayed: serde_json::Value = serde_json::from_slice(
            &read_create_result_at(&mut stream, 4, &upper_token)
                .await
                .expect("the replay is published under the repeat's own spelling"),
        )
        .unwrap();
        assert_eq!(
            replayed["request_token"], upper_token,
            "the replay echoes the repeat's spelling"
        );
        for field in ["name", "session_id", "terminal_id", "empty"] {
            assert_eq!(
                replayed[field], original[field],
                "the replay names the same session: {field}"
            );
        }
        assert_eq!(
            read_create_result(&mut stream, 5).await,
            None,
            "nothing is republished under the original spelling"
        );

        drop(stream);
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}
