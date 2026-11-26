//! Independent released receipts, schema readers and canonical pin bytes.

use super::super::start_digest;
use super::*;

// Independently fixed released JSON, not serialized from a public ExecutionRef.
const OLD_PIN: &str = r#"{"authority":"aebagbafaydqqcikbmga2dqpca","resource":"aebagbafaydqqcikbmga2dqpca","activation":37,"instance":"abcdef0123456789abcdef0123456789"}"#;
const OLD_BINDING: &str = r#"{"session_key":"aebagbafaydqqcikbmga2dqpca","session":"old worker","terminal_id":"19","execution":{"authority":"aebagbafaydqqcikbmga2dqpca","resource":"aebagbafaydqqcikbmga2dqpca","activation":37,"instance":"abcdef0123456789abcdef0123456789"},"worktree_path":"/old/work","branch":"mission/old","tab_title":"Old task"}"#;

#[test]
fn released_binding_and_receipt_reopen_across_schema_versions() {
    for version in [1, 2, 3] {
        let (dir, mut store) = open();
        let task = store.add(add("phux", "Frozen receipt")).expect("add");
        let request = start_req(
            task.objective.alias,
            OperationId::derive_bytes(b"old-start"),
            true,
        );
        let applied = store.commit_start(request.clone()).expect("start");
        // Only unrelated objective metadata is taken from setup. The receipt grammar,
        // binding, execution and result values are independent released bytes.
        let binding = if version == 3 {
            OLD_BINDING.to_owned()
        } else {
            r#"{"session_key":"aebagbafaydqqcikbmga2dqpca","session":"old worker","terminal_id":"19","worktree_path":"/old/work","branch":"mission/old","tab_title":"Old task"}"#.to_owned()
        };
        let result = format!(
            r#"{{"objective":{},"agent_kind":"released","model":"old-model","run_id":{},"ordinal":4,"run_state":"running","disposition":"started","queue_position":null,"binding":{binding}}}"#,
            serde_json::to_string(&applied.objective).expect("objective"),
            serde_json::to_string(&applied.run_id).expect("run id"),
        );
        let mut legacy_request = request.clone();
        legacy_request.authority = None;
        let legacy_digest = start_digest(&legacy_request).expect("legacy digest");
        store
            .conn
            .execute(
                "UPDATE operation SET result_json = ?1, digest = ?2 WHERE id = ?3",
                rusqlite::params![result, legacy_digest, request.operation_id.as_slice()],
            )
            .expect("old operation");
        store
            .conn
            .execute(
                "UPDATE event SET json = ?1 WHERE operation_id = ?2",
                rusqlite::params![result, request.operation_id.as_slice()],
            )
            .expect("old event");
        store
            .conn
            .execute(
                "UPDATE binding SET execution = ?1, terminal_id = '19'",
                rusqlite::params![OLD_PIN],
            )
            .expect("old binding");
        if version != 3 {
            store
                .conn
                .execute_batch(
                    "DROP INDEX binding_execution;
                 ALTER TABLE binding DROP COLUMN execution;
                 ALTER TABLE launch_intent DROP COLUMN authority;",
                )
                .expect("released schema");
        }
        store
            .conn
            .pragma_update(None, "user_version", version)
            .expect("version");
        drop(store);
        let reopened = Store::open(&dir.path().join("work-v1.sqlite")).expect("migrate/reopen");
        let run = reopened.open_runs().expect("open run").remove(0);
        let replay = reopened
            .replay_start(&request)
            .expect("legacy digest")
            .expect("receipt");
        assert!(replay.idempotent_replay);
        assert_eq!(replay.agent_kind, "released");
        assert_eq!(replay.model, "old-model");
        assert_eq!(replay.ordinal, 4);
        let receipt = replay.binding.expect("binding");
        assert_eq!(receipt.session, "old worker");
        assert_eq!(receipt.terminal_id.as_deref(), Some("19"));
        assert_eq!(receipt.worktree_path, "/old/work");
        assert_eq!(receipt.branch, "mission/old");
        if version == 3 {
            for pin in [
                run.execution.expect("row pin"),
                receipt.execution.expect("receipt pin"),
            ] {
                assert_eq!(pin.authority, "aebagbafaydqqcikbmga2dqpca");
                assert_eq!(pin.resource, "aebagbafaydqqcikbmga2dqpca");
                assert_eq!(pin.activation, 37);
                assert_eq!(pin.instance, "abcdef0123456789abcdef0123456789");
            }
            assert_eq!(run.authority, request.authority);
        } else {
            assert!(run.execution.is_none());
            assert!(run.authority.is_none());
            assert!(receipt.execution.is_none());
        }
        assert_eq!(
            reopened
                .replay_launch(&request.operation_id)
                .expect("launch replay")
                .expect("receipt")
                .ordinal,
            4
        );
    }
}

