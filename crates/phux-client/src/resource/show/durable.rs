//! The same resource noun, addressed by durable identity rather than a live
//! handle. One filtered organization read; never fetch the whole inventory.
use crate::{
    attach::connection::Connection,
    org::{self, OrgError},
};
use phux_protocol::{
    api::ResourceObserved, caps::ServerFeatureExt, ids::ResourceKey, wire::frame::OrgFilter,
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
    if !conn
        .server_features_ext()
        .contains(ServerFeatureExt::DurableResourceInspection)
    {
        return Err(OrgError::Unsupported);
    }
    let inventory = org::fetch(conn, OrgFilter::Resource(key), 1).await?;
    let record = inventory
        .resources
        .into_iter()
        .find(|r| r.key == key)
        .ok_or_else(|| OrgError::NotFound(format!("^{key}")))?;
    Ok(ResourceObserved {
        schema_version: 2,
        authority: inventory.authority.to_string(),
        resource: record.key.to_string(),
        session: record.session.to_string(),
        name: record.name,
        kind: super::super::kind_name(record.kind).to_owned(),
        execution: record.activation.map(crate::terminal::observation),
        terminal_id: record
            .live
            .as_ref()
            .and_then(phux_protocol::ids::ResourceId::local_id),
    })
}
