//! Wire primitives for `phux agent set` / `clear` — writing and reading the
//! structured `phux.agent/v1` record (ADR-0040) over L3 `SET_METADATA` /
//! `DELETE_METADATA` / `GET_METADATA`.
//!
//! The record type and its encode/parse convention live in
//! [`crate::agent_meta`]; this module owns the round trips. Selector
//! resolution (which pane a `phux agent set/clear TARGET` names) stays
//! client-side — see `crates/phux/src/commands/agent/record.rs`.

use std::collections::HashMap;
use std::path::Path;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, RESOURCE_AGENT_KEY, Scope};
use phux_protocol::wire::info::SessionSnapshot;

use crate::agent_meta::{AgentRecord, parse_agent_record};
use crate::attach::AttachError;
use crate::attach::connection::{Answer, Connection};
use crate::state::Degradation;

/// One `GET_METADATA` round-trip for `pane`'s agent record.
///
/// Returns an [`Answer`] rather than a bare `Option` so a refusal cannot be
/// mistaken for "this pane has no record".
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

/// Write `record` to `pane`, then confirm it landed with a trailing
/// `GET_METADATA` (consuming `request_id + 1`).
///
/// `SET_METADATA` carries no reply frame, so the confirming round-trip is
/// load-bearing, not cosmetic: the process could otherwise exit before the
/// server reads the write. Frames are ordered on one connection, so the
/// reply proves the write was applied before this returns.
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

/// Delete `pane`'s record, then confirm the delete with a trailing
/// `GET_METADATA` (consuming `request_id + 1`). Same load-bearing shape as
/// [`set_record`].
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

/// Fetch the `phux.agent/v1` index — `ResourceId` → decoded record — for
/// every pane in `snapshot`, over one fresh connection to `socket_path`.
///
/// One `GET_METADATA` round trip per pane: sequential rather than pipelined
/// since phux-h5hj.12 (a hand-rolled pipeline that counted down only on
/// `METADATA_VALUE` wedged on a correlated `ERROR` refusal). A pane with no
/// record, or bytes that fail the §3.7 validation, is simply absent from the
/// index, as is one the server refuses to read (this index has no channel to
/// report a refusal on). Best-effort: a transport failure returns what was
/// collected so the caller degrades to heuristics instead of erroring.
///
/// Every interleaved degradation notice observed along the way is appended
/// to `notices`, in encounter order, for the caller to print.
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

    #[tokio::test]
    async fn set_then_clear_round_trip_and_surface_interleaved_degradation() {
        use crate::testkit::{ScriptSpec, ScriptedServer};

        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let listener = tokio::net::UnixListener::from_std(listener).expect("tokio listener");
        let pane = ResourceId::local(9);
        let record = AgentRecord {
            name: "codex".to_owned(),
            ..AgentRecord::default()
        };
        let spec = ScriptSpec::new().degradation_notice("satellite edge is unreachable: timed out");
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

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
        use crate::testkit::{ScriptSpec, ScriptedServer};

        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let listener = tokio::net::UnixListener::from_std(listener).expect("tokio listener");
        let pane = ResourceId::local(4);
        let spec = ScriptSpec::new();
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

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
