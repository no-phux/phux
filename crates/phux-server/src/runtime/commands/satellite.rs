//! Satellite command relay.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use super::*;

/// Relay one satellite-targeted command (already rewritten to the
/// satellite's `Local` ids) over the owning hub link and send the correlated
/// `COMMAND_RESULT` (ADR-0007 §4). A non-hub server answers
/// `UnsupportedSatelliteRoute`; an unreachable satellite fails fast.
///
/// Stream-establishing commands register the caller as a hub-side proxy
/// subscriber atomically with the relay; `DETACH_RESOURCE` is resolved
/// hub-side so one consumer's detach never tears down the shared link
/// stream. Every hub consumer is one identity on the satellite, so the hub
/// owns input-lease exclusion between its consumers (L1 §9.1).
#[allow(
    clippy::too_many_arguments,
    reason = "caller identity, routed command, mailbox, and negotiated bootstrap context"
)]
pub(super) async fn handle_satellite_command(
    ctx: &CommandContext,
    request_id: u32,
    host: &phux_protocol::ids::SatelliteHost,
    command: Command,
) {
    let CommandContext {
        state,
        client_id,
        out_tx,
        ..
    } = ctx;
    let client_id = *client_id;
    note_satellite_use(state, host, client_id, &command);
    let result = match state.with(|s| s.hub_relay(host)) {
        None => CommandResult::Error {
            code: ErrorCode::UnsupportedSatelliteRoute,
            message: format!(
                "no satellite route to {host:?}: this server is not a federation hub \
                 for that host (check `phux server --hub` and the [[satellites]] registry)"
            ),
        },
        Some(relay) => match &command {
            Command::AttachResource {
                terminal_id,
                role_policy,
                ..
            } => {
                relay_satellite_attach(
                    ctx,
                    &SatelliteLeaseTarget::new(state, host, client_id, terminal_id),
                    &relay,
                    &command,
                    role_policy.unwrap_or_default(),
                )
                .await
            }
            Command::SubscribeResourceEvents { terminal_id, .. } => {
                let change =
                    register_satellite_event_filter(state, client_id, host, &command, out_tx);
                let result = relay_stream_establishing(
                    ctx,
                    &relay,
                    &unfiltered_for_link(&command),
                    terminal_id,
                    None,
                )
                .await;
                settle_satellite_event_filter(state, client_id, change, &result);
                result
            }
            Command::DetachResource { terminal_id } => {
                resolve_hub_detach_terminal(state, &relay, client_id, host, terminal_id).await
            }
            Command::AcquireInput {
                terminal_id, mode, ..
            } => {
                relay_satellite_acquire_input(
                    &SatelliteLeaseTarget::new(state, host, client_id, terminal_id),
                    &relay,
                    *mode,
                    &command,
                    out_tx,
                )
                .await
            }
            Command::ReleaseInput { terminal_id } => {
                relay_satellite_release_input(
                    &SatelliteLeaseTarget::new(state, host, client_id, terminal_id),
                    &relay,
                    &command,
                )
                .await
            }
            // L1 §9.1: `APPLY_INPUT` crosses the same lease gate as
            // `ROUTE_INPUT`, then the satellite owns its dedupe.
            Command::RouteInput { terminal_id, .. } | Command::ApplyInput { terminal_id, .. } => {
                relay_satellite_route_input(
                    &SatelliteLeaseTarget::new(state, host, client_id, terminal_id),
                    &relay,
                    &command,
                )
                .await
            }
            Command::KillResourceIf { .. } => {
                relay_conditional_kill(state, &relay, host, &command, client_id).await
            }
            _ => relay.command_from(command.clone(), client_id).await,
        },
    };
    reply_satellite_command(ctx, request_id, host, &command, result).await;
}

/// ADR-0109 (L1 §5.2.1): the resource `command` attaches, drives, or reads,
/// which counts as attaching it for `UNATTACHED_SINCE_SPAWN` whether or not
/// the command then succeeds.
const fn used_resource(command: &Command) -> Option<&phux_protocol::ids::ResourceId> {
    match command {
        Command::AttachResource { terminal_id, .. }
        | Command::RouteInput { terminal_id, .. }
        | Command::ApplyInput { terminal_id, .. }
        | Command::AcquireInput { terminal_id, .. }
        | Command::PutFile { terminal_id, .. }
        | Command::Transcribe { terminal_id, .. }
        | Command::SignalTerminal { terminal_id, .. }
        | Command::GetScreen { terminal_id, .. } => Some(terminal_id),
        _ => None,
    }
}

