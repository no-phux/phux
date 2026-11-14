use super::{LaunchQueued, StartBinding, StartPlan, StartRequest};
use crate::error::Error;
use crate::id::{Alias, ObjectiveState, OperationId, RunState};
use crate::store::{AddObjective, RevisionEdit, StateEdit, Store};

fn open() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("work-v1.sqlite")).expect("open");
    (dir, store)
}

fn add(repo: &str, title: &str) -> AddObjective {
    AddObjective {
        repo: repo.to_owned(),
        title: title.to_owned(),
        description: String::new(),
        operation_id: OperationId::generate().expect("id"),
        actor: "tester".to_owned(),
    }
}

fn start_req(alias: Alias, op: OperationId, launch: bool) -> StartRequest {
    let (worktree_path, branch) = paths_for(&alias);
    StartRequest {
        launch: launch.then(|| binding_for(&alias, &worktree_path, &branch)),
        alias,
        agent_kind: "fake".to_owned(),
        model: String::new(),
        max_agents: 1,
        expected_revision: 1,
        operation_id: op,
        actor: "tester".to_owned(),
        worktree_path,
        branch,
        repo_root: "/tmp/repo".to_owned(),
    }
}

fn paths_for(alias: &Alias) -> (String, String) {
    (
        format!("/tmp/{}", alias.display().replace('/', "-")),
        format!("mission/{}", alias.number),
    )
}

fn alias_of(alias: &Alias) -> String {
    alias.display().replace('/', "-")
}

fn binding_for(alias: &Alias, worktree_path: &str, branch: &str) -> StartBinding {
    StartBinding {
        session_key: "sessionkey".to_owned(),
        session: format!("{}-task", alias.number),
        terminal_id: "7".to_owned(),
        worktree_path: worktree_path.to_owned(),
        branch: branch.to_owned(),
        tab_title: format!("#{} Title", alias.number),
    }
}

fn admit(store: &mut Store, alias: Alias, op: OperationId) -> super::StartApplied {
    store
        .commit_start(start_req(alias, op, true))
        .expect("start")
}

#[test]
fn start_admits_one_runner_then_queues_in_order() {
    let (_dir, mut store) = open();
    let first = store.add(add("phux", "Billing page")).expect("add");
    let second = store.add(add("phux", "Other")).expect("add");
    let op = OperationId::generate().expect("id");
    let admitted = admit(&mut store, first.objective.alias.clone(), op);
    assert!(!admitted.idempotent_replay);
    assert_eq!(admitted.disposition, "started");
    assert_eq!(admitted.run_state, RunState::Running);
    assert_eq!(admitted.objective.state, ObjectiveState::Working);
    assert_eq!(admitted.objective.revision, 2);
    assert!(admitted.binding.is_some());
    let replay = store
        .replay_start(&start_req(first.objective.alias, op, true))
        .expect("replay");
    let replay = replay.expect("stored");
    assert!(replay.idempotent_replay);
    assert_eq!(replay.run_id, admitted.run_id);
    assert_eq!(replay.binding, admitted.binding);

    let mut queued_req = start_req(
        second.objective.alias.clone(),
        OperationId::generate().expect("id"),
        false,
    );
    queued_req.expected_revision = second.objective.revision;
    assert_eq!(
        store.plan_start(&queued_req).expect("plan"),
        StartPlan::Queue { position: 1 }
    );
    let queued = store.commit_start(queued_req).expect("queue");
    assert_eq!(queued.disposition, "queued");
    assert_eq!(queued.queue_position, Some(1));
    assert_eq!(queued.objective.state, ObjectiveState::Queued);
    assert!(queued.binding.is_none());

    let third = store.add(add("phux", "Third")).expect("add");
    let mut later = start_req(
        third.objective.alias,
        OperationId::generate().expect("id"),
        false,
    );
    later.expected_revision = third.objective.revision;
    let later = store.commit_start(later).expect("queue 2");
    assert_eq!(later.queue_position, Some(2));
}

#[test]
fn start_refuses_a_stale_revision_a_second_run_and_a_bound_worktree() {
    let (_dir, mut store) = open();
    let added = store.add(add("phux", "Billing")).expect("add");
    let alias = added.objective.alias;
    let mut stale = start_req(alias.clone(), OperationId::generate().expect("id"), false);
    stale.expected_revision = 9;
    assert!(matches!(
        store.plan_start(&stale),
        Err(Error::RevisionConflict { expected: 9, .. })
    ));
    admit(
        &mut store,
        alias.clone(),
        OperationId::generate().expect("id"),
    );
    let mut again = start_req(alias.clone(), OperationId::generate().expect("id"), true);
    again.expected_revision = 2;
    let again = store.commit_start(again).expect_err("second run");
    assert!(matches!(again, Error::AlreadyStarted { .. }));
    assert_eq!(
        again.to_string(),
        format!("{} is already running", alias.display())
    );

    let other = store.add(add("phux", "Taken")).expect("add");
    let mut clash = start_req(
        other.objective.alias,
        OperationId::generate().expect("id"),
        false,
    );
    clash.expected_revision = 1;
    clash.worktree_path = format!("/tmp/{}", alias_of(&alias));
    clash.branch = "somewhere-else".to_owned();
    assert!(matches!(
        store.plan_start(&clash),
        Err(Error::WorktreeBound { .. })
    ));
}

