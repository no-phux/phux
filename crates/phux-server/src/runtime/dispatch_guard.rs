//! Turns a [`crate::policy::enforce`] denial into the reply
//! `docs/spec/workload-auth.md` §6-§7 asks for: a correlated frame or
//! command gets its correlated `PERMISSION_DENIED`; an uncorrelated one is
//! dropped with at most one `ERROR` per second; the connection stays up. The
//! reply names no subject and no rule.

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{
    Command, CommandResult, DirectoryErrorCode, DirectoryListingError, ErrorCode, FrameKind,
    MoveError, MoveResult, SpawnError, SpawnResult,
};

use crate::state::{ClientId, Outbound, SharedState};

/// The one refusal message: no subject, no rule.
const DENIED: &str = "permission denied";

/// Guard a decoded frame after HELLO; `true` means refused and dropped.
/// `HELLO` (a duplicate closes elsewhere) and `COMMAND` (guarded by
/// [`guard_command`]) pass through.
pub(super) async fn refuse_frame(
    state: &SharedState,
    client_id: ClientId,
    frame: &FrameKind,
    negotiated: bool,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> bool {
    if !negotiated || matches!(frame, FrameKind::Hello { .. } | FrameKind::Command { .. }) {
        return false;
    }
    if state
        .with(|s| crate::policy::authorize_frame(s, client_id, frame))
        .is_ok()
    {
        return false;
    }
    if let Some(reply) = frame_refusal(state, client_id, frame) {
        let _ = out_tx.send(Outbound::Frame(reply)).await;
    }
    true
}

/// What the command guard decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Guarded {
    /// Dispatch it now.
    Admitted,
    /// Hold it for a decision (ADR-0128); its result is deferred.
    Held,
    /// Refused; its `COMMAND_RESULT` has been sent.
    Refused,
}

/// Guard a nested command above the input lane, the bulk worker, every
/// handler, and every satellite relay.
pub(super) async fn guard_command(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    command: &Command,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> Guarded {
    match state.with(|s| crate::policy::authorize_command(s, client_id, command)) {
        Ok(crate::policy::Admission::Run) => return Guarded::Admitted,
        Ok(crate::policy::Admission::Hold) => return Guarded::Held,
        Err(_) => {}
    }
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Error {
                code: ErrorCode::PermissionDenied,
                message: DENIED.to_owned(),
            },
        }))
        .await;
    Guarded::Refused
}

/// Guard a QUIC Terminal-stream bind: `OBSERVE` on the bound Terminal.
/// Returns `true` when refused; the caller resets the stream.
pub(super) async fn refuse_stream_bind(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &WireResourceId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> bool {
    if state
        .with(|s| crate::policy::authorize_stream_bind(s, client_id, terminal_id))
        .is_ok()
    {
        return false;
    }
    if state.with_mut(|s| s.admit_denial_error(client_id)) {
        let _ = out_tx.send(Outbound::Frame(denied(None))).await;
    }
    true
}

/// The reply a refused frame gets, if any.
fn frame_refusal(state: &SharedState, client_id: ClientId, frame: &FrameKind) -> Option<FrameKind> {
    if let Some(reply) = native_refusal(frame) {
        return Some(reply);
    }
    if let Some(request_id) = request_id_of(frame) {
        return Some(denied(Some(request_id)));
    }
    // ATTACH has no request id, but its sender waits for an answer.
    if matches!(frame, FrameKind::Attach { .. }) {
        return Some(denied(None));
    }
    state
        .with_mut(|s| s.admit_denial_error(client_id))
        .then(|| denied(None))
}

/// The frame's own reply in its refusal form, where it has one.
fn native_refusal(frame: &FrameKind) -> Option<FrameKind> {
    match frame {
        FrameKind::SpawnResource { request_id, .. } => Some(FrameKind::ResourceSpawned {
            request_id: *request_id,
            result: SpawnResult::Err(SpawnError::SpawnFailed(DENIED.to_owned())),
        }),
        FrameKind::MoveResource { request_id, .. } => Some(FrameKind::ResourceMoved {
            request_id: *request_id,
            result: MoveResult::Err(MoveError::MoveFailed(DENIED.to_owned())),
        }),
        FrameKind::ListDirectory {
            request_id, path, ..
        } => Some(FrameKind::DirectoryListing {
            request_id: *request_id,
            result: Err(DirectoryListingError {
                path: path.clone(),
                code: DirectoryErrorCode::PermissionDenied,
                message: DENIED.to_owned(),
            }),
        }),
        _ => None,
    }
}

/// The request id of a correlated client frame whose reply has no refusal
/// form of its own, so the correlated `ERROR` is its refusal.
const fn request_id_of(frame: &FrameKind) -> Option<u32> {
    match frame {
        FrameKind::GetMetadata { request_id, .. }
        | FrameKind::SetMetadata { request_id, .. }
        | FrameKind::DeleteMetadata { request_id, .. }
        | FrameKind::ListMetadata { request_id, .. } => Some(*request_id),
        _ => None,
    }
}

fn denied(request_id: Option<u32>) -> FrameKind {
    FrameKind::Error {
        request_id,
        code: ErrorCode::PermissionDenied,
        message: DENIED.to_owned(),
    }
}
