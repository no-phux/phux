//! Wire primitives for `phux spawn` / `phux launch` (`SPAWN_RESOURCE`, SPEC
//! L1 §3.1), plus the ownership-verify + `KILL_RESOURCE` rollback dance
//! behind explicit placement.
//!
//! Explicit placement addresses an exact owning window and splices the new
//! leaf into that window's shared layout ([`crate::layout_ops`]). Selector
//! resolution (which pane `--target` names) stays client-side (ADR-0021) and
//! is the caller's job; this module starts from an already-resolved owner.

use std::path::Path;

use phux_protocol::caps::{ServerFeature, ServerFeatureSet};
use phux_protocol::ids::{IdempotencyKey, ResourceId, SessionId, WindowId};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, SpawnError, SpawnResult, StateScope,
};
use phux_protocol::wire::info::SessionSnapshot;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::layout::{SplitDir, Workspace};
use crate::layout_ops::LayoutMutation;
use crate::state::Degradation;

/// Send a `SPAWN_RESOURCE` frame over `conn` and return the matching
/// `RESOURCE_SPAWNED` result with any interleaved notices.
///
/// A correlated `ERROR` (a satellite may answer a relayed spawn that way) is
/// folded into [`SpawnResult::Err`].
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

/// The additive feature `frame` relies on that `features` lacks.
///
/// Retention needs `RETAIN_ON_EXIT` and a keyed spawn `SPAWN_IDEMPOTENCY`; a
/// server without the bit silently skips the field, so callers refuse first.
#[must_use]
pub fn missing_spawn_feature(
    frame: &FrameKind,
    features: ServerFeatureSet,
) -> Option<ServerFeature> {
    let FrameKind::SpawnResource {
        resource: Some(resource),
        ..
    } = frame
    else {
        return None;
    };
    [
        (resource.retain_secs.is_some(), ServerFeature::RetainOnExit),
        (
            resource.idempotency_key.is_some(),
            ServerFeature::SpawnIdempotency,
        ),
    ]
    .into_iter()
    .find(|(asked, feature)| *asked && !features.contains(*feature))
    .map(|(_, feature)| feature)
}

/// The additive features the server at `socket_path` advertises in
/// `HELLO_OK`.
///
/// # Errors
///
/// Transport failures from [`Connection::connect`].
pub async fn server_features(socket_path: &Path) -> Result<ServerFeatureSet, AttachError> {
    let conn = Connection::connect(socket_path).await?;
    Ok(conn
        .negotiated_bootstrap()
        .map_or_else(ServerFeatureSet::new, |negotiated| {
            negotiated.server_features
        }))
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
        // `SpawnError` is `#[non_exhaustive]`: a code with no arm here is a
        // vocabulary this client does not have, i.e. version skew.
        _ => format!(
            "spawn failed: {}",
            crate::explain::unexpected_reply("SPAWN_RESOURCE")
        ),
    }
}

/// The window/session a Terminal belongs to, read out of a `GET_STATE`
/// snapshot.
#[must_use]
pub fn ownership_for_terminal(
    snapshot: &SessionSnapshot,
    terminal: &ResourceId,
) -> Option<(WindowId, SessionId)> {
    let window = snapshot
        .resources
        .iter()
        .find(|pane| &pane.id == terminal)?
        .window_id;
    let session = snapshot
        .windows
        .iter()
        .find(|candidate| candidate.id == window)?
        .session_id;
    Some((window, session))
}

/// An explicit spawn placement, already resolved by the caller: the
/// Terminal the new pane was placed beside, and the pane the spawn just
/// created. Selector resolution is not this module's job (ADR-0021).
#[derive(Debug, Clone)]
pub struct Placement {
    /// The Terminal the new pane was placed beside.
    pub owner: ResourceId,
    /// `owner`'s window, from the caller's pre-spawn snapshot.
    pub owner_window: WindowId,
    /// `owner`'s session, from the caller's pre-spawn snapshot.
    pub owner_session: SessionId,
    /// The Terminal the spawn just created.
    pub new_pane: ResourceId,
}

/// What [`verify_and_publish_placement`] did.
#[derive(Debug)]
pub enum RollbackOutcome {
    /// Ownership held and the layout was published; nothing was rolled back.
    Placed,
    /// Placement failed and the rollback `KILL_RESOURCE` confirmed the
    /// spawned pane was removed.
    RolledBack {
        /// Why placement failed.
        reason: String,
    },
    /// Placement failed, and the rollback kill's own outcome could not
    /// confirm the pane was removed (a refusal, an unexpected reply, or a
    /// transport failure attempting it).
    RollbackUnconfirmed {
        /// Why placement failed.
        reason: String,
        /// What the rollback kill attempt itself reported.
        cleanup_note: String,
    },
}

/// Verify explicit ownership after a spawn, then publish the layout.
///
/// An older server may ignore `owner_terminal`, so both panes must still
/// resolve to the owner's window and session in a fresh `GET_STATE` before
/// `new_pane` is spliced beside `owner` (into `projection`'s key when given,
/// ADR-0129). Any failure, transport included, rolls the spawn back with
/// `KILL_RESOURCE` and is reported in the outcome. Degradation notices are
/// appended to `notices` in encounter order.
pub async fn verify_and_publish_placement(
    socket_path: &Path,
    placement: &Placement,
    dir: SplitDir,
    ratio: f32,
    projection: Option<&str>,
    layout_request_id: u32,
    notices: &mut Vec<String>,
) -> RollbackOutcome {
    let (verify_notices, ownership_error) = verify_ownership(socket_path, placement).await;
    notices.extend(verify_notices);
    if let Some(reason) = ownership_error {
        return rollback_after_failure(socket_path, &placement.new_pane, reason, notices).await;
    }

    if let Err(reason) = publish_layout(
        socket_path,
        placement,
        dir,
        ratio,
        projection,
        layout_request_id,
    )
    .await
    {
        return rollback_after_failure(socket_path, &placement.new_pane, reason, notices).await;
    }
    RollbackOutcome::Placed
}

