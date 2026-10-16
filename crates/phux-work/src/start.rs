//! `mission start`: one run, and a binding when a worker was launched.
//!
//! The digest covers the task, the agent, the cap, the expected revision,
//! and the planned worktree. It does not cover the terminal id, so a retry
//! of the same operation returns the first launch.

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::error::Error;
use crate::id::{Alias, ObjectiveState, OpaqueId, OperationId};
use crate::store::{
    Objective, Store, bump, digest, load_alias_conn, load_checked, project_state, timestamp,
    write_objective,
};

/// What a started run is bound to. Absent when the task is only queued.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StartBinding {
    /// Organization session key, base32 text.
    pub session_key: String,
    /// Session name.
    pub session: String,
    /// Worker pane id, decimal.
    pub terminal_id: String,
    /// Absolute worktree path.
    pub worktree_path: String,
    /// `mission/<n>-<slug>`.
    pub branch: String,
    /// Worker tab label, such as `#3 Billing page`.
    pub tab_title: String,
}

/// `start` arguments. `launch` is set only when a slot was free and the
/// worker pane exists. `expected_revision` is required.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StartRequest {
    /// Task to start.
    pub alias: Alias,
    /// Harness kind, without the model.
    pub agent_kind: String,
    /// Model, or empty.
    pub model: String,
    /// Cap on running agents. At the cap, the task is queued.
    pub max_agents: u32,
    /// Fail when the stored revision is not this value. Required.
    pub expected_revision: u64,
    /// Idempotency key. The launch facts are not part of the key.
    pub operation_id: OperationId,
    /// Recorded on the event row.
    pub actor: String,
    /// Worktree path this start would use.
    pub worktree_path: String,
    /// Branch this start would use.
    pub branch: String,
    /// Present when the caller has launched a worker.
    pub launch: Option<StartBinding>,
}

/// Whether [`Store::plan_start`] would launch or queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "plan", rename_all = "snake_case")]
pub enum StartPlan {
    /// A slot is free.
    Admit,
    /// The cap is full. `position` is the FIFO place this task would take,
    /// starting at 1.
    Queue {
        /// 1-based place in the queue.
        position: u64,
    },
}

/// Result of [`Store::commit_start`], including a replay of the same operation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StartApplied {
    /// Objective after the write.
    pub objective: Objective,
    /// The operation id the caller sent.
    pub operation_id: OperationId,
    /// True when the id was already on disk and no row changed.
    pub idempotent_replay: bool,
    /// Harness kind.
    pub agent_kind: String,
    /// Model, or empty.
    pub model: String,
    /// Run id.
    pub run_id: OpaqueId,
    /// Per-task attempt number, starting at 1.
    pub ordinal: u64,
    /// `running` or `queued`.
    pub run_state: String,
    /// `started` or `queued`.
    pub disposition: String,
    /// Set when `disposition` is `queued`.
    pub queue_position: Option<u64>,
    /// Set when a worker was launched.
    pub binding: Option<StartBinding>,
}

impl Store {
    /// Read whether `request` would launch or queue. Does not write.
    ///
    /// # Errors
    ///
    /// [`Error::NotStartable`], [`Error::AlreadyStarted`], [`Error::WorktreeBound`],
    /// [`Error::NotFound`], and [`Error::RevisionConflict`].
    pub fn plan_start(&self, request: &StartRequest) -> Result<StartPlan, Error> {
        validate_start(request)?;
        let objective = load_checked_conn(&self.conn, &request.alias, request.expected_revision)?;
        refuse_unstartable(
            &self.conn,
            &objective,
            &request.worktree_path,
            &request.branch,
        )?;
        let running = count_runs(&self.conn, "running")?;
        if running >= u64::from(request.max_agents) {
            let queued = count_runs(&self.conn, "queued")?;
            return Ok(StartPlan::Queue {
                position: queued.saturating_add(1),
            });
        }
        Ok(StartPlan::Admit)
    }

    /// The stored result when `request` repeats an admitted start.
    ///
    /// # Errors
    ///
    /// [`Error::OperationConflict`] when the id was used for a different body.
    pub fn replay_start(&self, request: &StartRequest) -> Result<Option<StartApplied>, Error> {
        let digest = start_digest(request)?;
        match load_operation(&self.conn, &request.operation_id)? {
            None => Ok(None),
            Some((stored, result)) if stored == digest => {
                let snapshot: StartSnapshot = serde_json::from_str(&result)?;
                Ok(Some(applied_from(snapshot, request.operation_id, true)))
            }
            Some(_) => Err(Error::OperationConflict {
                operation_id: request.operation_id.to_hex(),
            }),
        }
    }

