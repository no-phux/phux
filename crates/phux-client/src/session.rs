//! Session-identity writes: `phux rename` and `phux new`'s
//! create-without-attach (ADR-0022 §5).
//!
//! Against a server that advertises `ORGANIZATION` both are organization
//! commands: `SESSION_RENAME` naming the session's key and the revision read
//! from its record, and `SESSION_CREATE` with the seed pane as its launch.
//! Each carries a fresh `OperationId` (a keyed create's idempotency key is
//! its operation id), and the `COMMITTED` or `REFUSED` reply is the outcome.
//!
//! Against an older server both are L3 `SET_METADATA` writes of a
//! conventional key the server intercepts (ADR-0027); this module owns those
//! write + read-back round trips too.

use phux_protocol::ids::{ResourceId, SessionId};
use phux_protocol::wire::frame::{FrameKind, SESSION_CREATE_KEY, Scope};

use crate::attach::AttachError;
use crate::attach::connection::Connection;

mod create;
mod empty;
mod rename;

pub use create::{
    CreateOutcome, CreateSessionError, CreateSessionRequest, create_session, keyed_create_supported,
};
pub use empty::{
    CreateEmptyOutcome, check_session_directory, create_empty_session,
    create_empty_session_with_attrs, keep_empty_supported,
};
pub use rename::{RenameError, rename_checked};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CreatedSession {
    terminal_id: u32,
    session_id: Option<SessionId>,
}

/// Send the `SESSION_CREATE_KEY` write; the nonce inside correlates the
/// read-back that follows.
async fn send_create(conn: &mut Connection, create_bytes: Vec<u8>) -> Result<(), AttachError> {
    conn.send(&FrameKind::SetMetadata {
        request_id: 1,
        scope: Scope::Global,
        key: SESSION_CREATE_KEY.to_owned(),
        value: create_bytes,
    })
    .await
}

/// One create's read-back, classified.
enum ReadBack {
    /// The correlated nonce key answered with a value.
    Correlated(Vec<u8>),
    /// The correlated read was refused.
    Refused(String),
    /// The correlated key held no value.
    Absent,
}

/// Read only this request's one-shot result.
async fn read_result(
    conn: &mut Connection,
    result_key: String,
    notices: &mut Vec<String>,
) -> Result<ReadBack, AttachError> {
    let (answer, interleaved) = conn
        .request_metadata(2, Scope::Global, result_key)
        .await?
        .into_parts();
    notices.extend(crate::state::degradation_notices(&interleaved));
    Ok(match answer {
        Ok(Some(bytes)) => ReadBack::Correlated(bytes),
        Ok(None) => ReadBack::Absent,
        Err(refusal) => ReadBack::Refused(refusal.to_string()),
    })
}

/// Read the seed pane and owning Session ids out of a create-result document,
/// rejecting one that does not answer for this request: both the name and
/// the nonce must match.
fn created_session_from_result(
    bytes: &[u8],
    name: &str,
    request_token: &str,
) -> Option<CreatedSession> {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .filter(|v| v.get("name").and_then(serde_json::Value::as_str) == Some(name))
        .filter(|v| {
            v.get("request_token").and_then(serde_json::Value::as_str) == Some(request_token)
        })
        .and_then(|v| {
            let terminal_id = u32::try_from(v.get("terminal_id")?.as_u64()?).ok()?;
            let session_id = v
                .get("session_id")
                .and_then(serde_json::Value::as_u64)
                .and_then(|id| u32::try_from(id).ok())
                .map(SessionId::new);
            Some(CreatedSession {
                terminal_id,
                session_id,
            })
        })
}

/// Bind a legacy name-only create receipt to the session that still owns its
/// returned Terminal, so concurrent rename/recreate activity cannot seed a
/// same-named but unrelated session.
fn created_session_id_from_snapshot(
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    name: &str,
    terminal_id: u32,
) -> Option<SessionId> {
    let owner = crate::resource::session_of(snapshot, &ResourceId::local(terminal_id))?;
    snapshot
        .sessions
        .iter()
        .find(|session| session.name == name && session.id == owner)
        .map(|session| session.id)
}

/// Whether a create-result document answers an empty-session request: the
/// name and nonce match, and the server confirms it created no terminal.
fn empty_session_result_matches(bytes: &[u8], name: &str, request_token: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes).is_ok_and(|v| {
        v.get("name").and_then(serde_json::Value::as_str) == Some(name)
            && v.get("request_token").and_then(serde_json::Value::as_str) == Some(request_token)
            && v.get("empty").and_then(serde_json::Value::as_bool) == Some(true)
    })
}

#[cfg(test)]
mod tests;
