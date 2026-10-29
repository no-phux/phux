use phux_client_core::org_ops::live_target;
use phux_client_runtime::control::mint_operation;
use phux_protocol::ResourceId;
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::{ResourceKey, ResourceKind, SessionKey};
use phux_protocol::wire::frame::{
    ArrangementDoc, Command, Committed, ExitPolicy, LaunchMode, LaunchSpec, MAX_ORG_BATCH,
    OrgFilter, ResourceRecord, TerminateMode,
};

use crate::attach::connection::Connection;

#[cfg(doc)]
use super::OrgInventory;
use super::edit::{Edit, FIRST_REQUEST_ID, run_cas};
use super::inventory::fetch;
use super::{OrgError, accept, commit};

/// `phux rm` for a resource: remove an inactive resource record.
///
/// # Errors
///
/// `EXECUTION_UNCERTAIN` for a lost activation; a refusal when it still
/// runs; transport failures.
pub async fn remove_resource(
    conn: &mut Connection,
    resource: ResourceKey,
) -> Result<Committed, OrgError> {
    let operation = mint_operation();
    let command = Command::ResourceRemove {
        operation,
        resource,
    };
    commit(conn, FIRST_REQUEST_ID, command).await
}

/// `phux rm` for a session: delete an empty session.
///
/// # Errors
///
/// `NOT_EMPTY` while it has members; transport failures.
pub async fn delete_session(
    conn: &mut Connection,
    session: SessionKey,
) -> Result<Committed, OrgError> {
    let operation = mint_operation();
    let command = Command::SessionDelete { operation, session };
    commit(conn, FIRST_REQUEST_ID, command).await
}

/// Whether the server behind `conn` takes `SESSION_SET_PROJECT`.
#[must_use]
pub fn session_project_supported(conn: &Connection) -> bool {
    conn.negotiated_bootstrap().is_some_and(|negotiated| {
        negotiated
            .server_features_ext
            .contains(ServerFeatureExt::SessionProject)
    })
}

/// `phux project link` / `unlink`: set or clear a session's project link
/// (`SESSION_SET_PROJECT`; the last writer wins).
///
/// # Errors
///
/// [`OrgError::Unsupported`] unless the server advertises
/// `SESSION_PROJECT`, with nothing sent; `INVALID_ARGUMENT` for a blank,
/// over-long or control-character label; transport failures.
pub async fn set_session_project(
    conn: &mut Connection,
    session: SessionKey,
    project: Option<&str>,
) -> Result<Committed, OrgError> {
    if !session_project_supported(conn) {
        return Err(OrgError::Unsupported);
    }
    let command = Command::SessionSetProject {
        operation: mint_operation(),
        session,
        project: project.map(str::to_owned),
    };
    commit(conn, FIRST_REQUEST_ID, command).await
}

/// `phux relaunch`: start a new activation of an inactive resource.
///
/// # Errors
///
/// `EXECUTION_UNCERTAIN` for a lost activation; a refusal while it runs;
/// transport failures.
pub async fn relaunch(conn: &mut Connection, resource: ResourceKey) -> Result<Committed, OrgError> {
    run_cas(conn, Edit::Relaunch(resource)).await
}

/// Create one resource record of `kind` in `session` (`RESOURCE_CREATE`).
///
/// It starts from `launch`, or, under [`LaunchMode::Deferred`], stays a
/// dormant record holding `launch` until [`relaunch`] or [`restore`] starts
/// it. The record is removed when its process exits.
///
/// # Errors
///
/// A refusal (an unknown session, a deferred create with no `launch`);
/// transport failures.
pub async fn create_resource(
    conn: &mut Connection,
    session: SessionKey,
    kind: ResourceKind,
    launch: Option<LaunchSpec>,
    launch_mode: LaunchMode,
) -> Result<Committed, OrgError> {
    commit(
        conn,
        FIRST_REQUEST_ID,
        Command::ResourceCreate {
            placement: None,
            operation: mint_operation(),
            session,
            kind,
            launch: launch.map(Box::new),
            name: None,
            on_exit: ExitPolicy::Remove,
            launch_mode,
        },
    )
    .await
}

