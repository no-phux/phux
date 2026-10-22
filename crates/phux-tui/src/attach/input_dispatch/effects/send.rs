//! The frames a resolved action's effects send, and the bookkeeping
//! that parks them until the reply.

use std::collections::{HashMap, HashSet};

use phux_protocol::ResourceId;
use phux_protocol::ids::SessionId;
use phux_protocol::wire::frame::FrameKind;

use crate::attach::actions::{PendingSplit, PendingWindow};
use crate::attach::connection::Connection;
use crate::attach::directory_picker::PendingDirectory;
use crate::attach::jobs::{Job, Start};
use crate::attach::outcome::AttachError;
use crate::render::overlay::ToastOverlay;

use super::super::ctx::DispatchCtx;
use super::{PendingSessionRename, ReattachTarget};

/// Emit `DETACH` and wait for `DETACHED`; a detach already in flight is
/// not re-sent.
pub(super) async fn send_detach(
    detach: bool,
    conn: &mut Connection,
    detach_pending: &mut bool,
) -> Result<(), AttachError> {
    if !detach || *detach_pending {
        return Ok(());
    }
    conn.send(&FrameKind::Detach).await?;
    conn.unbind_all_terminals();
    *detach_pending = true;
    Ok(())
}

/// Send the parked split/new-window `SPAWN_RESOURCE`s and remember them.
pub(super) async fn send_parked_spawns(
    spawn_terminal: Option<(u32, PendingSplit, FrameKind)>,
    spawn_window: Option<(u32, PendingWindow, FrameKind)>,
    conn: &mut Connection,
    pending_splits: &mut HashMap<u32, PendingSplit>,
    pending_windows: &mut HashMap<u32, PendingWindow>,
) -> Result<(), AttachError> {
    if let Some((request_id, pending, frame)) = spawn_terminal {
        pending_splits.insert(request_id, pending);
        conn.send(&frame).await?;
    }
    if let Some((request_id, pending, frame)) = spawn_window {
        pending_windows.insert(request_id, pending);
        conn.send(&frame).await?;
    }
    Ok(())
}

/// `go-to-directory`: record the listing the picker now waits on, then send
/// the `LIST_DIRECTORY`. Recording first means the reply can never race
/// ahead of the id it has to match.
pub(super) async fn send_directory_request(
    request: Option<(PendingDirectory, FrameKind)>,
    conn: &mut Connection,
    pending_directory: &mut Option<PendingDirectory>,
) -> Result<(), AttachError> {
    let Some((pending, frame)) = request else {
        return Ok(());
    };
    *pending_directory = Some(pending);
    conn.send(&frame).await
}

/// Send kill-pane/kill-window `KILL_RESOURCE`s, parking targets and request
/// ids first: the close or refusal can race back before the send returns.
pub(super) async fn send_kill_frames(
    kill_frames: Vec<FrameKind>,
    targets: Vec<ResourceId>,
    kill_requests: Vec<(u32, ResourceId)>,
    conn: &mut Connection,
    expected_closes: &mut HashSet<ResourceId>,
    pending_kills: &mut HashMap<u32, ResourceId>,
) -> Result<(), AttachError> {
    expected_closes.extend(targets);
    pending_kills.extend(kill_requests);
    for frame in kill_frames {
        conn.send(&frame).await?;
    }
    Ok(())
}

/// ADR-0033: supervisory COMMAND frames (take/give the wheel, signal the
/// pane). Fire-and-forward — the server's `COMMAND_RESULT` + `TerminalControl`
/// broadcast drive the chrome; the input loop does not block on the reply.
pub(super) async fn send_command_frames(
    command_frames: Vec<FrameKind>,
    conn: &mut Connection,
) -> Result<(), AttachError> {
    for frame in command_frames {
        conn.send(&frame).await?;
    }
    Ok(())
}

/// Start a background job (ADR-0172): its notice shows as a passthrough
/// toast; a refusal (its kind is exclusive and already running) toasts why.
pub(super) fn start_job(job: Option<Job>, ctx: &mut DispatchCtx<'_>) {
    let Some(job) = job else {
        return;
    };
    let toast = match ctx.jobs.start(job) {
        Start::Running(None) => return,
        Start::Running(Some(notice)) => {
            ToastOverlay::passthrough(notice.title, notice.lines, ctx.theme)
        }
        Start::Busy(busy) => ToastOverlay::new(busy.title, busy.lines, ctx.theme),
    };
    ctx.overlays.push(Box::new(toast));
}