/// Note a local resource `command` uses (see [`used_resource`]) before the
/// command runs, so a conditional kill checked after it sees it. A
/// satellite-tagged id is the hub's to note, in [`note_satellite_use`].
pub(in crate::runtime) fn note_local_use(
    state: &SharedState,
    client_id: ClientId,
    command: &Command,
) {
    if let Some(terminal_id) = used_resource(command) {
        state.with_mut(|s| s.note_resource_use(terminal_id, client_id));
    }
}

/// ADR-0109: note a hub consumer's use of a satellite resource in the hub's
/// spawn ledger before relaying; only the hub can tell its consumers apart.
fn note_satellite_use(
    state: &SharedState,
    host: &phux_protocol::ids::SatelliteHost,
    client_id: ClientId,
    command: &Command,
) {
    let Some(id) = used_resource(command).and_then(phux_protocol::ids::ResourceId::local_id) else {
        return;
    };
    state.with_mut(|s| s.hub_note_satellite_use(host, id, client_id));
}

/// Relay a `KILL_RESOURCE_IF` once the hub has checked the half of
/// `UNATTACHED_SINCE_SPAWN` only it can see (ADR-0109, L1 §9.1); the
/// satellite checks the rest. A refusal never touches the link.
async fn relay_conditional_kill(
    state: &SharedState,
    relay: &crate::hub::relay::RelayHandle,
    host: &phux_protocol::ids::SatelliteHost,
    command: &Command,
    client_id: ClientId,
) -> CommandResult {
    let Command::KillResourceIf {
        terminal_id,
        precondition,
        ..
    } = command
    else {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "a conditional-kill relay was handed another command".to_owned(),
        };
    };
    if !hub_vouches_for_kill(state, host, terminal_id, precondition) {
        return CommandResult::Error {
            code: ErrorCode::PreconditionFailed,
            message: format!(
                "{host}/{terminal_id} not killed: this hub did not spawn it under that \
                 instance token, no longer remembers it, or another hub consumer has \
                 attached or used it"
            ),
        };
    }
    relay.command_from(command.clone(), client_id).await
}

/// `true` unless the kill asks for `UNATTACHED_SINCE_SPAWN` and the hub's
/// spawn ledger cannot vouch for it under the kill's instance token.
fn hub_vouches_for_kill(
    state: &SharedState,
    host: &phux_protocol::ids::SatelliteHost,
    terminal_id: &phux_protocol::ids::ResourceId,
    precondition: &phux_protocol::wire::frame::KillPrecondition,
) -> bool {
    use phux_protocol::wire::frame::KillConditions;
    if !precondition
        .conditions
        .contains(KillConditions::UNATTACHED_SINCE_SPAWN)
    {
        return true;
    }
    let (Some(id), Some(instance)) = (terminal_id.local_id(), precondition.instance) else {
        return false;
    };
    state.with(|s| s.hub_vouches_unattached(host, id, instance))
}

/// Record the hub-side proxy attach a successful `ATTACH_RESOURCE` just
/// established, then send the relayed reply to the caller.
pub(super) async fn reply_satellite_command(
    ctx: &CommandContext,
    request_id: u32,
    host: &phux_protocol::ids::SatelliteHost,
    command: &Command,
    result: CommandResult,
) {
    let client_id = ctx.client_id;
    if !matches!(result, CommandResult::Error { .. })
        && let Command::AttachResource { terminal_id, .. } = command
        && let Some(id) = terminal_id.local_id()
    {
        ctx.state.with_mut(|s| {
            s.register_satellite_proxy_attach(client_id, host.clone(), id);
        });
    }
    debug!(
        ?client_id,
        request_id,
        satellite = %host,
        "satellite-routed COMMAND relayed; sending COMMAND_RESULT"
    );
    tokio::select! {
        biased;
        () = ctx.token.cancelled() => {}
        _ = ctx.out_tx.send(Outbound::Frame(FrameKind::CommandResult { request_id, result })) => {}
    }
}