/// Replace `session`'s arrangement with `document` (key space), against
/// its current revision.
///
/// # Errors
///
/// [`OrgError::NotFound`] when the session has no arrangement; a refusal
/// for a document naming a non-member; transport failures.
pub async fn replace_session_arrangement(
    conn: &mut Connection,
    session: SessionKey,
    document: &ArrangementDoc,
) -> Result<Committed, OrgError> {
    run_cas(conn, Edit::ReplaceArrangement(session, document)).await
}

/// Replace the arrangement of `member`'s session with `document`.
///
/// `document` is a client document over live handles; its leaves are
/// translated to the session's member keys. Runs over a fresh connection
/// (`phux archive` restore).
///
/// # Errors
///
/// [`OrgError::NotFound`] when `member` is not a live member here or a leaf
/// names no member of its session; otherwise as
/// [`replace_session_arrangement`].
pub async fn replace_arrangement_of_member(
    socket_path: &std::path::Path,
    member: &ResourceId,
    document: &crate::layout::ArrangementDoc,
) -> Result<Committed, OrgError> {
    use crate::layout::wire::{EntryBinding, HandleMap, to_protocol};
    let mut conn = Connection::connect(socket_path)
        .await
        .map_err(OrgError::Attach)?;
    let inventory = fetch(&mut conn, OrgFilter::All, FIRST_REQUEST_ID).await?;
    let session = inventory
        .resource_by_live(member)
        .map(|record| record.session)
        .ok_or_else(|| OrgError::NotFound(crate::selector::format_terminal_id(member)))?;
    let handles = HandleMap::from_pairs(
        inventory
            .members(&session)
            .map(|record| (record.key, record.live.clone())),
    );
    let document = to_protocol(document, &handles, &EntryBinding::default())
        .map_err(|err| OrgError::NotFound(err.to_string()))?;
    replace_session_arrangement(&mut conn, session, &document).await
}

/// `phux restore [SESSION]`: relaunch every restorable resource
/// ([`OrgInventory::restorable`]), in their sessions, one result per
/// resource in page order.
///
/// # Errors
///
/// Only a failure to read the records; each relaunch reports its own result.
pub async fn restore(
    conn: &mut Connection,
    session: Option<SessionKey>,
) -> Result<Vec<(ResourceKey, Result<Committed, OrgError>)>, OrgError> {
    let filter = session.map_or(OrgFilter::All, OrgFilter::Session);
    let view = fetch(conn, filter, FIRST_REQUEST_ID).await?;
    let targets: Vec<ResourceKey> = view
        .restorable(session.as_ref())
        .into_iter()
        .map(|record| record.key)
        .collect();
    let mut results = Vec::with_capacity(targets.len());
    for key in targets {
        let result = relaunch(conn, key).await;
        results.push((key, result));
    }
    Ok(results)
}

/// [`restore`] for the session named `name`, over a fresh connection to
/// `socket_path`.
///
/// # Errors
///
/// [`OrgError::Attach`] when the server cannot be reached,
/// [`OrgError::NotFound`] when no session has that name, or a failure to
/// read the records; each relaunch reports its own result.
pub async fn restore_session_on(
    socket_path: &std::path::Path,
    name: &str,
) -> Result<Vec<(ResourceKey, Result<Committed, OrgError>)>, OrgError> {
    let mut conn = Connection::connect(socket_path)
        .await
        .map_err(OrgError::Attach)?;
    let session = fetch(&mut conn, OrgFilter::All, FIRST_REQUEST_ID)
        .await?
        .session_named(name)
        .map(|record| record.key)
        .ok_or_else(|| OrgError::NotFound(name.to_owned()))?;
    let results = restore(&mut conn, Some(session)).await;
    drop(conn);
    results
}