/// Roll `pane` back after `reason`, folding whatever the rollback kill
/// itself reports into the returned [`RollbackOutcome`].
async fn rollback_after_failure(
    socket_path: &Path,
    pane: &ResourceId,
    reason: String,
    notices: &mut Vec<String>,
) -> RollbackOutcome {
    let (cleanup_notices, cleanup) = rollback(socket_path, pane).await;
    notices.extend(cleanup_notices);
    match cleanup {
        CleanupOutcome::Removed => RollbackOutcome::RolledBack { reason },
        CleanupOutcome::Unconfirmed(cleanup_note) => RollbackOutcome::RollbackUnconfirmed {
            reason,
            cleanup_note,
        },
    }
}

/// Connect, send one command, and return its result with the interleaved
/// degradation notices.
async fn request_fresh(
    socket_path: &Path,
    command: Command,
) -> Result<(CommandResult, Vec<String>), AttachError> {
    let mut conn = Connection::connect(socket_path).await?;
    let (result, interleaved) = conn.request(1, command).await?.into_parts();
    let notices = Degradation::from_interleaved(&interleaved)
        .notices()
        .to_vec();
    Ok((result, notices))
}

/// Why both placed panes do not still resolve to the owner's window and
/// session, if they do not; a transport failure counts as unconfirmed.
async fn verify_ownership(
    socket_path: &Path,
    placement: &Placement,
) -> (Vec<String>, Option<String>) {
    let scope = StateScope::Server;
    let (result, notices) = match request_fresh(socket_path, Command::GetState { scope }).await {
        Ok(reply) => reply,
        Err(err) => {
            return (
                Vec::new(),
                Some(format!("ownership verification failed: {err}")),
            );
        }
    };
    let expected = Some((placement.owner_window, placement.owner_session));
    let reason = match result {
        CommandResult::OkWith(CommandValue::State(state)) => {
            let owner_after = ownership_for_terminal(&state, &placement.owner);
            let spawned_after = ownership_for_terminal(&state, &placement.new_pane);
            (owner_after != expected || spawned_after != expected).then_some(
                "server did not honor explicit spawn ownership (unsupported or \
                 ownership mismatch)"
                    .to_owned(),
            )
        }
        other => Some(crate::explain::explain_unexpected(
            "ownership verification",
            &other,
        )),
    };
    (notices, reason)
}

/// Splice `placement.new_pane` beside `placement.owner` in the owner
/// session's shared layout, or the validated `projection` key.
async fn publish_layout(
    socket_path: &Path,
    placement: &Placement,
    dir: SplitDir,
    ratio: f32,
    projection: Option<&str>,
    request_id: u32,
) -> Result<(), String> {
    if let Some(key) = projection {
        crate::layout_ops::validate_projection_key(key, placement.owner_session)
            .map_err(|err| err.to_string())?;
    }
    let mut conn = Connection::connect(socket_path)
        .await
        .map_err(|err| err.to_string())?;
    let mutation = LayoutMutation::SplitPreservingFocus {
        target: placement.owner.clone(),
        new_pane: placement.new_pane.clone(),
        dir,
        ratio,
    };
    let layout = match projection {
        Some(key) => crate::layout_ops::LayoutOps::with_key(
            &mut conn,
            placement.owner_session,
            key.to_owned(),
            request_id,
        ),
        None => Ok(crate::layout_ops::LayoutOps::new(
            &mut conn,
            placement.owner_session,
            request_id,
        )),
    };
    let mut layout = layout.map_err(|err| err.to_string())?;
    layout
        .mutate_or_seed(Workspace::single(placement.owner.clone()), mutation)
        .await
        .map(|_workspace| ())
        .map_err(|err| err.to_string())
}

/// What a rollback `KILL_RESOURCE` reported.
enum CleanupOutcome {
    /// The server confirmed the pane was killed.
    Removed,
    /// A refusal, an unexpected reply, or a transport failure — the raw
    /// text for the caller's diagnostic.
    Unconfirmed(String),
}

/// Kill `pane` (a rollback after a failed placement) and classify the
/// outcome.
async fn rollback(socket_path: &Path, pane: &ResourceId) -> (Vec<String>, CleanupOutcome) {
    let kill = Command::KillResource {
        terminal_id: pane.clone(),
        operation_id: None,
    };
    match request_fresh(socket_path, kill).await {
        Ok((CommandResult::Ok, notices)) => (notices, CleanupOutcome::Removed),
        Ok((other, notices)) => (
            notices,
            CleanupOutcome::Unconfirmed(crate::explain::explain_unexpected("cleanup", &other)),
        ),
        Err(err) => (
            Vec::new(),
            CleanupOutcome::Unconfirmed(format!("cleanup failed: {err}")),
        ),
    }
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
    fn ownership_for_terminal_reads_window_then_session() {
        use phux_protocol::ids::SessionId;
        use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot, WindowInfo};

        let session = SessionId::new(1);
        let window = WindowId::new(10);
        let pane = ResourceId::local(3);
        let snapshot = SessionSnapshot::new(session, window, pane.clone())
            .with_windows(vec![WindowInfo::new(window, session, "one")])
            .with_resources(vec![ResourceInfo::new(pane.clone(), window, 80, 24)]);
        assert_eq!(
            ownership_for_terminal(&snapshot, &pane),
            Some((window, session))
        );
        assert_eq!(
            ownership_for_terminal(&snapshot, &ResourceId::local(99)),
            None
        );
    }
}