#[test]
fn start_refuses_an_archived_task_and_a_launch_past_the_cap() {
    let (_dir, mut store) = open();
    let added = store.add(add("phux", "Old")).expect("add");
    let alias = added.objective.alias;
    store
        .archive(RevisionEdit {
            alias: alias.clone(),
            expected_revision: 1,
            operation_id: OperationId::generate().expect("id"),
            actor: "tester".to_owned(),
        })
        .expect("archive");
    let mut archived = start_req(alias.clone(), OperationId::generate().expect("id"), false);
    archived.expected_revision = 2;
    let archived = store.plan_start(&archived).expect_err("archived");
    assert!(matches!(archived, Error::NotStartable { .. }));
    assert_eq!(
        archived.to_string(),
        format!("{} is archived; restore it to start it", alias.display())
    );

    let live = store.add(add("phux", "Live")).expect("add");
    admit(
        &mut store,
        live.objective.alias,
        OperationId::generate().expect("id"),
    );
    let extra = store.add(add("phux", "Extra")).expect("add");
    let mut over = start_req(
        extra.objective.alias,
        OperationId::generate().expect("id"),
        true,
    );
    over.expected_revision = 1;
    over.worktree_path = "/tmp/extra-worktree".to_owned();
    over.branch = "mission/extra".to_owned();
    if let Some(binding) = over.launch.as_mut() {
        binding.worktree_path.clone_from(&over.worktree_path);
        binding.branch.clone_from(&over.branch);
    }
    let over = store.commit_start(over).expect_err("cap");
    assert!(matches!(
        over,
        Error::AtCapacity {
            running: 1,
            limit: 1
        }
    ));
    assert_eq!(over.to_string(), "no agent slot free (1 of 1 running)");
}

#[test]
fn a_replayed_start_ignores_a_different_terminal_id() {
    let (_dir, mut store) = open();
    let added = store.add(add("phux", "Billing")).expect("add");
    let op = OperationId::generate().expect("id");
    let first = admit(&mut store, added.objective.alias.clone(), op);
    let mut retry = start_req(added.objective.alias, op, true);
    retry.expected_revision = 1;
    retry.launch.as_mut().expect("binding").terminal_id = "99".to_owned();
    let again = store.commit_start(retry).expect("replay");
    assert!(again.idempotent_replay);
    assert_eq!(
        again.binding.as_ref().expect("binding").terminal_id,
        first.binding.expect("binding").terminal_id
    );
}

#[test]
fn launch_queued_admits_one_runner_and_a_replay_does_not_write() {
    let (_dir, mut store) = open();
    let first = store.add(add("phux", "Billing")).expect("add");
    let second = store.add(add("phux", "Next")).expect("add");
    admit(
        &mut store,
        first.objective.alias.clone(),
        OperationId::generate().expect("id"),
    );
    let mut queued = start_req(
        second.objective.alias.clone(),
        OperationId::generate().expect("id"),
        false,
    );
    queued.expected_revision = second.objective.revision;
    let queued = store.commit_start(queued).expect("queue");
    let blocked = store.launch_queued(LaunchQueued {
        alias: second.objective.alias.clone(),
        expected_revision: queued.objective.revision,
        operation_id: OperationId::derive_bytes(b"promote"),
        actor: "keeper".to_owned(),
        max_agents: 1,
        launch: binding_for(&second.objective.alias, "/tmp/phux-2", "mission/2"),
    });
    assert!(matches!(blocked, Err(Error::AtCapacity { .. })));
    assert_eq!(
        store.show(&second.objective.alias).expect("show").state,
        ObjectiveState::Queued
    );

    let failed = store
        .record_state(StateEdit {
            alias: first.objective.alias.clone(),
            state: ObjectiveState::Failed,
            expected_revision: store.show(&first.objective.alias).expect("show").revision,
            operation_id: OperationId::generate().expect("id"),
            actor: "keeper".to_owned(),
            event_kind: Some("failed".to_owned()),
        })
        .expect("fail");
    assert!(failed.objective.state.frees_slot());
    let open = store.open_runs().expect("open");
    assert!(open.iter().all(|run| run.alias != first.objective.alias));
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].run_state, RunState::Queued);

    let operation_id = OperationId::derive_bytes(b"promote-after");
    let expected = queued.objective.revision;
    let launch = binding_for(&second.objective.alias, "/tmp/phux-2", "mission/2");
    let admitted = store
        .launch_queued(LaunchQueued {
            alias: second.objective.alias.clone(),
            expected_revision: expected,
            operation_id,
            actor: "keeper".to_owned(),
            max_agents: 1,
            launch: launch.clone(),
        })
        .expect("launch");
    assert!(!admitted.idempotent_replay);
    assert_eq!(admitted.objective.state, ObjectiveState::Working);
    assert_eq!(admitted.run_state, RunState::Running);
    assert_eq!(
        admitted.binding.as_ref().expect("binding").terminal_id,
        launch.terminal_id
    );
    let kinds = store.event_kinds().expect("events");
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| kind.as_str() == "launched")
            .count(),
        1
    );
    let replay = store.replay_launch(&operation_id).expect("replay");
    let replay = replay.expect("stored");
    assert!(replay.idempotent_replay);
    assert_eq!(store.event_kinds().expect("events"), kinds);
    let mut other = launch.clone();
    other.terminal_id = "99".to_owned();
    let again = store
        .launch_queued(LaunchQueued {
            alias: second.objective.alias,
            expected_revision: expected,
            operation_id,
            actor: "keeper".to_owned(),
            max_agents: 1,
            launch: other,
        })
        .expect("replay write");
    assert!(again.idempotent_replay);
    assert_eq!(
        again.binding.expect("binding").terminal_id,
        launch.terminal_id
    );
}