#[test]
fn keeper_normalizes_unique_index_bytes_and_rejects_invalid_retained_pins() {
    let (_dir, mut store) = open();
    let task = store.add(add("phux", "Canonical")).expect("add");
    let mut request = start_req(
        task.objective.alias,
        OperationId::derive_bytes(b"canonical"),
        true,
    );
    let pin = request
        .launch
        .as_mut()
        .expect("launch")
        .execution
        .as_mut()
        .expect("pin");
    pin.resource = "aebagbafaydqqcikbmga2dqpca".to_owned();
    pin.activation = 37;
    pin.instance = "ABCDEF0123456789ABCDEF0123456789".to_owned();
    let applied = store.commit_start(request.clone()).expect("start");
    let bytes: String = store
        .conn
        .query_row("SELECT execution FROM binding", [], |row| row.get(0))
        .expect("stored bytes");
    assert_eq!(bytes, OLD_PIN);
    assert_eq!(
        applied
            .binding
            .expect("binding")
            .execution
            .expect("pin")
            .instance,
        "abcdef0123456789abcdef0123456789"
    );
    let second = store
        .add(add("phux", "Duplicate pin"))
        .expect("second task");
    let mut duplicate = start_req(
        second.objective.alias,
        OperationId::derive_bytes(b"duplicate-pin"),
        true,
    );
    duplicate.max_agents = 2;
    duplicate.launch.as_mut().expect("launch").execution = request
        .launch
        .as_ref()
        .expect("original launch")
        .execution
        .clone();
    duplicate
        .launch
        .as_mut()
        .expect("launch")
        .execution
        .as_mut()
        .expect("pin")
        .instance = "abcdef0123456789abcdef0123456789".to_owned();
    assert!(
        store.commit_start(duplicate).is_err(),
        "same normalized execution is unique"
    );
    assert_eq!(store.open_runs().expect("only original run").len(), 1);
    let result: String = store
        .conn
        .query_row(
            "SELECT result_json FROM operation WHERE id = ?1",
            rusqlite::params![request.operation_id.as_slice()],
            |row| row.get(0),
        )
        .expect("receipt bytes");
    for (from, to) in [
        (r#""activation":37"#, r#""activation":0"#),
        (
            "abcdef0123456789abcdef0123456789",
            "00000000000000000000000000000000",
        ),
        ("aebagbafaydqqcikbmga2dqpca", "invalid"),
    ] {
        store
            .conn
            .execute(
                "UPDATE binding SET execution = ?1",
                rusqlite::params![OLD_PIN.replace(from, to)],
            )
            .expect("corrupt row");
        assert!(store.open_runs().is_err(), "invalid stored identity: {to}");
        store
            .conn
            .execute(
                "UPDATE operation SET result_json = ?1 WHERE id = ?2",
                rusqlite::params![result.replace(from, to), request.operation_id.as_slice()],
            )
            .expect("corrupt receipt");
        assert!(
            store.replay_start(&request).is_err(),
            "invalid receipt identity: {to}"
        );
    }
}
