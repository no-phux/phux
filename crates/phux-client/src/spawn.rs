//! Wire primitives for `phux spawn` / `phux launch` (`SPAWN_RESOURCE`, SPEC
//! L1 §3.1), plus the ownership-verify + `KILL_RESOURCE` rollback dance
//! behind explicit placement.
//!
//! Explicit placement addresses an exact owning window and splices the new
//! leaf into that window's shared layout ([`crate::layout_ops`]). Selector
//! resolution (which pane `--target` names) stays client-side (ADR-0021) and
//! is the caller's job; this module starts from an already-resolved owner.

use std::path::Path;

use phux_protocol::ids::{ResourceId, SessionId, WindowId};
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
/// A transport failure verifying ownership or rolling back is folded into
/// the returned outcome's reason/cleanup text rather than propagated,
/// matching the historical behavior of reporting placement failure rather
/// than a bare connection error.
pub async fn verify_and_publish_placement(
    socket_path: &Path,
    placement: &Placement,
    dir: SplitDir,
    ratio: f32,
    layout_request_id: u32,
    notices: &mut Vec<String>,
) -> RollbackOutcome {
    let (verify_notices, ownership_error) = verify_ownership(socket_path, placement).await;
    notices.extend(verify_notices);
    if let Some(reason) = ownership_error {
        return rollback_after_failure(socket_path, &placement.new_pane, reason, notices).await;
    }

    if let Err(reason) = publish_layout(socket_path, placement, dir, ratio, layout_request_id).await
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
/// `placement.owner_session`'s shared layout.
async fn publish_layout(
    socket_path: &Path,
    placement: &Placement,
    dir: SplitDir,
    ratio: f32,
    request_id: u32,
) -> Result<(), String> {
    let mut conn = Connection::connect(socket_path)
        .await
        .map_err(|err| err.to_string())?;
    let mutation = LayoutMutation::SplitPreservingFocus {
        target: placement.owner.clone(),
        new_pane: placement.new_pane.clone(),
        dir,
        ratio,
    };
    crate::layout_ops::LayoutOps::new(&mut conn, placement.owner_session, request_id)
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
}
