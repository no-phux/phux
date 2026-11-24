//! `mission start`: one run, and a binding when a worker was launched.
//!
//! The digest covers the task, the agent, the cap, the expected revision,
//! and the planned worktree. It does not cover the terminal id, so a retry
//! of the same operation returns the first launch.

use phux_protocol::api::ExecutionRef;
use phux_protocol::ids::AuthorityId;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::error::Error;
use crate::id::{Alias, ObjectiveState, OpaqueId, OperationId, RunState};
use crate::operation::{load_operation, replay};
use crate::store::{
    Objective, Store, bump, digest, load_checked, project_state, timestamp, write_objective,
};

mod binding;
use binding::{StoredBinding, insert_binding, map_open_run, validate_binding_authority};

mod receipt;
mod request;
mod runs;
use receipt::{StartSnapshot, applied_from, record_start};
use request::{legacy_start_matches, start_digest, validate_start};
use runs::{
    bound_holder, count_runs, insert_intent, insert_run, load_checked_conn, next_ordinal,
    oldest_queued, refuse_unstartable,
};

/// What a started run is bound to. Absent when the task is only queued.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StartBinding {
    /// Organization session key, base32 text.
    pub session_key: String,
    /// Session name.
    pub session: String,
    /// Live pane at launch, if any; a display hint, never durable identity.
    pub terminal_id: Option<String>,
    /// Authority-qualified execution. Absent only on legacy receipts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionRef>,
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
    /// Git repository root. Empty when the caller has none. Kept so a queued
    /// run can be launched later without the keeper's working directory.
    #[serde(default)]
    pub repo_root: String,
    /// Owning organization authority selected before creating a worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<AuthorityId>,
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
    pub run_state: RunState,
    /// `started` or `queued`.
    pub disposition: String,
    /// Set when `disposition` is `queued`.
    pub queue_position: Option<u64>,
    /// Set when a worker was launched.
    pub binding: Option<StartBinding>,
}

/// A live run the keeper can project or launch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OpenRun {
    /// Display alias.
    pub alias: Alias,
    /// Objective id.
    pub objective_id: OpaqueId,
    /// Objective revision.
    pub revision: u64,
    /// Objective state.
    pub state: ObjectiveState,
    /// Run id.
    pub run_id: OpaqueId,
    /// `running` or `queued`.
    pub run_state: RunState,
    /// Per-task attempt number.
    pub ordinal: u64,
    /// Harness kind.
    pub agent_kind: String,
    /// Model, or empty.
    pub model: String,
    /// Cap recorded with the run.
    pub max_agents: u32,
    /// Git repository root. Empty when the run predates launch intent.
    pub repo_root: String,
    /// Pinned owner of queued or running work. Legacy launch intents are unpinned.
    pub authority: Option<AuthorityId>,
    /// Worktree path from the binding, or the path the queue would use.
    pub worktree_path: String,
    /// Branch from the binding, or the branch the queue would use.
    pub branch: String,
    /// Pane observed at launch, retained only as an event wake-up hint.
    pub terminal_id: Option<String>,
    /// Organization session key. Absent while queued.
    pub session_key: Option<String>,
    /// Exact execution accepted from the owner; old pane IDs are not evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionRef>,
    /// An unresolved `credential_lost` signal is open.
    pub credential_lost: bool,
}

