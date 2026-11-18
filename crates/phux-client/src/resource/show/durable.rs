//! The same resource noun, addressed by durable identity rather than a live
//! handle. One filtered organization read; never fetch the whole inventory.
use crate::{
    attach::connection::Connection,
    org::{self, OrgError},
};
use phux_protocol::{
    api::{ExecutionRef, ResourceObserved},
    caps::ServerFeatureExt,
    ids::{AuthorityId, ResourceKey},
    wire::frame::{OrgFilter, ResourceRecord},
};
use std::path::Path;

/// Inspect the resource's current activation and membership, even after its
/// engine or creation receipt has gone. Does not adopt a historical operation.
///
/// # Errors
/// Unsupported before sending on older peers; absence includes filtered records.
pub async fn show_key(socket: &Path, key: ResourceKey) -> Result<ResourceObserved, OrgError> {
    let mut conn = Connection::connect(socket)
        .await
        .map_err(OrgError::Attach)?;
    show_key_on(&mut conn, key).await
}

/// Durable resource inspection on an already authenticated connection.
///
/// # Errors
/// Transport, unsupported, refusal, or no visible retained record.
pub async fn show_key_on(
    conn: &mut Connection,
    key: ResourceKey,
) -> Result<ResourceObserved, OrgError> {
    let (_, record) = show_key_level_on(conn, key).await?;
    record.ok_or_else(|| OrgError::NotFound(format!("^{key}")))
}

/// Read a durable resource and the serving authority, even when it is absent.
///
/// # Errors
///
/// Transport, unsupported or refusal.
pub async fn show_key_level_on(
    conn: &mut Connection,
    key: ResourceKey,
) -> Result<(AuthorityId, Option<ResourceObserved>), OrgError> {
    let (authority, record) = read_key_on(conn, key).await?;
    Ok((authority, record.map(|record| project(authority, record))))
}

fn project(authority: AuthorityId, record: ResourceRecord) -> ResourceObserved {
    let authority = authority.to_string();
    let resource = record.key.to_string();
    let execution = record.activation.map(crate::terminal::observation);
    let execution_ref = execution
        .as_ref()
        .map(|observation| ExecutionRef::from_observation(&authority, &resource, observation));
    ResourceObserved {
        schema_version: 2,
        authority,
        resource,
        session: record.session.to_string(),
        name: record.name,
        kind: super::super::kind_name(record.kind).to_owned(),
        execution,
        execution_ref,
        terminal_id: record
            .live
            .as_ref()
            .and_then(phux_protocol::ids::ResourceId::local_id),
    }
}

pub(crate) async fn read_key_on(
    conn: &mut Connection,
    key: ResourceKey,
) -> Result<(AuthorityId, Option<ResourceRecord>), OrgError> {
    if !conn
        .server_features_ext()
        .contains(ServerFeatureExt::DurableResourceInspection)
    {
        return Err(OrgError::Unsupported);
    }
    let inventory = org::fetch(conn, OrgFilter::Resource(key), 1).await?;
    let record = inventory.resources.into_iter().find(|r| r.key == key);
    Ok((inventory.authority, record))
}
