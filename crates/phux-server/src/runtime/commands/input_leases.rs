//! Input leases: extracted from the shared module.
use super::{
    ClientId, CommandResult, ControlAction, ControlRequest, ErrorCode, InputEvent, InputMode,
    Resolved, ResolvedOwned, ResourceHandle, SharedState, TerminalHandle, TerminalInput,
    TerminalSignal, debug, oneshot, terminal_not_found, warn, wrong_resource_kind,
};

/// A resolved, lease-checked local input destination.
#[derive(Debug)]
pub(crate) struct InputDestination {
    pub(crate) pane: phux_core::ids::ResourceId,
    /// The Terminal facet: input atoms only ever go to a Terminal, so the
    /// resolver settles the kind before `action` runs.
    pub(crate) handle: TerminalHandle,
}

/// Resolve and lease-gate a local headless destination, then run `action`
/// while the authority lock remains held.
pub(crate) fn with_route_input_destination<R>(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    action: impl FnOnce(InputDestination) -> R,
) -> Result<R, CommandResult> {
    state.with(|s| {
        // workload-auth §7 step 2: nothing queued before a revocation lands.
        if s.connection_revoked(client_id) {
            return Err(CommandResult::Error {
                code: ErrorCode::PermissionDenied,
                message: "permission denied".to_owned(),
            });
        }
        let local = match s.resolve_resource(terminal_id) {
            Resolved::Remote(_) => {
                return Err(CommandResult::Error {
                    code: ErrorCode::UnsupportedSatelliteRoute,
                    message: format!("ROUTE_INPUT to satellite route unsupported: {terminal_id:?}"),
                });
            }
            Resolved::Unknown => {
                return Err(terminal_not_found(terminal_id));
            }
            Resolved::Local(local) => local,
        };
        let pane = local.id;
        if s.native_owned_terminal(pane) {
            return Err(CommandResult::Error {
                code: ErrorCode::InputNotWritten,
                message: "SDK-owned guardian input is diagnostics-only; use native agent commands"
                    .to_owned(),
            });
        }
        if s.retained_exit(pane).is_some() {
            return Err(exited_input_refusal());
        }
        if s.input_blocked(pane, client_id) {
            debug!(
                ?client_id,
                ?terminal_id,
                "ROUTE_INPUT blocked: another client holds the input lease (ADR-0033)"
            );
            return Err(CommandResult::Error {
                code: ErrorCode::InputLeaseHeld,
                message: "input lease held by another client".to_owned(),
            });
        }
        let handle = local
            .handle
            .terminal()
            .map_err(wrong_resource_kind)?
            .clone();
        Ok(action(InputDestination { pane, handle }))
    })
}

/// Input to a Terminal whose process exited and which is retained read-only
/// (ADR-0124): nothing was written, as for a pane with no PTY (L1 §6.2.1).
pub(super) fn exited_input_refusal() -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::InputNotWritten,
        message: "the pane's process exited; it is retained read-only".to_owned(),
    }
}

pub(crate) fn terminal_input_from_event(event: InputEvent) -> Result<TerminalInput, CommandResult> {
    match event {
        InputEvent::Key(event) => Ok(TerminalInput::Key(event)),
        InputEvent::Mouse(event) => Ok(TerminalInput::Mouse(event)),
        InputEvent::Focus(event) => Ok(TerminalInput::Focus(event)),
        InputEvent::Paste(event) => Ok(TerminalInput::Paste(event)),
        _ => Err(CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message: "unsupported ROUTE_INPUT event".to_owned(),
        }),
    }
}

pub(crate) fn handle_route_input(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    event: InputEvent,
) -> CommandResult {
    debug!(?client_id, ?terminal_id, "ROUTE_INPUT delivering input");
    let input = match terminal_input_from_event(event) {
        Ok(input) => input,
        Err(result) => return result,
    };
    let send = match with_route_input_destination(state, client_id, terminal_id, |destination| {
        destination.handle.input.try_send(input)
    }) {
        Ok(send) => send,
        Err(result) => return result,
    };
    match send {
        Ok(()) => CommandResult::Ok,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            warn!(
                ?terminal_id,
                "ROUTE_INPUT mailbox full; dropping (fire-and-forget per SPEC §9)"
            );
            CommandResult::Ok
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor unavailable for ROUTE_INPUT".to_owned(),
        },
    }
}

/// The local Terminal behind `terminal_id` as `(pane, handle)`, or the
/// not-found / wrong-kind refusal.
pub(super) fn terminal_handle(
    s: &crate::state::ServerState,
    terminal_id: &phux_protocol::ids::ResourceId,
) -> Result<(phux_core::ids::ResourceId, ResourceHandle), CommandResult> {
    let not_found = || terminal_not_found(terminal_id);
    let core = s.terminal_from_wire(terminal_id).ok_or_else(not_found)?;
    let handle = s.resource_handle(core).cloned().ok_or_else(not_found)?;
    handle.terminal().map_err(wrong_resource_kind)?;
    Ok((core, handle))
}

/// A granted input lease (ADR-0033), to broadcast through the pane's actor.
pub(super) struct LeaseGrant {
    pub(super) handle: ResourceHandle,
    /// `Acquired` (was free / self) or `Seized` (preempted another).
    pub(super) action: ControlAction,
    pub(super) core: phux_core::ids::ResourceId,
    /// `(ttl_ms, expiry generation)`; `None` for no TTL.
    pub(super) expiry: Option<(u32, u64)>,
}