/// `phux mv`: move one resource's membership into session `to`. The process
/// keeps running.
///
/// # Errors
///
/// A refusal (authorization on both sessions, a missing record); transport
/// failures.
pub async fn move_resource(
    conn: &mut Connection,
    resource: ResourceKey,
    to: SessionKey,
) -> Result<Committed, OrgError> {
    run_cas(conn, Edit::Move { resource, to }).await
}

/// One chunk of a bulk move and how it ended.
#[derive(Debug)]
pub struct BatchResult {
    /// The resources in this chunk.
    pub resources: Vec<ResourceKey>,
    /// The chunk's receipt or refusal. A refused chunk moved nothing.
    pub result: Result<Committed, OrgError>,
}

/// Move many resources into `to`, chunked at the server's batch bound
/// ([`MAX_ORG_BATCH`]); each chunk is atomic and reports its own result.
///
/// # Errors
///
/// Only a failure to read the records; chunks report their own results.
pub async fn move_many(
    conn: &mut Connection,
    resources: &[ResourceKey],
    to: SessionKey,
) -> Result<Vec<BatchResult>, OrgError> {
    let mut results = Vec::new();
    for chunk in resources.chunks(MAX_ORG_BATCH) {
        let result = run_cas(
            conn,
            Edit::MoveMany {
                resources: chunk,
                to,
            },
        )
        .await;
        results.push(BatchResult {
            resources: chunk.to_vec(),
            result,
        });
    }
    Ok(results)
}

/// Undo a committed organization edit by its receipt. Checked against the
/// current state: a changed record refuses `CONFLICT`, an expired receipt
/// `NOT_FOUND`.
///
/// # Errors
///
/// The refusal; transport failures.
pub async fn undo(conn: &mut Connection, receipt: &Committed) -> Result<Committed, OrgError> {
    let command = Command::OrganizationUndo {
        operation: mint_operation(),
        undo: receipt.operation,
        expected: receipt.revisions.clone(),
    };
    commit(conn, FIRST_REQUEST_ID, command).await
}

/// Clear a session's Intake mark; members and execution are untouched.
///
/// # Errors
///
/// The refusal; transport failures.
pub async fn intake_resolve(
    conn: &mut Connection,
    session: SessionKey,
) -> Result<Committed, OrgError> {
    run_cas(conn, Edit::IntakeResolve(session)).await
}

/// Legacy alias for [`intake_resolve`].
///
/// # Errors
///
/// The same transport and compare-and-swap refusals as [`intake_resolve`].
pub async fn inbox_resolve(
    conn: &mut Connection,
    session: SessionKey,
) -> Result<Committed, OrgError> {
    intake_resolve(conn, session).await
}

/// Rename (or, with `None`, unname) a resource record.
///
/// # Errors
///
/// The refusal; transport failures.
pub async fn rename_resource(
    conn: &mut Connection,
    resource: ResourceKey,
    name: Option<&str>,
) -> Result<Committed, OrgError> {
    let command = Command::ResourceRename {
        operation: mint_operation(),
        resource,
        name: name.map(str::to_owned),
    };
    commit(conn, FIRST_REQUEST_ID, command).await
}

/// Terminate a resource's current activation; the outcome arrives as an
/// `ACTIVATION_CHANGED` event.
///
/// # Errors
///
/// [`OrgError::NotFound`] when nothing executes here; the refusal;
/// transport failures.
pub async fn terminate(
    conn: &mut Connection,
    record: &ResourceRecord,
    mode: TerminateMode,
) -> Result<(), OrgError> {
    let target = live_target(record).ok_or_else(|| OrgError::NotFound(record.key.to_string()))?;
    accept(
        conn,
        FIRST_REQUEST_ID,
        Command::ResourceTerminate {
            operation: mint_operation(),
            target,
            mode,
        },
    )
    .await
}