    /// Insert the run and, when `launch` is set, its binding.
    ///
    /// The caller passes the revision `plan_start` checked. A queued start
    /// leaves `launch` empty. Replaying the same operation id is a no-op
    /// and does not read the terminal id. The keeper serves one request at
    /// a time, and a launch that arrives after the cap filled is
    /// [`Error::AtCapacity`] with nothing written.
    ///
    /// # Errors
    ///
    /// The errors of [`Store::plan_start`], plus [`Error::AtCapacity`] and
    /// operation conflicts.
    pub fn commit_start(&mut self, request: StartRequest) -> Result<StartApplied, Error> {
        let actor = crate::id::parse_actor(&request.actor)?;
        let digest = start_digest(&request)?;
        validate_start(&request)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((stored, result)) = load_operation(&tx, &request.operation_id)? {
            if stored != digest {
                return Err(Error::OperationConflict {
                    operation_id: request.operation_id.to_hex(),
                });
            }
            let snapshot: StartSnapshot = serde_json::from_str(&result)?;
            return Ok(applied_from(snapshot, request.operation_id, true));
        }
        let current = load_checked(&tx, &request.alias, request.expected_revision)?;
        refuse_unstartable(&tx, &current, &request.worktree_path, &request.branch)?;
        let running = count_runs(&tx, "running")?;
        let launching = request.launch.is_some();
        let limit = u64::from(request.max_agents);
        if launching && running >= limit {
            return Err(Error::AtCapacity { running, limit });
        }
        let now = timestamp();
        let ordinal = next_ordinal(&tx, current.id)?;
        let run_id = OpaqueId::generate()?;
        let run_state = if launching { "running" } else { "queued" };
        let disposition = if launching { "started" } else { "queued" };
        insert_run(
            &tx,
            run_id,
            current.id,
            ordinal,
            &request.agent_kind,
            run_state,
            &now,
        )?;
        if let Some(binding) = &request.launch {
            insert_binding(&tx, run_id, binding)?;
        }
        let requested = if launching {
            ObjectiveState::Working
        } else {
            ObjectiveState::Queued
        };
        let projected = project_state(&tx, current.id, requested)?;
        let previous = current.revision;
        let mut updated = current;
        updated.state = projected;
        updated.revision = bump(previous)?;
        now.clone_into(&mut updated.updated_at);
        write_objective(&tx, &updated, previous)?;
        let queue_position = if launching {
            None
        } else {
            Some(count_runs(&tx, "queued")?)
        };
        let snapshot = StartSnapshot {
            objective: updated,
            agent_kind: request.agent_kind,
            model: request.model,
            run_id,
            ordinal,
            run_state: run_state.to_owned(),
            disposition: disposition.to_owned(),
            queue_position,
            binding: request.launch,
        };
        let result_json = serde_json::to_string(&snapshot)?;
        tx.execute(
            "INSERT INTO operation (id, kind, digest, objective_id, result_json, created_at)
             VALUES (?1, 'start', ?2, ?3, ?4, ?5)",
            params![
                request.operation_id.as_slice(),
                digest,
                snapshot.objective.id.as_slice(),
                &result_json,
                now,
            ],
        )?;
        tx.execute(
            "INSERT INTO event (ts, actor, objective_id, operation_id, kind, json)
             VALUES (?1, ?2, ?3, ?4, 'run_started', ?5)",
            params![
                now,
                actor,
                snapshot.objective.id.as_slice(),
                request.operation_id.as_slice(),
                &result_json,
            ],
        )?;
        tx.commit()?;
        Ok(applied_from(snapshot, request.operation_id, false))
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct StartSnapshot {
    objective: Objective,
    agent_kind: String,
    model: String,
    run_id: OpaqueId,
    ordinal: u64,
    run_state: String,
    disposition: String,
    queue_position: Option<u64>,
    binding: Option<StartBinding>,
}

fn applied_from(
    snapshot: StartSnapshot,
    operation_id: OperationId,
    replayed: bool,
) -> StartApplied {
    StartApplied {
        objective: snapshot.objective,
        operation_id,
        idempotent_replay: replayed,
        agent_kind: snapshot.agent_kind,
        model: snapshot.model,
        run_id: snapshot.run_id,
        ordinal: snapshot.ordinal,
        run_state: snapshot.run_state,
        disposition: snapshot.disposition,
        queue_position: snapshot.queue_position,
        binding: snapshot.binding,
    }
}

fn start_digest(request: &StartRequest) -> Result<String, Error> {
    digest(
        "start",
        &serde_json::json!({
            "agent_kind": &request.agent_kind,
            "alias": request.alias.display(),
            "branch": &request.branch,
            "expected_revision": request.expected_revision,
            "max_agents": request.max_agents,
            "model": &request.model,
            "worktree_path": &request.worktree_path,
        }),
    )
}

fn validate_start(request: &StartRequest) -> Result<(), Error> {
    if !token_ok(&request.agent_kind, 64)
        || (!request.model.is_empty() && !token_ok(&request.model, 128))
    {
        return Err(Error::invalid_text(
            "agent",
            "must look like KIND or KIND/MODEL (letters, digits, `.`, `_`, `-`)",
        ));
    }
    if request.max_agents == 0 {
        return Err(Error::invalid_text("max_agents", "must be at least 1"));
    }
    if request.worktree_path.is_empty() || request.worktree_path.contains('\0') {
        return Err(Error::invalid_text("worktree", "must be a path"));
    }
    if request.branch.is_empty() || request.branch.contains('\0') || request.branch.len() > 256 {
        return Err(Error::invalid_text("branch", "must be a branch name"));
    }
    if let Some(binding) = &request.launch
        && (binding.session.is_empty()
            || binding.worktree_path.is_empty()
            || binding.branch.is_empty())
    {
        return Err(Error::invalid_text("binding", "is incomplete"));
    }
    Ok(())
}

fn token_ok(text: &str, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
}

fn load_operation(
    conn: &Connection,
    operation_id: &OperationId,
) -> Result<Option<(String, String)>, Error> {
    conn.query_row(
        "SELECT digest, result_json FROM operation WHERE id = ?1",
        params![operation_id.as_slice()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(Error::from)
}

fn load_checked_conn(conn: &Connection, alias: &Alias, expected: u64) -> Result<Objective, Error> {
    let objective = load_alias_conn(conn, alias)?;
    if expected != objective.revision {
        return Err(Error::RevisionConflict {
            alias: alias.display(),
            expected,
            actual: objective.revision,
        });
    }
    Ok(objective)
}

fn refuse_unstartable(
    conn: &Connection,
    objective: &Objective,
    worktree_path: &str,
    branch: &str,
) -> Result<(), Error> {
    if objective.archived {
        return Err(Error::NotStartable {
            alias: objective.alias.display(),
        });
    }
    if has_live_run(conn, objective.id)? {
        return Err(Error::AlreadyStarted {
            alias: objective.alias.display(),
        });
    }
    if let Some((alias, taken)) = bound_holder(conn, objective.id, worktree_path, branch)? {
        return Err(Error::WorktreeBound { path: taken, alias });
    }
    Ok(())
}

fn has_live_run(conn: &Connection, id: OpaqueId) -> Result<bool, Error> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM run
             WHERE objective_id = ?1 AND ended_at IS NULL AND state IN ('running', 'queued')
             LIMIT 1",
            params![id.as_slice()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

fn count_runs(conn: &Connection, state: &str) -> Result<u64, Error> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM run WHERE state = ?1 AND ended_at IS NULL",
        params![state],
        |row| row.get(0),
    )?;
    u64::try_from(count).map_err(|_| Error::Corrupt("run count".to_owned()))
}

fn bound_holder(
    conn: &Connection,
    id: OpaqueId,
    worktree_path: &str,
    branch: &str,
) -> Result<Option<(String, String)>, Error> {
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT objective.display_alias, binding.worktree_path, binding.branch
             FROM binding
             JOIN run ON run.id = binding.run_id
             JOIN objective ON objective.id = run.objective_id
             WHERE run.state = 'running' AND run.ended_at IS NULL
               AND objective.archived = 0 AND objective.id != ?1
               AND (binding.worktree_path = ?2 OR binding.branch = ?3)
             LIMIT 1",
            params![id.as_slice(), worktree_path, branch],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    Ok(row.map(|(alias, path, held_branch)| {
        let taken = if path == worktree_path {
            path
        } else {
            held_branch
        };
        (alias, taken)
    }))
}

fn next_ordinal(tx: &Transaction<'_>, id: OpaqueId) -> Result<u64, Error> {
    let max: Option<i64> = tx.query_row(
        "SELECT MAX(ordinal) FROM run WHERE objective_id = ?1",
        params![id.as_slice()],
        |row| row.get(0),
    )?;
    let max = u64::try_from(max.unwrap_or(0)).map_err(|_| Error::Corrupt("ordinal".to_owned()))?;
    max.checked_add(1)
        .ok_or_else(|| Error::Corrupt("ordinal wrapped".to_owned()))
}

fn insert_run(
    tx: &Transaction<'_>,
    id: OpaqueId,
    objective_id: OpaqueId,
    ordinal: u64,
    agent_kind: &str,
    state: &str,
    now: &str,
) -> Result<(), Error> {
    let ordinal = i64::try_from(ordinal).map_err(|_| Error::Corrupt("ordinal".to_owned()))?;
    tx.execute(
        "INSERT INTO run (id, objective_id, ordinal, agent_kind, state, started_at, ended_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
        params![
            id.as_slice(),
            objective_id.as_slice(),
            ordinal,
            agent_kind,
            state,
            now
        ],
    )?;
    Ok(())
}

fn insert_binding(
    tx: &Transaction<'_>,
    run_id: OpaqueId,
    binding: &StartBinding,
) -> Result<(), Error> {
    let id = OpaqueId::generate()?;
    tx.execute(
        "INSERT INTO binding (
            id, run_id, server_id, session_key, terminal_id, worktree_path, branch
         ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6)",
        params![
            id.as_slice(),
            run_id.as_slice(),
            binding.session_key,
            binding.terminal_id,
            binding.worktree_path,
            binding.branch,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{StartBinding, StartPlan, StartRequest};
    use crate::error::Error;
    use crate::id::{Alias, ObjectiveState, OperationId};
    use crate::store::{AddObjective, RevisionEdit, Store};

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
        assert_eq!(admitted.run_state, "running");
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
}