/// Handle `ACQUIRE_INPUT` (ADR-0033): take the pane's exclusive input lease.
/// `Cooperative` fails with `InputLeaseHeld` against another holder; `Seize`
/// preempts. A grant broadcasts `TerminalControl` and arms `ttl_ms`
/// (`0` = never). Satellite ids never reach here (`route_to_satellite`).
pub(crate) async fn handle_acquire_input(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    mode: InputMode,
    ttl_ms: u32,
) -> CommandResult {
    let granted = state.with_mut(|s| {
        let (core, handle) = terminal_handle(s, terminal_id)?;
        let prior = s.input_lease_holder(core);
        if mode == InputMode::Cooperative
            && let Some(holder) = prior
            && holder != client_id
        {
            return Err(CommandResult::Error {
                code: ErrorCode::InputLeaseHeld,
                message: format!("input lease held by client {}", holder.0),
            });
        }
        // ADR-0163 §6: a non-human principal never takes a human's lease,
        // however deliberately it asks.
        if s.lease_protected_from(core, client_id) {
            return Err(CommandResult::Error {
                code: ErrorCode::InputLeaseHeld,
                message: "input lease held by a human; an agent cannot take it".to_owned(),
            });
        }
        s.set_input_lease(core, client_id);
        let action = match prior {
            Some(holder) if holder != client_id => ControlAction::Seized,
            _ => ControlAction::Acquired,
        };
        // Every grant re-arms the TTL, superseding the prior timer.
        let expiry = s
            .refresh_input_lease_expiry(core, ttl_ms != 0)
            .map(|generation| (ttl_ms, generation));
        Ok(LeaseGrant {
            handle,
            action,
            core,
            expiry,
        })
    });
    let grant = match granted {
        Ok(grant) => grant,
        Err(refusal) => return refusal,
    };
    let _ = grant
        .handle
        .control
        .send(ControlRequest::LeaseChanged {
            input_holder: Some(super::super::wire_client(client_id)),
            action: grant.action,
            actor: Some(super::super::wire_client(client_id)),
        })
        .await;
    if let Some((ttl_ms, generation)) = grant.expiry {
        spawn_input_lease_expiry(state, grant.core, generation, ttl_ms);
    }
    CommandResult::Ok
}

/// Schedule the timer that expires `terminal`'s input lease after `ttl_ms`
/// (ADR-0033). Any later acquire, release, or disconnect bumps the pane's
/// expiry generation and aborts this task; the generation re-check under the
/// lock covers the window before the abort handle is attached, so exactly
/// one `terminal_control` results.
pub(super) fn spawn_input_lease_expiry(
    state: &SharedState,
    terminal: phux_core::ids::ResourceId,
    generation: u64,
    ttl_ms: u32,
) {
    let task_state = state.clone();
    #[cfg(test)]
    let counter = state.with(crate::state::ServerState::input_lease_expiry_task_counter);
    let join = tokio::task::spawn_local(async move {
        #[cfg(test)]
        let _live = LiveExpiryTaskGuard::new(counter);
        tokio::time::sleep(std::time::Duration::from_millis(u64::from(ttl_ms))).await;
        let expired = task_state.with_mut(|s| {
            if !s.input_lease_expiry_is_current(terminal, generation) {
                return None;
            }
            // Not `refresh_input_lease_expiry`, which would abort this task.
            s.clear_input_lease_expiry(terminal);
            s.expire_input_lease(terminal)?;
            s.resource_handle(terminal).cloned()
        });
        let Some(handle) = expired else {
            return;
        };
        let _ = handle
            .control
            .send(ControlRequest::LeaseChanged {
                input_holder: None,
                action: ControlAction::Expired,
                actor: None,
            })
            .await;
    });
    // `false`: `generation` was already superseded, so abort now.
    let abort = join.abort_handle();
    let attached =
        state.with_mut(|s| s.attach_input_lease_expiry_abort(terminal, generation, abort.clone()));
    if !attached {
        abort.abort();
    }
}

/// Counts live lease-expiry tasks for `lease_expiry_task_tests`; an abort
/// drops it too.
#[cfg(test)]
pub(super) struct LiveExpiryTaskGuard(std::sync::Arc<std::sync::atomic::AtomicUsize>);

#[cfg(test)]
impl LiveExpiryTaskGuard {
    pub(super) fn new(counter: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self(counter)
    }
}

#[cfg(test)]
impl Drop for LiveExpiryTaskGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Handle `RELEASE_INPUT` (ADR-0033): drop the caller's input lease.
/// Idempotent; broadcasts `Released` only when a lease was given up.
pub(crate) async fn handle_release_input(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
) -> CommandResult {
    let released = state.with_mut(|s| {
        let (core, handle) = terminal_handle(s, terminal_id)?;
        let did_release = s.release_input_lease(core, client_id);
        if did_release {
            // The lease's TTL goes with it.
            s.refresh_input_lease_expiry(core, false);
        }
        Ok(did_release.then_some(handle))
    });
    let handle = match released {
        Ok(Some(handle)) => handle,
        Ok(None) => return CommandResult::Ok,
        Err(refusal) => return refusal,
    };
    let _ = handle
        .control
        .send(ControlRequest::LeaseChanged {
            input_holder: None,
            action: ControlAction::Released,
            actor: Some(super::super::wire_client(client_id)),
        })
        .await;
    CommandResult::Ok
}

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
