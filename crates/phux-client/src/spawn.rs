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
/// `RESOURCE_SPAWNED` result, paired with whatever the server interleaved
/// ahead of it.
///
/// A peer that answers a spawn with a correlated `ERROR` instead of
/// `RESOURCE_SPAWNED` (permitted for a relayed spawn: `relay.rs`'s
/// `handle_inbound` states a satellite MAY do this) is folded into
/// [`SpawnResult::Err`] rather than left unanswered — a hand-rolled wait that
/// only matched `RESOURCE_SPAWNED` used to wedge on exactly this reply.
///
/// The correlation id is read out of `frame`'s own `request_id` field, so
/// the id sent and the id waited on cannot drift.
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
/// A spawn that asks for retention (`retain_secs`, ADR-0124) needs
/// `RETAIN_ON_EXIT`; a keyed spawn (`idempotency_key`, ADR-0126) needs
/// `SPAWN_IDEMPOTENCY`. A server without the bit skips the field by length
/// and silently does something else (closes the pane at exit, spawns a
/// second pane on a retry), so the caller refuses before sending.
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
/// `placement.owner` and `placement.new_pane` must both still resolve to
/// `(placement.owner_window, placement.owner_session)`; on success
/// `new_pane` is spliced beside `owner` in that session's shared layout.
/// Either failure rolls the spawn back with `KILL_RESOURCE`.
///
/// Field-tagged compatibility means an older server can legally ignore
/// `owner_terminal`, so ownership is verified against a fresh `GET_STATE`
/// before layout is published — otherwise L3 could reference a pane that
/// belongs to another session/window.
///
/// Degradation notices observed along the way (the ownership-verify
/// `GET_STATE`, and the rollback kill if one runs) are appended to
/// `notices`, in encounter order, so a caller can print them in the order
/// they were seen.
///
/// `projection`, when given, names the shared-layout metadata key
/// (`--projection`, ADR-0129) the new pane is spliced into instead of the
/// default `phux.tui.layout/v1/<session>` envelope; it is validated against
/// `placement.owner_session` before anything is written.
///
/// A transport failure verifying ownership or rolling back is folded into
/// the returned outcome's reason/cleanup text rather than propagated,
/// matching the historical behavior of reporting placement failure rather
/// than a bare connection error.
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

/// Verify that `placement.owner` and `placement.new_pane` both still
/// resolve to `(placement.owner_window, placement.owner_session)` after the
/// spawn. A transport failure here is folded into the returned reason
/// exactly like a semantic mismatch would be — the caller treats both as
/// "placement could not be confirmed" and rolls back either way.
async fn verify_ownership(
    socket_path: &Path,
    placement: &Placement,
) -> (Vec<String>, Option<String>) {
    let mut conn = match Connection::connect(socket_path).await {
        Ok(conn) => conn,
        Err(err) => {
            return (
                Vec::new(),
                Some(format!("ownership verification failed: {err}")),
            );
        }
    };
    let request = conn
        .request(
            1,
            Command::GetState {
                scope: StateScope::Server,
            },
        )
        .await;
    drop(conn);
    match request {
        Ok(reply) => {
            let (result, interleaved) = reply.into_parts();
            let notices = Degradation::from_interleaved(&interleaved)
                .notices()
                .to_vec();
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
        Err(err) => (
            Vec::new(),
            Some(format!("ownership verification failed: {err}")),
        ),
    }
}

/// Splice `placement.new_pane` beside `placement.owner` in
/// `placement.owner_session`'s shared layout — the named `projection` key
/// when given (validated against `placement.owner_session` first), the
/// default `phux.tui.layout/v1/<session>` envelope otherwise.
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
    let mut conn = match Connection::connect(socket_path).await {
        Ok(conn) => conn,
        Err(err) => {
            return (
                Vec::new(),
                CleanupOutcome::Unconfirmed(format!("cleanup failed: {err}")),
            );
        }
    };
    match conn
        .request(
            1,
            Command::KillResource {
                terminal_id: pane.clone(),
                operation_id: None,
            },
        )
        .await
    {
        Ok(reply) => {
            let (result, interleaved) = reply.into_parts();
            let notices = Degradation::from_interleaved(&interleaved)
                .notices()
                .to_vec();
            let outcome = match result {
                CommandResult::Ok => CleanupOutcome::Removed,
                other => CleanupOutcome::Unconfirmed(crate::explain::explain_unexpected(
                    "cleanup", &other,
                )),
            };
            (notices, outcome)
        }
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

    /// Long enough that a loaded machine cannot trip it, short enough that a
    /// genuine wedge fails this test instead of hanging the run.
    const WEDGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

    #[tokio::test]
    async fn satellite_refusal_ends_the_spawn_instead_of_wedging_it() {
        // phux-h5hj.12. A satellite MAY answer a relayed spawn with a
        // generic correlated ERROR instead of RESOURCE_SPAWNED; a wait that
        // only matched RESOURCE_SPAWNED wedged on exactly this reply.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let socket = temp.path().join("refusing.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind");
        let spec = crate::testkit::ScriptSpec::new().refuse_spawn(
            ErrorCode::UnsupportedSatelliteRoute,
            "no satellite route to build-box",
        );
        let server =
            tokio::spawn(
                async move { crate::testkit::ScriptedServer::accept(&listener, spec).await },
            );

        let (result, _degradation) =
            tokio::time::timeout(WEDGE_TIMEOUT, spawn_on(&socket, &spawn_frame()))
                .await
                .expect("a refused spawn must return; a timeout here is the wedge itself")
                .expect("transport");

        match result {
            SpawnResult::Err(SpawnError::SpawnFailed(reason)) => {
                assert!(
                    reason.contains("no satellite route to build-box"),
                    "the refusal must reach the operator, got {reason:?}"
                );
            }
            other => panic!("a correlated ERROR is this spawn's answer, got {other:?}"),
        }
        server.await.expect("scripted server");
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

    #[tokio::test]
    async fn a_scoped_denial_ends_the_spawn_with_its_own_refusal() {
        // A paired server refuses an out-of-scope SPAWN_RESOURCE with the
        // spawn's own reply, RESOURCE_SPAWNED carrying SpawnFailed
        // ("permission denied", workload-auth §7). The wait must end on it.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let socket = temp.path().join("scoped.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind");
        let spec = crate::testkit::ScriptSpec::new().spawn_result(SpawnResult::Err(
            SpawnError::SpawnFailed("permission denied".to_owned()),
        ));
        let server =
            tokio::spawn(
                async move { crate::testkit::ScriptedServer::accept(&listener, spec).await },
            );

        let (result, _degradation) =
            tokio::time::timeout(WEDGE_TIMEOUT, spawn_on(&socket, &spawn_frame()))
                .await
                .expect("a denied spawn must return; a timeout here is the wedge itself")
                .expect("transport");

        assert!(
            matches!(
                &result,
                SpawnResult::Err(SpawnError::SpawnFailed(reason)) if reason == "permission denied"
            ),
            "the denial must reach the operator, got {result:?}"
        );
        server.await.expect("scripted server");
    }
}
