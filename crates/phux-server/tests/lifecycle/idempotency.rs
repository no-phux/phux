//! Keyed operations never run twice (ADR-0126, `docs/spec/L1.md` §5.1.1): a
//! keyed spawn, session create, or kill repeated answers the original result
//! and does nothing; the same key with another payload is refused; and the
//! key rides the resulting event. The ten-minute horizon and the hub's
//! forwarding are unit-tested in `runtime::operation_dedupe`/`hub::relay`.

use phux_protocol::ids::{IdempotencyKey, ResourceId};
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, ErrorCode, SpawnError, SpawnResource, SpawnResult,
};
use phux_server_testkit::{Spawn, command, spawn_resource};
use tokio::net::UnixStream;

use crate::common::{
    Seen, Server, attach_create, create_result, find, next_event, send_create, sh, spawned, state,
    subscribe, wait_for_state,
};

/// A request token whose 16 bytes are `1..=16`.
const TOKEN: &str = "01020304-0506-0708-090a-0b0c0d0e0f10";

const fn key(byte: u8) -> Option<IdempotencyKey> {
    IdempotencyKey::new([byte; 16])
}

fn keyed(script: &str, key: Option<IdempotencyKey>) -> Spawn {
    Spawn {
        resource: Some(Box::new(SpawnResource::default().with_idempotency_key(key))),
        ..sh(script)
    }
}

/// A client attached to `main` (created on first use), so it hosts spawns.
async fn attached(server: &Server) -> UnixStream {
    let mut stream = server.connect().await;
    attach_create(&mut stream, "main", None, None).await;
    stream
}

async fn resource_count(stream: &mut UnixStream, request_id: u32) -> usize {
    state(stream, request_id).await.resources.len()
}

#[test]
fn keyed_spawn_replays_its_result_across_connections_and_refuses_a_new_payload() {
    phux_server_testkit::run_local(async {
        let server = Server::start(None, |_| {});
        let mut first = attached(&server).await;
        let before = resource_count(&mut first, 100).await;
        let id = spawned(&mut first, 1, keyed("read _", key(1))).await;
        assert_eq!(
            spawn_resource(&mut first, 2, keyed("read _", key(1))).await,
            SpawnResult::Replayed {
                id: id.clone(),
                instance: None
            },
            "the repeat answers the original id, marked replayed"
        );
        assert_eq!(
            resource_count(&mut first, 101).await,
            before + 1,
            "and spawns nothing"
        );

        // A bound spawn replays its instance token too.
        let bound = SpawnResource::default()
            .with_bind_instance(true)
            .with_idempotency_key(key(2));
        let bound = || Spawn {
            resource: Some(Box::new(bound.clone())),
            ..sh("read _")
        };
        let SpawnResult::OkBound {
            id: bound_id,
            instance,
        } = spawn_resource(&mut first, 3, bound()).await
        else {
            panic!("a bound spawn answers its instance");
        };
        assert_eq!(
            spawn_resource(&mut first, 4, bound()).await,
            SpawnResult::Replayed {
                id: bound_id,
                instance: Some(instance)
            }
        );

        let before = resource_count(&mut first, 102).await;
        assert_eq!(
            spawn_resource(&mut first, 5, keyed("read other", key(1))).await,
            SpawnResult::Err(SpawnError::IdempotencyConflict)
        );
        assert_eq!(resource_count(&mut first, 103).await, before);

        // The reply's connection is gone: the case a key exists for.
        drop(first);
        let mut second = attached(&server).await;
        assert_eq!(
            spawn_resource(&mut second, 9, keyed("read _", key(1))).await,
            SpawnResult::Replayed { id, instance: None }
        );

        drop(second);
        server.stop().await;
    });
}

/// A spawner that vanishes right after a keyed spawn may lose its reply and
/// have its pane reaped; the retry must then spawn fresh or replay a pane
/// that still exists, never a reaped one (L1 §3.1).
#[test]
fn a_retry_after_the_spawner_vanished_never_replays_a_reaped_pane() {
    phux_server_testkit::run_local(async {
        let server = Server::start(None, |_| {});
        let mut first = attached(&server).await;
        phux_server_testkit::send_frame(&mut first, &keyed("read _", key(6)).frame(1)).await;
        drop(first);

        let mut second = attached(&server).await;
        match spawn_resource(&mut second, 2, keyed("read _", key(6))).await {
            SpawnResult::Ok(_) => {}
            SpawnResult::Replayed { id, .. } => {
                assert!(
                    find(&state(&mut second, 100).await, &id).is_some(),
                    "a replay names a live pane"
                );
            }
            other => panic!("expected a fresh or replayed spawn, got {other:?}"),
        }

        drop(second);
        server.stop().await;
    });
}

fn create_body(name: &str, token: &str) -> serde_json::Value {
    serde_json::json!({ "name": name, "request_token": token, "empty": true })
}