/// Hand an in-process re-attach to the driver via `ctx.switch_request`.
/// Switching to the current session with no window/pane target is a no-op
/// (the picker's orientation row).
pub(super) fn record_reattach_request(
    reattach: Option<ReattachTarget>,
    switch_request: &mut Option<ReattachTarget>,
    session_name: &str,
    focused_session: Option<SessionId>,
) {
    let Some(target) = reattach else {
        return;
    };
    match target {
        ReattachTarget::Existing {
            name,
            id,
            window: None,
            pane: None,
            resource: None,
        } if is_current_session(id, &name, session_name, focused_session) => {
            tracing::debug!(target_session = %name, "switch-session to current session; no-op");
        }
        ReattachTarget::Existing {
            name,
            id,
            window,
            pane,
            resource,
        } => {
            tracing::info!(target_session = %name, target_id = ?id, target_window = ?window, target_pane = ?pane, target_resource = ?resource, "switch-session requested");
            *switch_request = Some(ReattachTarget::Existing {
                name,
                id,
                window,
                pane,
                resource,
            });
        }
        ReattachTarget::Create {
            name,
            directory,
            host,
        } => {
            tracing::info!(session = %name, directory = ?directory, host = ?host, "new-session requested");
            *switch_request = Some(ReattachTarget::Create {
                name,
                directory,
                host,
            });
        }
    }
}

pub(super) fn is_current_session(
    id: Option<SessionId>,
    name: &str,
    session_name: &str,
    focused_session: Option<SessionId>,
) -> bool {
    id.map_or_else(|| name == session_name, |id| Some(id) == focused_session)
}

/// rename-session via the shared [`phux_client::rename`] policy, correlated
/// by the driver rather than awaited here (a request/response helper would
/// eat interleaved output). A refusal is parked on `rename_notice`; the
/// status name changes only once the server confirms.
///
/// Against an `ORGANIZATION` server the rename is one `SESSION_RENAME`
/// naming the attached session's key, settled by its reply. Against an
/// older server it is a `SET_METADATA` of the translated name key followed
/// by a `GET_STATE` barrier whose snapshot is the outcome.
#[allow(
    clippy::future_not_send,
    reason = "DispatchCtx holds current-thread state"
)]
pub(super) async fn send_session_rename(
    rename_session: Option<String>,
    conn: &mut Connection,
    ctx: &mut DispatchCtx<'_>,
) -> Result<(), AttachError> {
    let Some(new_name) = rename_session.filter(|n| n != ctx.session_name.as_str()) else {
        return Ok(());
    };
    if ctx.rename_pending.is_some() {
        tracing::warn!("rename-session already in flight; dropping a second request");
        return Ok(());
    }
    let session_name = ctx.session_name.clone();
    let roster: Vec<_> = ctx
        .peers
        .sessions
        .iter()
        .map(|session| phux_client::rename::NamedSession {
            id: session.id,
            name: session.name.as_str(),
        })
        .collect();
    let session_id = match phux_client::rename::plan_rename(&roster, &session_name, &new_name) {
        phux_client::rename::RenamePlan::Unchanged { .. } => return Ok(()),
        phux_client::rename::RenamePlan::Refused(refusal) => {
            *ctx.rename_notice = Some(format!("could not rename session to {new_name}: {refusal}"));
            return Ok(());
        }
        phux_client::rename::RenamePlan::Send { session_id } => session_id,
    };
    let session_key = ctx
        .peers
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .and_then(|session| session.key);
    if ctx.organization_supported
        && let Some(key) = session_key
    {
        let request_id = ctx.take_request_id();
        let command = phux_client::rename::org_rename_command(
            phux_client::org::mint_operation(),
            key,
            &new_name,
        );
        conn.send(&FrameKind::Command {
            request_id,
            command,
        })
        .await?;
        tracing::info!(new_name = %new_name, "rename-session sent as SESSION_RENAME");
        *ctx.rename_pending = Some(PendingSessionRename {
            barrier: request_id,
            session_id: Some(session_id),
            current: session_name,
            new_name,
            organization: true,
        });
        return Ok(());
    }
    let request_id = ctx.take_request_id();
    let barrier = ctx.take_request_id();
    let (write, barrier_frame) =
        phux_client::rename::rename_frames(request_id, barrier, &session_name, &new_name);
    conn.send(&write).await?;
    conn.send(&barrier_frame).await?;
    tracing::info!(new_name = %new_name, "rename-session sent; waiting for GET_STATE confirmation");
    *ctx.rename_pending = Some(PendingSessionRename {
        barrier,
        session_id: Some(session_id),
        current: session_name,
        new_name,
        organization: false,
    });
    Ok(())
}
