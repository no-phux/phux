//! Real SQLite upgrades and downgrade fencing for work records.
#![allow(clippy::expect_used, reason = "test fixture assertions")]
use super::{
    AddObjective, Alias, Error, ObjectiveState, OpaqueId, OperationId, Store, params, schema,
};

fn catalog(conn: &rusqlite::Connection) -> Vec<String> {
    let mut statement = conn
        .prepare(
            "SELECT name || ' ' || COALESCE(sql, '')
             FROM sqlite_master
             WHERE name NOT LIKE 'sqlite_%'
             ORDER BY 1",
        )
        .expect("catalog");
    statement
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows")
}

#[test]
fn a_version_1_store_migrates_to_2_without_ddl() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("work-v1.sqlite");
    let before = {
        let conn = rusqlite::Connection::open(&path).expect("sqlite");
        schema::install_version_one(&conn).expect("v1");
        let id = OpaqueId::generate().expect("id");
        conn.execute(
            "INSERT INTO objective (
                id, repo, alias_number, display_alias, title, description,
                state, revision, created_at, updated_at, archived
             ) VALUES (?1, 'phux', 1, 'phux/1', 'Billing', '', 'failed', 1, ?2, ?2, 0)",
            params![id.as_slice(), "2020-01-01T00:00:00Z"],
        )
        .expect("row");
        catalog(&conn)
    };
    let store = Store::open_as(&path, 2).expect("migrate to v2");
    assert_eq!(store.user_version().expect("version"), 2);
    let shown = store
        .show(&Alias::parse("phux/1").expect("alias"))
        .expect("show");
    assert_eq!(shown.state, ObjectiveState::Failed);
    drop(store);
    let conn = rusqlite::Connection::open(&path).expect("sqlite");
    assert_eq!(catalog(&conn), before);
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("version");
    assert_eq!(version, 2);
}

#[test]
fn v2_migration_preserves_the_binding_without_guessing_an_execution() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("work-v2.sqlite");
    let mut old = Store::open_as(&path, 2).expect("v2");
    let task = old
        .add(AddObjective {
            repo: "phux".to_owned(),
            title: "One".to_owned(),
            description: String::new(),
            operation_id: OperationId::generate().expect("operation"),
            actor: "test".to_owned(),
        })
        .expect("add");
    let run = OpaqueId::generate().expect("run id");
    let binding = OpaqueId::generate().expect("binding id");
    old.conn
        .execute(
            "INSERT INTO run (id, objective_id, ordinal, agent_kind, state)
             VALUES (?1, ?2, 1, 'fake', 'running')",
            params![run.as_slice(), task.objective.id.as_slice()],
        )
        .expect("legacy run");
    old.conn
        .execute(
            "INSERT INTO binding (id, run_id, terminal_id, session_key, worktree_path, branch)
             VALUES (?1, ?2, '7', 'old-session', '/tmp/mission', 'mission/1')",
            params![binding.as_slice(), run.as_slice()],
        )
        .expect("legacy binding");
    drop(old);
    let migrated = Store::open(&path).expect("migrate");
    let open = migrated.open_runs().expect("runs").remove(0);
    assert_eq!(open.run_id, run);
    assert_eq!(open.terminal_id.as_deref(), Some("7"));
    assert_eq!(open.session_key.as_deref(), Some("old-session"));
    assert!(open.execution.is_none());
    assert!(open.authority.is_none());
    drop(migrated);
    assert!(matches!(
        Store::open_as(&path, 2),
        Err(Error::UnsupportedSchema {
            found: 3,
            supported: 2
        })
    ));
}
