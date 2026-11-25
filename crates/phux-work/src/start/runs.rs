//! Run admission queries and transaction-local run/launch-intent writes.

use super::{Alias, Error, Objective, OpaqueId, RunState, StartRequest};
use crate::store::load_alias_conn;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

pub(super) fn load_checked_conn(
    conn: &Connection,
    alias: &Alias,
    expected: u64,
) -> Result<Objective, Error> {
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

pub(super) fn refuse_unstartable(
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

pub(super) fn count_runs(conn: &Connection, state: RunState) -> Result<u64, Error> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM run WHERE state = ?1 AND ended_at IS NULL",
        params![state.as_str()],
        |row| row.get(0),
    )?;
    u64::try_from(count).map_err(|_| Error::Corrupt("run count".to_owned()))
}

pub(super) fn bound_holder(
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

pub(super) fn next_ordinal(tx: &Transaction<'_>, id: OpaqueId) -> Result<u64, Error> {
    let max: Option<i64> = tx.query_row(
        "SELECT MAX(ordinal) FROM run WHERE objective_id = ?1",
        params![id.as_slice()],
        |row| row.get(0),
    )?;
    let max = u64::try_from(max.unwrap_or(0)).map_err(|_| Error::Corrupt("ordinal".to_owned()))?;
    max.checked_add(1)
        .ok_or_else(|| Error::Corrupt("ordinal wrapped".to_owned()))
}

pub(super) fn insert_run(
    tx: &Transaction<'_>,
    id: OpaqueId,
    objective_id: OpaqueId,
    ordinal: u64,
    agent_kind: &str,
    state: RunState,
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
            state.as_str(),
            now
        ],
    )?;
    Ok(())
}

pub(super) fn insert_intent(
    tx: &Transaction<'_>,
    run_id: OpaqueId,
    request: &StartRequest,
) -> Result<(), Error> {
    let max_agents = i64::from(request.max_agents);
    tx.execute(
        "INSERT INTO launch_intent
            (run_id, model, max_agents, worktree_path, branch, repo_root, authority)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            run_id.as_slice(),
            request.model,
            max_agents,
            request.worktree_path,
            request.branch,
            request.repo_root,
            request.authority.map(|id| id.to_string()),
        ],
    )?;
    Ok(())
}

pub(super) fn oldest_queued(
    tx: &Transaction<'_>,
    objective_id: OpaqueId,
) -> Result<Option<(OpaqueId, u64, String, String)>, Error> {
    let row: Option<(Vec<u8>, i64, String, String)> = tx
        .query_row(
            "SELECT run.id, run.ordinal, run.agent_kind, COALESCE(launch_intent.model, '')
             FROM run
             LEFT JOIN launch_intent ON launch_intent.run_id = run.id
             WHERE run.objective_id = ?1 AND run.state = 'queued' AND run.ended_at IS NULL
             ORDER BY run.started_at, run.rowid
             LIMIT 1",
            params![objective_id.as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((id, ordinal, agent_kind, model)) = row else {
        return Ok(None);
    };
    let ordinal = u64::try_from(ordinal).map_err(|_| Error::Corrupt("ordinal".to_owned()))?;
    Ok(Some((
        OpaqueId::from_slice(&id)?,
        ordinal,
        agent_kind,
        model,
    )))
}
