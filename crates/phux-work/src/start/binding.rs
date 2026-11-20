//! Durable storage and validation of an execution binding.
use phux_protocol::api::{ExecutionRef, hex16};
use phux_protocol::ids::{AuthorityId, SessionKey};
use rusqlite::{Transaction, params};

use super::{Alias, Error, ObjectiveState, OpaqueId, OpenRun, RunState, StartBinding};

pub(super) fn map_open_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<OpenRun> {
    let alias: String = row.get(0)?;
    let objective_id: Vec<u8> = row.get(1)?;
    let revision: i64 = row.get(2)?;
    let state: String = row.get(3)?;
    let run_id: Vec<u8> = row.get(4)?;
    let run_state: String = row.get(5)?;
    let ordinal: i64 = row.get(6)?;
    let max_agents: i64 = row.get(9)?;
    let execution: Option<String> = row.get(15)?;
    let authority: Option<String> = row.get(16)?;
    let credential_lost: i64 = row.get(17)?;
    let stored = |err: Error| rusqlite::Error::ToSqlConversionFailure(Box::new(err));
    let execution = execution
        .as_deref()
        .map(serde_json::from_str::<ExecutionRef>)
        .transpose()
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
    let authority = authority
        .map(|text| {
            AuthorityId::parse(&text).ok_or_else(|| Error::Corrupt("launch authority".to_owned()))
        })
        .transpose()
        .map_err(stored)?;
    Ok(OpenRun {
        alias: Alias::parse(&alias).map_err(stored)?,
        objective_id: OpaqueId::from_slice(&objective_id).map_err(stored)?,
        revision: u64::try_from(revision)
            .map_err(|_| stored(Error::Corrupt("revision".to_owned())))?,
        state: ObjectiveState::parse(&state).map_err(stored)?,
        run_id: OpaqueId::from_slice(&run_id).map_err(stored)?,
        run_state: RunState::parse(&run_state).map_err(stored)?,
        ordinal: u64::try_from(ordinal)
            .map_err(|_| stored(Error::Corrupt("ordinal".to_owned())))?,
        agent_kind: row.get(7)?,
        model: row.get(8)?,
        max_agents: u32::try_from(max_agents).unwrap_or(0),
        repo_root: row.get(10)?,
        authority,
        worktree_path: row.get(11)?,
        branch: row.get(12)?,
        terminal_id: row.get(13)?,
        session_key: row.get(14)?,
        execution,
        credential_lost: credential_lost != 0,
    })
}

pub(super) fn insert_binding(
    tx: &Transaction<'_>,
    run_id: OpaqueId,
    binding: &StartBinding,
) -> Result<(), Error> {
    let pin = binding.execution.as_ref().ok_or_else(|| {
        Error::invalid_text(
            "execution",
            "new worker bindings require an exact execution pin",
        )
    })?;
    let (authority, resource, instance) = pin
        .validated()
        .map_err(|reason| Error::invalid_text("execution", reason))?;
    if SessionKey::parse(&binding.session_key).is_none() {
        return Err(Error::invalid_text(
            "session_key",
            "invalid durable session key",
        ));
    }
    if binding
        .terminal_id
        .as_ref()
        .is_some_and(|id| !id.parse::<u32>().is_ok_and(|id| id > 0))
    {
        return Err(Error::invalid_text(
            "terminal_id",
            "invalid presentation hint",
        ));
    }
    let execution = serde_json::to_string(&ExecutionRef {
        authority: authority.to_string(),
        resource: resource.to_string(),
        activation: pin.activation,
        instance: hex16(&instance),
    })?;
    let id = OpaqueId::generate()?;
    tx.execute(
        "INSERT INTO binding (
            id, run_id, server_id, session_key, terminal_id, worktree_path, branch, execution
         ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7)",
        params![
            id.as_slice(),
            run_id.as_slice(),
            binding.session_key,
            binding.terminal_id,
            binding.worktree_path,
            binding.branch,
            execution,
        ],
    )?;
    Ok(())
}

pub(super) fn validate_binding_authority(
    binding: &StartBinding,
    authority: Option<AuthorityId>,
) -> Result<(), Error> {
    let expected = authority.ok_or_else(|| {
        Error::invalid_text("authority", "worker launch intent does not name an owner")
    })?;
    let actual = binding
        .execution
        .as_ref()
        .and_then(|pin| AuthorityId::parse(&pin.authority));
    if actual == Some(expected) {
        Ok(())
    } else {
        Err(Error::invalid_text(
            "execution",
            "worker receipt does not match the launch authority",
        ))
    }
}
