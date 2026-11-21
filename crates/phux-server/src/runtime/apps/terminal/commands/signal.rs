//! Terminal signal delivery.

use phux_protocol::wire::frame::{CommandResult, ErrorCode, TerminalSignal};
use phux_terminal_api::actor::{ControlRequest, TerminalResource as _};
use tokio::sync::oneshot;

use crate::resource::ResourceHandle;
use crate::runtime::commands::{actor_reply, terminal_not_found, wrong_resource_kind};
use crate::state::{ClientId, ResolvedOwned, SharedState};

/// Handle `SIGNAL_TERMINAL` (ADR-0033): signal the pane's process group via
/// its actor, leaving the pane addressable.
pub(crate) async fn handle_signal_terminal(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    signal: TerminalSignal,
    operation_id: Option<phux_protocol::ids::IdempotencyKey>,
    commit: Option<&crate::runtime::operation_dedupe::OperationClaim>,
) -> CommandResult {
    let resolved = state.with(|s| {
        let resolved = s.resolve_resource(terminal_id).into_owned();
        let holder = match &resolved {
            ResolvedOwned::Local(local) => {
                s.leases().holder(local.id).map(crate::runtime::wire_client)
            }
            ResolvedOwned::Remote(_) | ResolvedOwned::Unknown => None,
        };
        (resolved, holder)
    });
    let (resolved, input_holder) = resolved;
    let (_, handle) = match local_terminal(resolved, terminal_id, "SIGNAL_TERMINAL") {
        Ok(local) => local,
        Err(refusal) => return refusal,
    };
    // ADR-0124: a retained pane's child is already reaped.
    if state.with(|s| {
        s.idspace
            .resource_from_wire(terminal_id)
            .is_some_and(|pane| s.retained().exit(pane).is_some())
    }) {
        return CommandResult::error(
            ErrorCode::InvalidCommand,
            format!("{terminal_id} exited; its process cannot be signalled"),
        );
    }
    let (reply_tx, reply_rx) = oneshot::channel();
    let request = ControlRequest::Signal {
        signal,
        input_holder,
        by: crate::runtime::wire_client(client_id),
        operation_id,
        reply: reply_tx,
    };
    let Ok(terminal) = handle.terminal() else {
        return CommandResult::error(
            ErrorCode::InternalError,
            "pane actor unavailable for SIGNAL_TERMINAL",
        );
    };
    if !commit_signal(&terminal.control, request, commit).await {
        return CommandResult::error(
            ErrorCode::InternalError,
            "pane actor unavailable for SIGNAL_TERMINAL",
        );
    }
    let result = actor_reply(reply_rx.await, ErrorCode::InternalError, "SIGNAL_TERMINAL");
    record_signal_failure(commit, &result);
    result
}

/// A command's local Terminal target as `(pane, handle)`, or the refusal for
/// a satellite route, an unknown id, or another resource kind.
pub(in crate::runtime) fn local_terminal(
    resolved: ResolvedOwned,
    terminal_id: &phux_protocol::ids::ResourceId,
    verb: &str,
) -> Result<(phux_core::ids::ResourceId, ResourceHandle), CommandResult> {
    match resolved {
        ResolvedOwned::Local(local) => {
            local.handle.terminal().map_err(wrong_resource_kind)?;
            Ok((local.id, local.handle))
        }
        ResolvedOwned::Remote(_) => Err(CommandResult::error(
            ErrorCode::UnsupportedSatelliteRoute,
            format!("{verb} on satellite route unsupported: {terminal_id}"),
        )),
        ResolvedOwned::Unknown => Err(terminal_not_found(terminal_id)),
    }
}

/// A queued signal that failed replaces the success its key was committed
/// with, so a retry answers the failure.
pub(in crate::runtime) fn record_signal_failure(
    commit: Option<&crate::runtime::operation_dedupe::OperationClaim>,
    result: &CommandResult,
) {
    if let (Some(claim), CommandResult::Error { .. }) = (commit, result) {
        claim.bind(&crate::runtime::operation_dedupe::CachedOutcome::Signal(
            result.clone(),
        ));
    }
}

/// Queue a signal and commit its key at once: the actor delivers
/// it even if this handler is cancelled before the reply. `false` when the
/// actor is gone.
pub(in crate::runtime) async fn commit_signal(
    control: &tokio::sync::mpsc::Sender<ControlRequest>,
    request: ControlRequest,
    commit: Option<&crate::runtime::operation_dedupe::OperationClaim>,
) -> bool {
    if control.send(request).await.is_err() {
        return false;
    }
    if let Some(claim) = commit {
        claim.bind(&crate::runtime::operation_dedupe::CachedOutcome::Signal(
            CommandResult::Ok,
        ));
    }
    true
}
