use phux_client_core::org_ops::OrgOutcome;
use phux_client_runtime::control::mint_operation;
use phux_protocol::ids::{OperationId, ResourceKey, Revision, SessionKey};
use phux_protocol::wire::frame::{
    ArrangementDoc, Command, Committed, MoveItem, OrgFilter, Refusal, ResourceRecord, SessionRecord,
};

use crate::attach::connection::Connection;

use super::inventory::{OrgInventory, arrangement, fetch};
use super::{MAX_CAS_ATTEMPTS, OrgError, outcome_error, request};

/// First request id the helpers below allocate from; clear of the small ids
/// other verbs use on the same connection.
pub(super) const FIRST_REQUEST_ID: u32 = 0x6f00;

/// Re-read one session's records (`OrgFilter::Session`).
async fn session_view(
    conn: &mut Connection,
    session: SessionKey,
) -> Result<OrgInventory, OrgError> {
    fetch(conn, OrgFilter::Session(session), FIRST_REQUEST_ID).await
}

fn resource_in(view: &OrgInventory, key: &ResourceKey) -> Result<ResourceRecord, OrgError> {
    view.resource(key)
        .cloned()
        .ok_or_else(|| OrgError::NotFound(key.to_string()))
}

fn session_in(view: &OrgInventory, key: &SessionKey) -> Result<SessionRecord, OrgError> {
    view.session(key)
        .cloned()
        .ok_or_else(|| OrgError::NotFound(key.to_string()))
}

/// Find the session a resource belongs to, then re-read that session.
async fn resource_view(
    conn: &mut Connection,
    resource: ResourceKey,
) -> Result<(OrgInventory, ResourceRecord), OrgError> {
    let all = fetch(conn, OrgFilter::All, FIRST_REQUEST_ID).await?;
    let session = resource_in(&all, &resource)?.session;
    let view = session_view(conn, session).await?;
    let record = resource_in(&view, &resource)?;
    Ok((view, record))
}

/// One compare-and-swap edit, built afresh from current records on every
/// attempt.
#[derive(Debug, Clone, Copy)]
pub(super) enum Edit<'a> {
    Relaunch(ResourceKey),
    Move {
        resource: ResourceKey,
        to: SessionKey,
    },
    MoveMany {
        resources: &'a [ResourceKey],
        to: SessionKey,
    },
    IntakeResolve(SessionKey),
    ReplaceArrangement(SessionKey, &'a ArrangementDoc),
}

/// The command for `edit` against freshly read records, under a fresh
/// operation id.
async fn build(conn: &mut Connection, edit: Edit<'_>) -> Result<Command, OrgError> {
    let operation = mint_operation();
    Ok(match edit {
        Edit::Relaunch(resource) => {
            let (_, record) = resource_view(conn, resource).await?;
            Command::ResourceRelaunch {
                operation,
                resource,
                expected: record.revision,
            }
        }
        Edit::IntakeResolve(session) => {
            let record = session_in(&session_view(conn, session).await?, &session)?;
            Command::intake_resolve(operation, session, record.revision)
        }
        Edit::Move { resource, to } => {
            let (from_view, record) = resource_view(conn, resource).await?;
            let from = session_in(&from_view, &record.session)?;
            let to_record = session_in(&session_view(conn, to).await?, &to)?;
            Command::ResourceMove {
                operation,
                resource,
                from: from.key,
                from_expected: from.revision,
                to,
                to_expected: to_record.revision,
            }
        }
        Edit::ReplaceArrangement(session, document) => {
            replace_command(conn, operation, session, document).await?
        }
        Edit::MoveMany { resources, to } => {
            let all = fetch(conn, OrgFilter::All, FIRST_REQUEST_ID).await?;
            let to_record = session_in(&all, &to)?;
            let mut items = Vec::with_capacity(resources.len());
            for key in resources {
                let record = resource_in(&all, key)?;
                let from = session_in(&all, &record.session)?;
                items.push(MoveItem {
                    resource: *key,
                    from: from.key,
                    from_expected: from.revision,
                });
            }
            Command::ResourceMoveMany {
                operation,
                items,
                to,
                to_expected: to_record.revision,
            }
        }
    })
}

/// `ARRANGEMENT_REPLACE` of `session`'s arrangement at its current revision.
async fn replace_command(
    conn: &mut Connection,
    operation: OperationId,
    session: SessionKey,
    document: &ArrangementDoc,
) -> Result<Command, OrgError> {
    let record = session_in(&session_view(conn, session).await?, &session)?;
    let key = record
        .arrangement
        .ok_or_else(|| OrgError::NotFound(format!("arrangement of {session}")))?;
    let current = arrangement(conn, FIRST_REQUEST_ID, key).await?;
    Ok(Command::ArrangementReplace {
        operation,
        arrangement: key,
        expected: current.revision,
        document: document.clone(),
    })
}

/// Run `edit` against fresh records until it does not conflict, at most
/// [`MAX_CAS_ATTEMPTS`] times. An `ACCEPTED` reply (a relaunch whose start
/// follows as events) reads as a receipt under the command's operation.
pub(super) async fn run_cas(conn: &mut Connection, edit: Edit<'_>) -> Result<Committed, OrgError> {
    let mut current = Revision::default();
    for _ in 0..MAX_CAS_ATTEMPTS {
        let command = build(conn, edit).await?;
        let operation = command.idempotency_key().copied();
        match request(conn, FIRST_REQUEST_ID, command).await?.0 {
            OrgOutcome::Committed(committed) => return Ok(*committed),
            OrgOutcome::Accepted => {
                return operation
                    .map(|operation| Committed::new(operation, Revision::default()))
                    .ok_or_else(|| {
                        OrgError::Unexpected("accepted without an operation".to_owned())
                    });
            }
            OrgOutcome::Refused(Refusal::Conflict { current: now }) => current = now,
            other => return Err(outcome_error(other)),
        }
    }
    Err(OrgError::Refused(Refusal::Conflict { current }))
}
