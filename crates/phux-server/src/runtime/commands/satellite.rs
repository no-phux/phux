//! Satellite command relay.

use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind};
use tracing::debug;

use crate::runtime::commands::routing::CommandContext;
use crate::runtime::commands::satellite_lease::{
    SatelliteLeaseTarget, relay_satellite_acquire_input, relay_satellite_attach,
    relay_satellite_release_input, relay_satellite_route_input,
};
use crate::state::{ClientId, Outbound, SharedState};

/// Relay one satellite-targeted command (already rewritten to `Local` ids)
/// over the hub link and send the correlated `COMMAND_RESULT` (ADR-0007 §4).
/// Stream-establishing commands register a hub-side proxy subscriber with
/// the relay; `DETACH_RESOURCE` resolves hub-side. Every hub consumer is one
/// identity on the satellite, so the hub owns lease exclusion.
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
    let relay = state.with_mut(|s| {
        let owner = s.federation_mut().consumer_registration(client_id);
        s.federation()
            .relay(host)
            .map(|relay| relay.for_consumer(owner))
    });
    if let Some(relay) = &relay
        && let Some(refusal) = refuse_satellite_watch(state, client_id, host, relay, &command).await
    {
        reply_satellite_command(ctx, request_id, host, refusal).await;
        return;
    }
    let result = match relay {
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
                resolve_hub_detach_terminal(&relay, client_id, terminal_id).await
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
            // `APPLY_INPUT` crosses the same lease gate as
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
    reply_satellite_command(ctx, request_id, host, result).await;
}

/// Refuse a watch of a satellite window or display the caller's grant
/// names no class grant for (`docs/authority.md`). The guard admitted the
/// relay on the host grant; only the satellite's inventory says what the
/// document shows, so the hub judges it here, per consumer, before the
/// shared link carries anything.
async fn refuse_satellite_watch(
    state: &SharedState,
    client_id: ClientId,
    host: &phux_protocol::ids::SatelliteHost,
    relay: &crate::hub::relay::RelayHandle,
    command: &Command,
) -> Option<CommandResult> {
    use phux_protocol::wire::frame::ControlClass;

    use crate::policy::filter::admits_satellite_watch;
    let (Command::AttachResource { terminal_id, .. }
    | Command::SubscribeResourceEvents { terminal_id, .. }
    | Command::GetScreen { terminal_id, .. }) = command
    else {
        return None;
    };
    let id = terminal_id.local_id()?;
    // Free for the owner and for a grant that watches every class there.
    let watches_all = state.with(|s| {
        ControlClass::ALL
            .into_iter()
            .all(|class| admits_satellite_watch(s, client_id, host.as_str(), Some(class)))
    });
    if watches_all {
        return None;
    }
    let class = match relay.resource_control(id).await {
        Ok(class) => class,
        Err(refusal) => return Some(refusal),
    };
    state.with(|s| {
        (!admits_satellite_watch(s, client_id, host.as_str(), class)).then(|| {
            CommandResult::Error {
                code: ErrorCode::PermissionDenied,
                message: crate::policy::filter::watch_refusal(
                    s,
                    client_id,
                    class.unwrap_or(ControlClass::Display),
                    Some(host.as_str()),
                ),
            }
        })
    })
}

/// ADR-0109: the resource `command` attaches, drives, or reads,
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
    state.with_mut(|s| s.federation_mut().note_satellite_use(host, id, client_id));
}

/// Relay a `KILL_RESOURCE_IF` once the hub has checked the half of
/// `UNATTACHED_SINCE_SPAWN` only it can see (ADR-0109); the
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
        return CommandResult::error(
            ErrorCode::InternalError,
            "a conditional-kill relay was handed another command",
        );
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
    state.with(|s| s.federation().vouches_unattached(host, id, instance))
}

/// Send the correlated reply; proxy ownership is committed by the link,
/// not by a potentially delayed consumer continuation.
pub(super) async fn reply_satellite_command(
    ctx: &CommandContext,
    request_id: u32,
    host: &phux_protocol::ids::SatelliteHost,
    result: CommandResult,
) {
    let client_id = ctx.client_id;
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
        return CommandResult::error(
            ErrorCode::InternalError,
            "client connection cancellation is unavailable",
        );
    };
    // Only ATTACH_RESOURCE opens a content stream with a snapshot to await.
    let owner = ctx
        .state
        .with_mut(|s| s.federation_mut().consumer_registration(ctx.client_id));
    let relay = relay.for_consumer(owner);
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
                bootstrap_profile: content.then_some(ctx.profile).flatten(),
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
    relay: &crate::hub::relay::RelayHandle,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
) -> CommandResult {
    let Some(id) = terminal_id.local_id() else {
        return CommandResult::Ok;
    };
    relay.unsubscribe_resource(client_id, id).await
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
    crate::runtime::connections::ensure_event_pump(state, client_id);
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
