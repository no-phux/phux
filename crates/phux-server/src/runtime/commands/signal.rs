//! Terminal signal delivery.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use super::*;

/// Handle `SIGNAL_TERMINAL` (ADR-0033): signal the pane's process group via
/// its actor, leaving the pane addressable.
pub(crate) async fn handle_signal_terminal(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    signal: TerminalSignal,
    operation_id: Option<phux_protocol::ids::IdempotencyKey>,
    commit: Option<&super::super::operation_dedupe::OperationClaim>,
) -> CommandResult {
    let resolved = state.with(|s| {
        let resolved = s.resolve_resource(terminal_id).into_owned();
        let holder = match &resolved {
            ResolvedOwned::Local(local) => s
                .input_lease_holder(local.id)
                .map(super::super::wire_client),
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
        s.terminal_from_wire(terminal_id)
            .is_some_and(|pane| s.retained_exit(pane).is_some())
    }) {
        return CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message: format!("{terminal_id} exited; its process cannot be signalled"),
        };
    }
    let (reply_tx, reply_rx) = oneshot::channel();
    let request = ControlRequest::Signal {
        signal,
        input_holder,
        by: super::super::wire_client(client_id),
        operation_id,
        reply: reply_tx,
    };
    if !commit_signal(&handle.control, request, commit).await {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor unavailable for SIGNAL_TERMINAL".to_owned(),
        };
    }
    let result = actor_reply(reply_rx.await, ErrorCode::InternalError, "SIGNAL_TERMINAL");
    record_signal_failure(commit, &result);
    result
}

/// A command's local Terminal target as `(pane, handle)`, or the refusal for
/// a satellite route, an unknown id, or another resource kind (L1 §1.1).
pub(super) fn local_terminal(
    resolved: ResolvedOwned,
    terminal_id: &phux_protocol::ids::ResourceId,
    verb: &str,
) -> Result<(phux_core::ids::ResourceId, ResourceHandle), CommandResult> {
    match resolved {
        ResolvedOwned::Local(local) => {
            local.handle.terminal().map_err(wrong_resource_kind)?;
            Ok((local.id, local.handle))
        }
        ResolvedOwned::Remote(_) => Err(CommandResult::Error {
            code: ErrorCode::UnsupportedSatelliteRoute,
            message: format!("{verb} on satellite route unsupported: {terminal_id:?}"),
        }),
        ResolvedOwned::Unknown => Err(terminal_not_found(terminal_id)),
    }
}

/// The command result of a pane actor's `Result<(), String>` reply; a
/// refusal carries `refusal_code`.
pub(super) fn actor_reply(
    reply: Result<Result<(), String>, oneshot::error::RecvError>,
    refusal_code: ErrorCode,
    verb: &str,
) -> CommandResult {
    match reply {
        Ok(Ok(())) => CommandResult::Ok,
        Ok(Err(message)) => CommandResult::Error {
            code: refusal_code,
            message,
        },
        Err(_) => CommandResult::Error {
            code: ErrorCode::InternalError,
            message: format!("pane actor dropped {verb} reply"),
        },
    }
}

/// A queued signal that failed replaces the success its key was committed
/// with, so a retry answers the failure (L1 §5.1.1).
pub(super) fn record_signal_failure(
    commit: Option<&super::super::operation_dedupe::OperationClaim>,
    result: &CommandResult,
) {
    if let (Some(claim), CommandResult::Error { .. }) = (commit, result) {
        claim.bind(&super::super::operation_dedupe::CachedOutcome::Signal(
            result.clone(),
        ));
    }
}

/// Queue a signal and commit its key at once (L1 §5.1.1): the actor delivers
/// it even if this handler is cancelled before the reply. `false` when the
/// actor is gone.
pub(super) async fn commit_signal(
    control: &tokio::sync::mpsc::Sender<ControlRequest>,
    request: ControlRequest,
    commit: Option<&super::super::operation_dedupe::OperationClaim>,
) -> bool {
    if control.send(request).await.is_err() {
        return false;
    }
    if let Some(claim) = commit {
        claim.bind(&super::super::operation_dedupe::CachedOutcome::Signal(
            CommandResult::Ok,
        ));
    }
    true
}