/// A repeated `request_token` republishes the original result (not a
/// duplicate-name refusal), from any connection and in any letter case,
/// under the repeat's own spelling; the same token naming another session
/// creates nothing.
#[test]
fn session_create_with_a_repeated_request_token_replays_its_result() {
    phux_server_testkit::run_local(async {
        let server = Server::start(Some("main"), |_| {});
        let mut first = server.connect().await;
        send_create(&mut first, 1, &create_body("idem", TOKEN)).await;
        let original = create_result(&mut first, 2, TOKEN)
            .await
            .expect("the create publishes");
        assert_eq!(original["name"], "idem");
        send_create(&mut first, 3, &create_body("idem", TOKEN)).await;
        assert_eq!(
            create_result(&mut first, 4, TOKEN).await.as_ref(),
            Some(&original)
        );

        let mut second = server.connect().await;
        send_create(&mut second, 1, &create_body("idem", TOKEN)).await;
        assert_eq!(
            create_result(&mut second, 2, TOKEN).await.as_ref(),
            Some(&original)
        );
        send_create(&mut second, 3, &create_body("other", TOKEN)).await;
        assert_eq!(create_result(&mut second, 4, TOKEN).await, None);

        let upper = TOKEN.to_ascii_uppercase();
        send_create(&mut second, 5, &create_body("idem", &upper)).await;
        let replayed = create_result(&mut second, 6, &upper)
            .await
            .expect("published under the repeat's spelling");
        assert_eq!(replayed["request_token"], upper.as_str());
        for field in ["name", "session_id", "terminal_id", "empty"] {
            assert_eq!(replayed[field], original[field], "{field}");
        }
        assert_eq!(
            create_result(&mut second, 7, TOKEN).await,
            None,
            "nothing under the original spelling"
        );

        drop((first, second));
        server.stop().await;
    });
}

/// A keyed kill repeated after it killed answers the first `OK` (unkeyed it
/// would find nothing), the same key naming another target is refused and
/// kills nothing, and the key rides the kill's `pane_closed`.
#[test]
fn keyed_kill_runs_once_and_its_key_rides_the_close() {
    phux_server_testkit::run_local(async {
        let server = Server::start(None, |_| {});
        let mut watcher = server.connect().await;
        subscribe(&mut watcher, 1, None, Some(u64::MAX)).await;
        let mut stream = attached(&server).await;
        let target = spawned(&mut stream, 1, sh("read _")).await;
        let bystander = spawned(&mut stream, 2, sh("read _")).await;
        let kill = |pane: &ResourceId, key| Command::KillResource {
            terminal_id: pane.clone(),
            operation_id: key,
        };

        assert_eq!(
            command(&mut stream, 3, kill(&target, key(1))).await,
            CommandResult::Ok
        );
        let closed = next_event(&mut watcher, |e: &Seen| {
            e.terminal.as_ref() == Some(&target)
                && matches!(e.event, AgentEvent::ResourceClosed { .. })
        })
        .await;
        let stamp = closed.stamp.expect("journaled");
        assert_eq!(stamp.operation_id, key(1), "the kill's key rides its close");
        assert!(
            stamp.actor.is_some(),
            "and names the connection that killed"
        );
        wait_for_state(&mut stream, 100, "the kill to reap", |s| {
            find(s, &target).is_none()
        })
        .await;

        assert!(matches!(
            command(&mut stream, 4, kill(&target, None)).await,
            CommandResult::Error {
                code: ErrorCode::TerminalNotFound,
                ..
            }
        ));
        assert_eq!(
            command(&mut stream, 5, kill(&target, key(1))).await,
            CommandResult::Ok
        );
        let CommandResult::Error { code, message } =
            command(&mut stream, 6, kill(&bystander, key(1))).await
        else {
            panic!("the same key naming another target must be refused");
        };
        assert_eq!(code, ErrorCode::InvalidCommand);
        assert!(message.contains("idempotency conflict"), "{message}");
        assert!(
            find(&state(&mut stream, 200).await, &bystander).is_some(),
            "nothing else died"
        );

        drop((watcher, stream));
        server.stop().await;
    });
}

/// A keyed spawn's `pane_spawned` carries its key; a replay journals nothing;
/// a keyed session create's seed pane carries its token.
#[test]
fn pane_spawned_carries_the_operation_id_and_replays_journal_nothing() {
    phux_server_testkit::run_local(async {
        let server = Server::start(None, |_| {});
        let mut watcher = server.connect().await;
        subscribe(&mut watcher, 1, None, Some(u64::MAX)).await;
        let mut spawner = attached(&server).await;
        let is_spawn = |e: &Seen| matches!(e.event, AgentEvent::ResourceSpawned { .. });

        let id = spawned(&mut spawner, 1, keyed("read _", key(5))).await;
        let event = next_event(&mut watcher, |e| {
            is_spawn(e) && e.terminal.as_ref() == Some(&id)
        })
        .await;
        assert_eq!(event.stamp.and_then(|s| s.operation_id), key(5));

        assert!(matches!(
            spawn_resource(&mut spawner, 2, keyed("read _", key(5))).await,
            SpawnResult::Replayed { .. }
        ));
        let unkeyed = spawned(&mut spawner, 3, sh("read _")).await;
        let event = next_event(&mut watcher, is_spawn).await;
        assert_eq!(
            event.terminal,
            Some(unkeyed),
            "the replay journaled no second pane_spawned"
        );
        assert_eq!(event.stamp.and_then(|s| s.operation_id), None);

        let body = serde_json::json!({
            "name": "keyed",
            "command": ["/bin/sh", "-c", "read _"],
            "request_token": TOKEN,
        });
        send_create(&mut spawner, 4, &body).await;
        let event = next_event(&mut watcher, is_spawn).await;
        let token_key = IdempotencyKey::new(std::array::from_fn(|i| u8::try_from(i + 1).unwrap()));
        assert_eq!(event.stamp.and_then(|s| s.operation_id), token_key);

        drop((watcher, spawner));
        server.stop().await;
    });
}
