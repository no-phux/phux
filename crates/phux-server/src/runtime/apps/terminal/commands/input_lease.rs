//! Input lease acquire, expiry, and release.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use super::*;

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
            input_holder: Some(crate::runtime::wire_client(client_id)),
            action: grant.action,
            actor: Some(crate::runtime::wire_client(client_id)),
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
pub(in crate::runtime) fn spawn_input_lease_expiry(
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
struct LiveExpiryTaskGuard(std::sync::Arc<std::sync::atomic::AtomicUsize>);

#[cfg(test)]
impl LiveExpiryTaskGuard {
    pub(in crate::runtime) fn new(counter: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
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
            actor: Some(crate::runtime::wire_client(client_id)),
        })
        .await;
    CommandResult::Ok
}
