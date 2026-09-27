//! Wire round trips for the `phux.agent/v1` record (ADR-0040) behind
//! `phux agent set` / `clear`; the record type lives in [`crate::agent_meta`].

use std::collections::HashMap;
use std::path::Path;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, RESOURCE_AGENT_KEY, Scope};
use phux_protocol::wire::info::SessionSnapshot;

use crate::agent_meta::{AgentRecord, parse_agent_record};
use crate::attach::AttachError;
use crate::attach::connection::{Answer, Connection};
use crate::state::Degradation;

/// One `GET_METADATA` round-trip for `pane`'s agent record; a refusal stays
/// distinct from "no record".
///
/// # Errors
///
/// Transport and decode failures from [`Connection::request_metadata`].
pub async fn get_record(
    conn: &mut Connection,
    pane: &ResourceId,
    request_id: u32,
) -> Result<(Answer<Option<AgentRecord>>, Degradation), AttachError> {
    let (answer, interleaved) = conn
        .request_metadata(
            request_id,
            Scope::Resource(pane.clone()),
            RESOURCE_AGENT_KEY.to_owned(),
        )
        .await?
        .into_parts();
    let degradation = Degradation::from_interleaved(&interleaved);
    Ok((
        answer.map(|value| value.as_deref().and_then(parse_agent_record)),
        degradation,
    ))
}

/// Write `record` to `pane`, then confirm with a trailing `GET_METADATA`.
///
/// The read-back consumes `request_id + 1`. `SET_METADATA` has no reply, so
/// the ordered read-back proves the write applied before the process exits.
///
/// # Errors
///
/// Transport and decode failures from [`Connection::send`] /
/// [`Connection::request_metadata`].
pub async fn set_record(
    conn: &mut Connection,
    request_id: u32,
    pane: &ResourceId,
    record: &AgentRecord,
) -> Result<(Answer<Option<AgentRecord>>, Degradation), AttachError> {
    conn.send(&FrameKind::SetMetadata {
        request_id,
        scope: Scope::Resource(pane.clone()),
        key: RESOURCE_AGENT_KEY.to_owned(),
        value: record.encode(),
    })
    .await?;
    get_record(conn, pane, request_id.wrapping_add(1)).await
}

/// Delete `pane`'s record, confirmed like [`set_record`].
///
/// # Errors
///
/// Transport and decode failures from [`Connection::send`] /
/// [`Connection::request_metadata`].
pub async fn clear_record(
    conn: &mut Connection,
    request_id: u32,
    pane: &ResourceId,
) -> Result<(Answer<Option<AgentRecord>>, Degradation), AttachError> {
    conn.send(&FrameKind::DeleteMetadata {
        request_id,
        scope: Scope::Resource(pane.clone()),
        key: RESOURCE_AGENT_KEY.to_owned(),
    })
    .await?;
    get_record(conn, pane, request_id.wrapping_add(1)).await
}

/// Fetch every pane's decoded `phux.agent/v1` record over one connection.
///
/// One sequential `GET_METADATA` per pane. Missing, invalid, or refused
/// records are absent; a transport failure returns what was collected.
/// Interleaved degradation notices are appended to `notices` in order.
pub async fn fetch_index(
    socket_path: &Path,
    snapshot: &SessionSnapshot,
    notices: &mut Vec<String>,
) -> HashMap<ResourceId, AgentRecord> {
    let mut index = HashMap::new();
    if snapshot.resources.is_empty() {
        return index;
    }
    let Ok(mut conn) = Connection::connect(socket_path).await else {
        return index;
    };
    for (offset, pane) in snapshot.resources.iter().enumerate() {
        let request_id = u32::try_from(offset).unwrap_or(u32::MAX).saturating_add(1);
        let Ok((answer, degradation)) = get_record(&mut conn, &pane.id, request_id).await else {
            return index;
        };
        notices.extend(degradation.notices().iter().cloned());
        if let Ok(Some(record)) = answer {
            index.insert(pane.id.clone(), record);
        }
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{ScriptSpec, serve_one};

    #[tokio::test]
    async fn set_then_clear_round_trip_and_surface_interleaved_degradation() {
        let dir = tempfile::tempdir().expect("temp dir");
        let pane = ResourceId::local(9);
        let record = AgentRecord {
            name: "codex".to_owned(),
            ..AgentRecord::default()
        };
        let spec = ScriptSpec::new().degradation_notice("satellite edge is unreachable: timed out");
        let (socket, server) = serve_one(dir.path(), spec);

        let mut conn = Connection::connect(&socket).await.expect("connect");
        let (answer, degradation) = set_record(&mut conn, 1, &pane, &record)
            .await
            .expect("scripted server answers");
        assert_eq!(answer, Ok(Some(record.clone())));
        assert!(!degradation.is_complete());
        assert_eq!(
            degradation.notices(),
            ["satellite edge is unreachable: timed out"]
        );
        drop(conn);
        let seen = server.await.expect("scripted server task");

        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::SetMetadata { request_id: 1, scope: Scope::Resource(scoped), key, .. }
                    if *scoped == pane && key == RESOURCE_AGENT_KEY
            )),
            "expected the SET at request_id 1; sent {seen:?}"
        );
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::GetMetadata { request_id: 2, scope: Scope::Resource(scoped), key }
                    if *scoped == pane && key == RESOURCE_AGENT_KEY
            )),
            "expected the confirming GET at request_id 2; sent {seen:?}"
        );
    }

    #[tokio::test]
    async fn clear_confirms_the_record_is_gone() {
        let dir = tempfile::tempdir().expect("temp dir");
        let pane = ResourceId::local(4);
        let (socket, server) = serve_one(dir.path(), ScriptSpec::new());

        let mut conn = Connection::connect(&socket).await.expect("connect");
        let (answer, degradation) = clear_record(&mut conn, 5, &pane)
            .await
            .expect("scripted server answers");
        drop(conn);
        let seen = server.await.expect("scripted server task");

        assert!(degradation.is_complete());
        assert_eq!(answer, Ok(None));
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::DeleteMetadata { request_id: 5, scope: Scope::Resource(scoped), key }
                    if *scoped == pane && key == RESOURCE_AGENT_KEY
            )),
            "expected the DELETE at request_id 5; sent {seen:?}"
        );
    }
}