/// Relay a stream-establishing command and register the caller's mailbox as
/// a hub-side proxy subscriber atomically with it.
pub(super) async fn relay_stream_establishing(
    ctx: &CommandContext,
    relay: &crate::hub::relay::RelayHandle,
    command: &Command,
    terminal_id: &phux_protocol::ids::ResourceId,
    resource_kind: Option<phux_protocol::ids::ResourceKind>,
) -> CommandResult {
    let Some(terminal) = terminal_id.local_id() else {
        return relay.command(command.clone()).await;
    };
    let consumer_cancel = ctx
        .state
        .with(|s| s.client_connection_cancellation(ctx.client_id));
    let Some(consumer_cancel) = consumer_cancel else {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "client connection cancellation is unavailable".to_owned(),
        };
    };
    // Only ATTACH_RESOURCE opens a content stream with a snapshot to await.
    let content = matches!(command, Command::AttachResource { .. });
    relay
        .command_subscribing(
            command.clone(),
            crate::hub::relay::ProxySubscription {
                terminal,
                client: ctx.client_id,
                out_tx: ctx.out_tx.clone(),
                consumer_cancel,
                // Stamped by `command_subscribing` at enqueue.
                seq: 0,
                awaits_snapshot: content,
                resource_kind,
                bootstrap_profile: content.then_some(ctx.profile),
                bootstrap_limits: content.then_some(ctx.limits),
            },
        )
        .await
}

/// Hub-side resolution of `DETACH_RESOURCE`: withdraw this consumer's proxy
/// subscription; the link session emits the satellite-side `DETACH_RESOURCE`
/// iff nobody else still observes the terminal. Success waits for the link's
/// removal receipt; no registry on a disconnected link is an idempotent Ok.
pub(super) async fn resolve_hub_detach_terminal(
    state: &SharedState,
    relay: &crate::hub::relay::RelayHandle,
    client_id: ClientId,
    host: &phux_protocol::ids::SatelliteHost,
    terminal_id: &phux_protocol::ids::ResourceId,
) -> CommandResult {
    let Some(id) = terminal_id.local_id() else {
        return CommandResult::Ok;
    };
    let result = relay.unsubscribe_terminal(client_id, id).await;
    if result == CommandResult::Ok {
        state.with_mut(|s| {
            s.unregister_satellite_proxy_attach(client_id, host, id);
            let scope = phux_protocol::ids::ResourceId::Satellite {
                host: host.clone(),
                id,
            };
            s.unsubscribe_terminal_events(client_id, &scope);
        });
    }
    result
}

/// The command the link forwards for a hub consumer's
/// `SUBSCRIBE_RESOURCE_EVENTS`: unfiltered, because the link is one client on
/// the satellite; each consumer's filter is applied by the hub (ADR-0123).
fn unfiltered_for_link(command: &Command) -> Command {
    match command {
        Command::SubscribeResourceEvents { terminal_id, .. } => Command::SubscribeResourceEvents {
            terminal_id: terminal_id.clone(),
            event_types: Vec::new(),
        },
        other => other.clone(),
    }
}

/// Finish a relayed `SUBSCRIBE_RESOURCE_EVENTS`: start the event pump on
/// success, or restore the scope [`register_satellite_event_filter`] changed.
fn settle_satellite_event_filter(
    state: &SharedState,
    client_id: ClientId,
    change: Option<crate::state::SatelliteScopeChange>,
    result: &CommandResult,
) {
    let Some(change) = change else {
        return;
    };
    if matches!(result, CommandResult::Error { .. }) {
        state.with_mut(|s| s.restore_satellite_scope(client_id, change));
        return;
    }
    super::super::client::ensure_event_pump(state, client_id);
}

/// Install the hub registry scope, with the consumer's filter, that a relayed
/// `SUBSCRIBE_RESOURCE_EVENTS` delivers through (ADR-0123). Returns the prior
/// scope for rollback; `None` for any other command.
fn register_satellite_event_filter(
    state: &SharedState,
    client_id: ClientId,
    host: &phux_protocol::ids::SatelliteHost,
    command: &Command,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> Option<crate::state::SatelliteScopeChange> {
    let Command::SubscribeResourceEvents {
        terminal_id,
        event_types,
    } = command
    else {
        return None;
    };
    let id = terminal_id.local_id()?;
    let scope = phux_protocol::ids::ResourceId::Satellite {
        host: host.clone(),
        id,
    };
    let filter = crate::state::EventFilter::of(event_types.clone());
    Some(
        state.with_mut(|s| {
            s.subscribe_satellite_events(client_id, scope, filter, None, out_tx.clone())
        }),
    )
}
