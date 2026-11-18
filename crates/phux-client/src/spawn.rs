//! Unplaced `SPAWN_RESOURCE` and canonical placed spawn for CLI and MCP.

use std::path::Path;

use phux_protocol::ids::{IdempotencyKey, ResourceId, SessionId};
use phux_protocol::wire::UnreadableResult;
use phux_protocol::wire::frame::{ErrorCode, FrameKind, SpawnError, SpawnResult};
use phux_protocol::wire::info::SessionSnapshot;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::state::Degradation;

mod placed;
pub use placed::{PlacedSpawn, PlacedSpawnError};

/// Send a `SPAWN_RESOURCE` frame over `conn` and return the matching
/// `RESOURCE_SPAWNED` result with any interleaved notices.
///
/// A correlated `ERROR` (a satellite may answer a relayed spawn that way) is
/// folded into [`SpawnResult::Err`]: a refusal into `SpawnFailed`, but a code
/// this build does not know (`UNREADABLE_REPLY` included) into
/// [`SpawnError::Unknown`], since the pane may exist.
///
/// # Errors
///
/// Transport and decode failures from [`Connection::request_spawn`].
pub async fn spawn(
    conn: &mut Connection,
    frame: &FrameKind,
) -> Result<(SpawnResult, Degradation), AttachError> {
    let (answer, interleaved) = conn.request_spawn(frame).await?.into_parts();
    let degradation = Degradation::from_interleaved(&interleaved);
    let result = answer.unwrap_or_else(|refusal| {
        if matches!(refusal.code, ErrorCode::Unknown(_)) {
            // Only matched on, never encoded: no wire bytes to carry.
            return SpawnResult::Err(SpawnError::Unknown(UnreadableResult::from_wire(Vec::new())));
        }
        SpawnResult::Err(SpawnError::SpawnFailed(format!(
            "server refused the spawn: {refusal}"
        )))
    });
    Ok((result, degradation))
}

/// [`spawn`] over a fresh connection.
///
/// # Errors
///
/// Transport failures from [`Connection::connect`] or [`spawn`].
pub async fn spawn_on(
    socket_path: &Path,
    frame: &FrameKind,
) -> Result<(SpawnResult, Degradation), AttachError> {
    let mut conn = Connection::connect(socket_path).await?;
    let outcome = spawn(&mut conn, frame).await?;
    drop(conn);
    Ok(outcome)
}

/// Why a string is not an idempotency key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("an idempotency key is 32 hex digits (16 bytes), not all zero")]
pub struct InvalidIdempotencyKey;

/// Parse an idempotency key (ADR-0126): 32 hex digits, not all zero. Draw
/// one from a CSPRNG per logical operation and reuse it on every retry.
///
/// # Errors
///
/// [`InvalidIdempotencyKey`] for any other text.
pub fn parse_idempotency_key(text: &str) -> Result<IdempotencyKey, InvalidIdempotencyKey> {
    if text.len() != 32 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(InvalidIdempotencyKey);
    }
    let mut bytes = [0_u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        let at = index * 2;
        *byte = u8::from_str_radix(&text[at..at + 2], 16).map_err(|_| InvalidIdempotencyKey)?;
    }
    IdempotencyKey::new(bytes).ok_or(InvalidIdempotencyKey)
}

/// The `phux spawn --json` result document for `terminal_id`.
///
/// `terminal_id` is the satellite-local id when `satellite` is non-null
/// (address it through the hub as `satellite` + `terminal_id`), and
/// `replayed` is `true` when a keyed retry answered an earlier spawn's pane.
/// One builder for the CLI and the MCP `phux_spawn` tool.
#[must_use]
pub fn spawned_document(terminal_id: &ResourceId, replayed: bool) -> serde_json::Value {
    let (id, host) = match terminal_id {
        ResourceId::Local { id } => (*id, None),
        ResourceId::Satellite { host, id } => (*id, Some(host.as_str())),
    };
    serde_json::json!({
        "schema_version": 1,
        "terminal_id": id,
        "satellite": host,
        "replayed": replayed,
    })
}

/// What a spawn whose outcome is unknown reports: never "refused" or
/// "failed", since a retry could duplicate the pane.
pub const SPAWN_OUTCOME_UNKNOWN: &str = "spawn outcome unknown: the server answered, but this \
     build could not read whether it created the pane; it may exist, so check `phux ls` \
     before retrying";

/// Whether `err` leaves the spawn's outcome unknown rather than refused.
#[must_use]
pub const fn spawn_outcome_unknown(err: &SpawnError) -> bool {
    matches!(err, SpawnError::Unknown(_))
}

