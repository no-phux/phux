//! Frozen start receipts and their public result projection.

use super::{Error, Objective, OpaqueId, OperationId, RunState, StartApplied, StoredBinding};
use rusqlite::{Transaction, params};

pub(super) fn record_start(
    tx: &Transaction<'_>,
    operation: OperationId,
    digest: &str,
    actor: &str,
    operation_kind: &str,
    event_kind: &str,
    snapshot: &StartSnapshot,
) -> Result<(), Error> {
    let result = serde_json::to_string(snapshot)?;
    let objective = &snapshot.objective;
    tx.execute(
        "INSERT INTO operation (id, kind, digest, objective_id, result_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            operation.as_slice(),
            operation_kind,
            digest,
            objective.id.as_slice(),
            result,
            objective.updated_at
        ],
    )?;
    tx.execute(
        "INSERT INTO event (ts, actor, objective_id, operation_id, kind, json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            objective.updated_at,
            actor,
            objective.id.as_slice(),
            operation.as_slice(),
            event_kind,
            result
        ],
    )?;
    Ok(())
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct StartSnapshot {
    pub(super) objective: Objective,
    pub(super) agent_kind: String,
    pub(super) model: String,
    pub(super) run_id: OpaqueId,
    pub(super) ordinal: u64,
    pub(super) run_state: RunState,
    pub(super) disposition: String,
    pub(super) queue_position: Option<u64>,
    pub(super) binding: Option<StoredBinding>,
}

pub(super) fn applied_from(
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
        binding: snapshot.binding.map(StoredBinding::public),
    }
}
