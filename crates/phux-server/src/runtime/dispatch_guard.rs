//! The dispatch guard's three call sites in the client loop, and the refusal
//! each one sends (`docs/spec/workload-auth.md` §6, §7).
//!
//! The decision is [`crate::policy::enforce`]; this module only turns a
//! denial into the reply the spec asks for:
//!
//! - a correlated frame or command gets its ordinary correlated error
//!   carrying `PERMISSION_DENIED`;
//! - an uncorrelated frame (`INPUT_*`, `FRAME_ACK`, a stream bind, ...) is
//!   dropped, with at most one uncorrelated `ERROR` per second per
//!   connection;
//! - the connection stays up.
//!
//! The reply names no subject and no rule, so a refused client learns
//! nothing about what exists. The owner's grant admits everything, so for a
//! local or transitional server every function here returns `false` and the
//! client loop runs exactly as it did before enforcement.

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{
    Command, CommandResult, DirectoryErrorCode, DirectoryListingError, ErrorCode, FrameKind,
    MoveError, MoveResult, SpawnError, SpawnResult,
};

use crate::state::{ClientId, Outbound, SharedState};

/// The one refusal message: no subject, no rule.
const DENIED: &str = "permission denied";

/// Guard a decoded frame before any routing or handler. Returns `true` when
/// the frame was refused and must be dropped.
///
/// Runs only after HELLO: before it, the only frames the loop accepts are
/// HELLO and PING, and no grant exists yet. A second HELLO passes through to
/// `negotiate_hello`, whose duplicate check is the PRE_HELLO-only rule and
/// closes the connection. `COMMAND` passes through to the command guard,
/// which classifies the nested tag.
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

/// Guard a nested command above the input lane, the bulk worker, every
/// handler, and every satellite relay. Returns `true` when the command was
/// refused; its `COMMAND_RESULT` has been sent.
pub(super) async fn refuse_command(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    command: &Command,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> bool {
    if state
        .with(|s| crate::policy::authorize_command(s, client_id, command))
        .is_ok()
    {
        return false;
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
    true
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
    // ATTACH carries no request id, but its sender waits for an answer, so
    // it always gets one, exactly as `SESSION_NOT_FOUND` is sent.
    if matches!(frame, FrameKind::Attach { .. }) {
        return Some(denied(None));
    }
    state
        .with_mut(|s| s.admit_denial_error(client_id))
        .then(|| denied(None))
}

/// The frame's own reply, in its refusal form, where that form can say
/// "permission denied": a waiter for the reply ends on it as on any other
/// refusal of the same request.
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

const fn denied_code() -> ErrorCode {
    ErrorCode::PermissionDenied
}

fn denied(request_id: Option<u32>) -> FrameKind {
    FrameKind::Error {
        request_id,
        code: denied_code(),
        message: DENIED.to_owned(),
    }
}