/// The actionable sentence for a typed `SpawnError`, shared by `phux spawn`,
/// `phux launch`, and the MCP `phux_spawn` tool.
#[must_use]
pub fn spawn_error_message(err: &SpawnError) -> String {
    match err {
        SpawnError::GroupNotFound => "spawn failed: server rejected the default group".to_owned(),
        SpawnError::SpawnFailed(reason) => format!("spawn failed: {reason}"),
        SpawnError::UnsupportedSatelliteRoute => "spawn failed: no route to that satellite \
             (is the server running with --hub, and the name in \
             `phux host ls --role satellite`?)"
            .to_owned(),
        SpawnError::SatelliteUnreachable(reason) => {
            format!("spawn failed: satellite unreachable: {reason}")
        }
        SpawnError::Unknown(_) => SPAWN_OUTCOME_UNKNOWN.to_owned(),
        // `SpawnError` is `#[non_exhaustive]`: a code with no arm here is a
        // vocabulary this client does not have, i.e. version skew.
        _ => format!(
            "spawn failed: {}",
            crate::explain::unexpected_reply("SPAWN_RESOURCE")
        ),
    }
}

/// The session a Terminal belongs to, read out of a `GET_STATE` snapshot
/// ([`crate::resource::session_of`]).
#[must_use]
pub fn ownership_for_terminal(
    snapshot: &SessionSnapshot,
    terminal: &ResourceId,
) -> Option<SessionId> {
    crate::resource::session_of(snapshot, terminal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::ids::GroupId;
    use phux_protocol::wire::frame::ErrorCode;

    fn spawn_frame() -> FrameKind {
        FrameKind::SpawnResource {
            request_id: 1,
            group: GroupId::new(1),
            command: Some(vec!["agent".to_owned()]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: None,
        }
    }

    /// A spawn answered with an unreadable reply leaves its outcome unknown:
    /// never folded into a refusal, and the message points at `phux ls`.
    #[tokio::test]
    async fn an_unreadable_spawn_answer_is_an_unknown_outcome() {
        let spec = crate::testkit::ScriptSpec::new()
            .refuse_spawn(ErrorCode::UNREADABLE_REPLY, "reply carried an unknown tag");
        let temp = tempfile::TempDir::new().expect("tempdir");
        let (socket, server) = crate::testkit::serve_one(temp.path(), spec);
        let (result, _degradation) = spawn_on(&socket, &spawn_frame()).await.expect("transport");
        let SpawnResult::Err(error) = &result else {
            panic!("got {result:?}");
        };
        assert!(spawn_outcome_unknown(error), "got {result:?}");
        assert!(spawn_error_message(error).contains("phux ls"));
        server.await.expect("scripted server");
    }

    /// A spawn refused by a correlated `ERROR` (a satellite) or by its own
    /// `SpawnFailed` reply (a scoped denial) ends with the refusal instead
    /// of wedging.
    #[tokio::test]
    async fn a_refused_spawn_ends_with_its_refusal() {
        let refusing = crate::testkit::ScriptSpec::new().refuse_spawn(
            ErrorCode::UnsupportedSatelliteRoute,
            "no satellite route to build-box",
        );
        let denying = crate::testkit::ScriptSpec::new().spawn_result(SpawnResult::Err(
            SpawnError::SpawnFailed("permission denied".to_owned()),
        ));
        for (spec, expected) in [
            (refusing, "no satellite route to build-box"),
            (denying, "permission denied"),
        ] {
            let temp = tempfile::TempDir::new().expect("tempdir");
            let (socket, server) = crate::testkit::serve_one(temp.path(), spec);
            let (result, _degradation) = tokio::time::timeout(
                std::time::Duration::from_secs(20),
                spawn_on(&socket, &spawn_frame()),
            )
            .await
            .expect("a refused spawn must return, not wedge")
            .expect("transport");
            assert!(
                matches!(
                    &result,
                    SpawnResult::Err(SpawnError::SpawnFailed(reason)) if reason.contains(expected)
                ),
                "got {result:?}"
            );
            server.await.expect("scripted server");
        }
    }

    #[test]
    fn ownership_for_terminal_reads_the_owning_session() {
        use phux_protocol::ids::{SessionId, WindowId};
        use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot, WindowInfo};

        let session = SessionId::new(1);
        let window = WindowId::new(10);
        let pane = ResourceId::local(3);
        let snapshot = SessionSnapshot::new(session, window, pane.clone())
            .with_windows(vec![WindowInfo::new(window, session, "one")])
            .with_resources(vec![ResourceInfo::new(pane.clone(), window, 80, 24)]);
        assert_eq!(ownership_for_terminal(&snapshot, &pane), Some(session));
        assert_eq!(
            ownership_for_terminal(&snapshot, &ResourceId::local(99)),
            None
        );
    }
}