/// Admit the oldest queued run once a worker pane exists.
///
/// `commit_start` rechecks the cap. Replaying `operation_id` returns the
/// first launch and does not read a different terminal id.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LaunchQueued {
    /// Task whose queued run should start.
    pub alias: Alias,
    /// Fail when the stored revision is not this value. Required.
    pub expected_revision: u64,
    /// Idempotency key.
    pub operation_id: OperationId,
    /// Recorded on the event row.
    pub actor: String,
    /// Cap on running agents.
    pub max_agents: u32,
    /// The worker pane that was launched.
    pub launch: StartBinding,
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
        let running = count_runs(&self.conn, RunState::Running)?;
        if running >= u64::from(request.max_agents) {
            let queued = count_runs(&self.conn, RunState::Queued)?;
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
            Some((stored, result))
                if stored == digest || legacy_start_matches(request, &stored)? =>
            {
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
    pub fn commit_start(&mut self, mut request: StartRequest) -> Result<StartApplied, Error> {
        let actor = crate::id::parse_actor(&request.actor)?;
        let digest = start_digest(&request)?;
        validate_start(&request)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((stored, result)) = load_operation(&tx, &request.operation_id)? {
            if stored != digest && !legacy_start_matches(&request, &stored)? {
                return Err(Error::OperationConflict {
                    operation_id: request.operation_id.to_hex(),
                });
            }
            let snapshot: StartSnapshot = serde_json::from_str(&result)?;
            return Ok(applied_from(snapshot, request.operation_id, true));
        }
        let launch = request
            .launch
            .take()
            .map(StoredBinding::from_public)
            .transpose()?;
        let current = load_checked(&tx, &request.alias, request.expected_revision)?;
        if request.authority.is_none() {
            return Err(Error::invalid_text(
                "authority",
                "start requires an owning authority",
            ));
        }
        refuse_unstartable(&tx, &current, &request.worktree_path, &request.branch)?;
        let running = count_runs(&tx, RunState::Running)?;
        let launching = launch.is_some();
        if let Some(binding) = &launch {
            validate_binding_authority(binding, request.authority)?;
        }
        let limit = u64::from(request.max_agents);
        if launching && running >= limit {
            return Err(Error::AtCapacity { running, limit });
        }
        let now = timestamp();
        let ordinal = next_ordinal(&tx, current.id)?;
        let run_id = OpaqueId::generate()?;
        let run_state = if launching {
            RunState::Running
        } else {
            RunState::Queued
        };
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
        if let Some(binding) = &launch {
            insert_binding(&tx, run_id, binding)?;
        }
        insert_intent(&tx, run_id, &request)?;
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
            Some(count_runs(&tx, RunState::Queued)?)
        };
        let snapshot = StartSnapshot {
            objective: updated,
            agent_kind: request.agent_kind,
            model: request.model,
            run_id,
            ordinal,
            run_state,
            disposition: disposition.to_owned(),
            queue_position,
            binding: launch,
        };
        record_start(
            &tx,
            request.operation_id,
            &digest,
            &actor,
            "start",
            "run_started",
            &snapshot,
        )?;
        tx.commit()?;
        Ok(applied_from(snapshot, request.operation_id, false))
    }

    /// Live runs, oldest first. Queued runs carry no terminal id.
    pub fn open_runs(&self) -> Result<Vec<OpenRun>, Error> {
        let mut statement = self.conn.prepare(
            "SELECT objective.display_alias, objective.id, objective.revision, objective.state,
                    run.id, run.state, run.ordinal, run.agent_kind,
                    COALESCE(launch_intent.model, ''),
                    COALESCE(launch_intent.max_agents, 0),
                    COALESCE(launch_intent.repo_root, ''),
                    COALESCE(binding.worktree_path, launch_intent.worktree_path, ''),
                    COALESCE(binding.branch, launch_intent.branch, ''),
                    binding.terminal_id, binding.session_key, binding.execution,
                    launch_intent.authority,
                    EXISTS (
                        SELECT 1 FROM signal
                        WHERE signal.objective_id = objective.id
                          AND signal.kind = 'credential_lost'
                          AND signal.resolved_at IS NULL
                    )
             FROM run
             JOIN objective ON objective.id = run.objective_id
             LEFT JOIN binding ON binding.run_id = run.id
             LEFT JOIN launch_intent ON launch_intent.run_id = run.id
             WHERE run.ended_at IS NULL AND run.state IN ('running', 'queued')
               AND objective.archived = 0
             ORDER BY run.started_at, run.rowid",
        )?;
        let rows = statement.query_map([], map_open_run)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    }

    /// The stored launch when `operation_id` repeats an admitted promotion.
    pub fn replay_launch(&self, operation_id: &OperationId) -> Result<Option<StartApplied>, Error> {
        match load_operation(&self.conn, operation_id)? {
            None => Ok(None),
            Some((_, result)) => {
                let snapshot: StartSnapshot = serde_json::from_str(&result)?;
                Ok(Some(applied_from(snapshot, *operation_id, true)))
            }
        }
    }

    /// Move the oldest queued run to `running` and bind `request.launch`.
    ///
    /// The cap is rechecked. A launch that arrives after the cap filled is
    /// [`Error::AtCapacity`] with nothing written. Replaying the operation
    /// id returns the first result.
    pub fn launch_queued(&mut self, request: LaunchQueued) -> Result<StartApplied, Error> {
        let actor = crate::id::parse_actor(&request.actor)?;
        if request.max_agents == 0 {
            return Err(Error::invalid_text("max_agents", "must be at least 1"));
        }
        let digest = digest(
            "launch",
            &serde_json::json!({
                "alias": request.alias.display(),
                "branch": &request.launch.branch,
                "expected_revision": request.expected_revision,
                "max_agents": request.max_agents,
                "worktree_path": &request.launch.worktree_path,
            }),
        )?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(snapshot) = replay(&tx, &request.operation_id, &digest)? {
            return Ok(applied_from(snapshot, request.operation_id, true));
        }
        let current = load_checked(&tx, &request.alias, request.expected_revision)?;
        let queued = oldest_queued(&tx, current.id)?;
        let Some((run_id, ordinal, agent_kind, model)) = queued else {
            return Err(Error::invalid_text("run", "is not queued"));
        };
        let authority: Option<String> = tx
            .query_row(
                "SELECT authority FROM launch_intent WHERE run_id = ?1",
                params![run_id.as_slice()],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let launch = StoredBinding::from_public(request.launch)?;
        validate_binding_authority(&launch, authority.as_deref().and_then(AuthorityId::parse))?;
        if let Some((alias, taken)) =
            bound_holder(&tx, current.id, &launch.worktree_path, &launch.branch)?
        {
            return Err(Error::WorktreeBound { path: taken, alias });
        }
        let running = count_runs(&tx, RunState::Running)?;
        let limit = u64::from(request.max_agents);
        if running >= limit {
            return Err(Error::AtCapacity { running, limit });
        }
        tx.execute(
            "UPDATE run SET state = 'running' WHERE id = ?1 AND state = 'queued' AND ended_at IS NULL",
            params![run_id.as_slice()],
        )?;
        insert_binding(&tx, run_id, &launch)?;
        let projected = project_state(&tx, current.id, ObjectiveState::Working)?;
        let previous = current.revision;
        let mut updated = current;
        updated.state = projected;
        updated.revision = bump(previous)?;
        updated.updated_at = timestamp();
        write_objective(&tx, &updated, previous)?;
        let event_kind = if projected == ObjectiveState::NeedsYou {
            "needs_you"
        } else {
            "launched"
        };
        let snapshot = StartSnapshot {
            objective: updated,
            agent_kind,
            model,
            run_id,
            ordinal,
            run_state: RunState::Running,
            disposition: "started".to_owned(),
            queue_position: None,
            binding: Some(launch),
        };
        record_start(
            &tx,
            request.operation_id,
            &digest,
            &actor,
            "launch",
            event_kind,
            &snapshot,
        )?;
        tx.commit()?;
        Ok(applied_from(snapshot, request.operation_id, false))
    }
}

#[cfg(test)]
mod tests;
