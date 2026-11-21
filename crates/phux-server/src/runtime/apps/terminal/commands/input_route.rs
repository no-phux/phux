//! Routed input destinations.

use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::{CommandResult, ControlAction, ErrorCode};
use phux_terminal_api::TerminalInput;
use phux_terminal_api::actor::{TerminalHandle, TerminalResource as _};
use tracing::{debug, warn};

use crate::resource::ResourceHandle;
use crate::runtime::commands::{terminal_not_found, wrong_resource_kind};
use crate::state::{ClientId, Resolved, SharedState};

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
        // Revocation: nothing queued before a revocation lands.
        if s.connection_revoked(client_id) {
            return Err(CommandResult::error(
                ErrorCode::PermissionDenied,
                "permission denied",
            ));
        }
        let local = match s.resolve_resource(terminal_id) {
            Resolved::Remote(_) => {
                return Err(CommandResult::error(
                    ErrorCode::UnsupportedSatelliteRoute,
                    format!("ROUTE_INPUT to satellite route unsupported: {terminal_id}"),
                ));
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
        if s.retained().exit(pane).is_some() {
            return Err(exited_input_refusal());
        }
        if s.leases().blocked(pane, client_id) {
            debug!(
                ?client_id,
                ?terminal_id,
                "ROUTE_INPUT blocked: another client holds the input lease (ADR-0033)"
            );
            return Err(CommandResult::error(
                ErrorCode::InputLeaseHeld,
                "input lease held by another client",
            ));
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
/// (ADR-0124): nothing was written, as for a pane with no PTY.
fn exited_input_refusal() -> CommandResult {
    CommandResult::error(
        ErrorCode::InputNotWritten,
        "the pane's process exited; it is retained read-only",
    )
}

pub(crate) fn terminal_input_from_event(event: InputEvent) -> Result<TerminalInput, CommandResult> {
    match event {
        InputEvent::Key(event) => Ok(TerminalInput::Key(event)),
        InputEvent::Mouse(event) => Ok(TerminalInput::Mouse(event)),
        InputEvent::Focus(event) => Ok(TerminalInput::Focus(event)),
        InputEvent::Paste(event) => Ok(TerminalInput::Paste(event)),
        _ => Err(CommandResult::error(
            ErrorCode::InvalidCommand,
            "unsupported ROUTE_INPUT event",
        )),
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
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => CommandResult::error(
            ErrorCode::InternalError,
            "pane actor unavailable for ROUTE_INPUT",
        ),
    }
}

/// The local Terminal behind `terminal_id` as `(pane, handle)`, or the
/// not-found / wrong-kind refusal.
pub(in crate::runtime) fn terminal_handle(
    s: &crate::state::ServerState,
    terminal_id: &phux_protocol::ids::ResourceId,
) -> Result<(phux_core::ids::ResourceId, ResourceHandle), CommandResult> {
    let not_found = || terminal_not_found(terminal_id);
    let core = s
        .idspace
        .resource_from_wire(terminal_id)
        .ok_or_else(not_found)?;
    let handle = s.resources().handle(core).cloned().ok_or_else(not_found)?;
    handle.terminal().map_err(wrong_resource_kind)?;
    Ok((core, handle))
}

/// A granted input lease (ADR-0033), to broadcast through the pane's actor.
pub(in crate::runtime) struct LeaseGrant {
    pub(in crate::runtime) handle: ResourceHandle,
    /// `Acquired` (was free / self) or `Seized` (preempted another).
    pub(in crate::runtime) action: ControlAction,
    pub(in crate::runtime) core: phux_core::ids::ResourceId,
    /// `(ttl_ms, expiry generation)`; `None` for no TTL.
    pub(in crate::runtime) expiry: Option<(u32, u64)>,
}
