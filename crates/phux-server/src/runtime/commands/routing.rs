//! Command context, entry point, and routing ahead of local dispatch.

use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities};
use phux_protocol::wire::frame::Command;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::runtime::commands::attach_terminal::{invalid_agent_cursor, refuse_invalid_role_policy};
use crate::runtime::commands::dispatch::dispatch_local;
use crate::runtime::commands::lifecycle::{handle_shutdown, handle_upgrade};
use crate::runtime::commands::satellite::{handle_satellite_command, note_local_use};
use crate::runtime::reply;
use crate::state::{ClientId, Outbound, SharedState};

/// Stable, payload-free label for a [`Command`] variant: the `kind` field on
/// the `handle_command` span, kept free of session names, env, and input.
pub(crate) const fn command_kind(command: &Command) -> &'static str {
    match command {
        Command::AttachResource { .. } => "attach_terminal",
        Command::DetachResource { .. } => "detach_terminal",
        Command::KillResource { .. } => "kill_terminal",
        Command::KillResourceIf { .. } => "kill_resource_if",
        Command::OpenListener { .. } => "open_listener",
        Command::KillResources { .. } => "kill_terminals",
        Command::CloseTabResources { .. } => "close_tab_resources",
        Command::DetachClients { .. } => "detach_clients",
        Command::GetState { .. } => "get_state",
        Command::GetScreen { .. } => "get_screen",
        Command::RouteInput { .. } => "route_input",
        Command::ApplyInput { .. } => "apply_input",
        Command::AcquireInput { .. } => "acquire_input",
        Command::ReleaseInput { .. } => "release_input",
        Command::SignalTerminal { .. } => "signal_terminal",
        Command::PutFile { .. } => "put_file",
        Command::GetFile { .. } => "get_file",
        Command::Checkout { .. } => "checkout",
        Command::GetPerf { .. } => "get_perf",
        Command::PerfControl { .. } => "perf_control",
        Command::OpenUrl { .. } => "open_url",
        Command::Transcribe { .. } => "transcribe",
        Command::AppendResourceOutput { .. } => "append_resource_output",
        Command::Mirror { .. } => "mirror",
        Command::MirrorProvide { .. } => "mirror_provide",
        _ => "other",
    }
}

/// The satellite a host-named command (`GET_FILE`, `MIRROR`) is for, taken
/// out so the satellite reads it as its own.
pub(crate) const fn take_named_host(command: &mut Command) -> Option<String> {
    match command {
        Command::GetFile { host, .. } | Command::Mirror { host, .. } => host.take(),
        _ => None,
    }
}

/// One connection's negotiated dispatch context: everything a command needs
/// to run as that connection's, now or later (a held command, ADR-0128).
pub(crate) struct CommandContext {
    pub(crate) state: SharedState,
    pub(crate) client_id: ClientId,
    pub(crate) out_tx: tokio::sync::mpsc::Sender<Outbound>,
    pub(crate) client_caps: ClientCapabilities,
    /// The Terminal bootstrap profile, or `None` for a connection that
    /// offered none; a Terminal stream it binds is refused.
    pub(crate) profile: Option<BootstrapProfile>,
    pub(crate) limits: BootstrapLimits,
    pub(crate) input_lane: Option<crate::runtime::apps::InputLane>,
    /// This connection's cancellation.
    pub(crate) token: CancellationToken,
    pub(crate) root_token: CancellationToken,
    /// QUIC multi-stream (docs/protocol.md#quic-stream-routing): subscribe now, bootstrap when the
    /// client binds a stream (`STREAM_BIND`).
    pub(crate) defer_subscription: bool,
}

#[cfg(test)]
impl CommandContext {
    /// `client_id`'s context on a fresh Unix-socket connection.
    pub(in crate::runtime) fn for_test(
        state: &SharedState,
        client_id: ClientId,
        out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    ) -> Self {
        Self {
            state: state.clone(),
            client_id,
            out_tx: out_tx.clone(),
            client_caps: ClientCapabilities::default(),
            profile: Some(BootstrapProfile::SynthesizedVtRaw),
            limits: BootstrapLimits::default(),
            input_lane: None,
            token: CancellationToken::new(),
            root_token: CancellationToken::new(),
            defer_subscription: false,
        }
    }
}

/// Dispatch a `COMMAND` and reply with its correlated `COMMAND_RESULT`
///. The result may follow frames the command triggered.
// `debug`: the span's close timing would otherwise log a line per command at
// the default level (`RUST_LOG=phux=debug` shows it).
#[tracing::instrument(
    level = "debug",
    name = "handle_command",
    skip_all,
    fields(client_id = ?ctx.client_id, request_id, kind = command_kind(&command)),
)]
pub(in crate::runtime) async fn handle_command(
    ctx: &CommandContext,
    request_id: u32,
    command: Command,
) {
    let Some((command, claim)) = route_command(ctx, request_id, command).await else {
        return;
    };
    let result = dispatch_local(ctx, command, claim.as_ref()).await;
    super::super::keyed_ops::settle(claim, &result);
    debug!(
        client_id = ?ctx.client_id,
        request_id, "COMMAND dispatched; sending COMMAND_RESULT"
    );
    reply::send(&ctx.out_tx, request_id, result).await;
}

/// Whether `terminal_id` names a local resource whose app cannot resume an
/// agent stream from a cursor, so an `agent_after_seq` attach is refused.
fn refuses_agent_cursor(
    s: &crate::state::ServerState,
    terminal_id: &phux_protocol::ids::ResourceId,
) -> bool {
    s.idspace
        .resource_from_wire(terminal_id)
        .and_then(|core| s.resources().handle(core))
        .is_some_and(|handle| {
            phux_protocol::kinds::kind_spec(crate::id_bridge::wire_kind(handle.kind))
                .is_none_or(|spec| spec.stream.resume.is_none())
        })
}

/// Everything that answers a command before local dispatch: the
/// process-level verbs, organization commands, refusals, satellite routing,
/// and keyed-command admission. `None` when the command has been answered;
/// otherwise the command to run here and its keyed claim, if any.
async fn route_command(
    ctx: &CommandContext,
    request_id: u32,
    command: Command,
) -> Option<(
    Command,
    Option<super::super::operation_dedupe::OperationClaim>,
)> {
    let CommandContext {
        state,
        client_id,
        out_tx,
        root_token,
        ..
    } = ctx;
    let client_id = *client_id;
    // UPGRADE and SHUTDOWN ack the client themselves and then re-exec or end
    // the process, so neither reaches the shared result send below.
    if matches!(command, Command::Upgrade) {
        handle_upgrade(state, request_id, out_tx).await;
        return None;
    }
    if matches!(command, Command::Shutdown) {
        handle_shutdown(state, client_id, request_id, out_tx, root_token).await;
        return None;
    }
    // ADR-0161: organization commands commit through the org store and
    // answer with their own result shapes; they never route to a satellite
    // or take the keyed-command record (core keeps their receipts).
    if command.is_organization() {
        if let Command::ResourceRelaunch {
            operation,
            resource,
            expected,
        } = &command
            && let Some(result) = crate::runtime::commands::native_agent::claim_relaunch(
                ctx, *operation, resource, *expected,
            )
        {
            reply::send(out_tx, request_id, result).await;
            return None;
        }
        let result = super::super::organization::handle(state, client_id, command, root_token);
        reply::send(out_tx, request_id, result).await;
        return None;
    }

    // GET_FILE and MIRROR name their host by name, not through a Terminal
    // id: a named satellite reads its own filesystem or screens, and the
    // link refuses one that does not advertise the method.
    let mut command = command;
    if let Some(host) = take_named_host(&mut command) {
        let host = phux_protocol::ids::SatelliteHost::new(host);
        handle_satellite_command(ctx, request_id, &host, command).await;
        return None;
    }

    // PUT_FILE chunks may carry 8 MiB: rewrite a satellite id in place rather
    // than cloning the payload through the generic router below.
    if let Command::PutFile { terminal_id, .. } = &mut command
        && let Some((sat_host, local_id)) = crate::hub::relay::satellite_route(terminal_id)
    {
        *terminal_id = phux_protocol::ids::ResourceId::local(local_id);
        handle_satellite_command(ctx, request_id, &sat_host, command).await;
        return None;
    }

    // ADR-0127: `{ VIEWER, DELIBERATE }` is refused before anything is
    // subscribed or relayed.
    if let Some(result) = refuse_invalid_role_policy(&command) {
        reply::send(out_tx, request_id, result).await;
        return None;
    }
    if let Command::AttachResource {
        terminal_id,
        agent_after_seq: Some(_),
        ..
    } = &command
        && state.with(|s| refuses_agent_cursor(s, terminal_id))
    {
        reply::send(out_tx, request_id, invalid_agent_cursor()).await;
        return None;
    }

    // ADR-0109: a verb that drives or reads a resource another connection
    // spawned counts as attaching it; noted before the verb runs.
    note_local_use(state, client_id, &command);

    // Only the satellite knows a document's control class: stamp the
    // classes this hub's guard lets the caller control there, so it refuses
    // a document of any other (`docs/resource-kinds.md#authority`).
    // It also names who it relays for, so the satellite's control indicator
    // and audit log show the device, not this hub (`docs/authority.md`).
    if let Command::SendDocumentInput {
        terminal_id,
        control,
        on_behalf_of,
        ..
    } = &mut command
        && let Some((host, _)) = crate::hub::relay::satellite_route(terminal_id)
    {
        let (allowed, relayed) = state.with(|s| {
            (
                crate::policy::filter::satellite_input_classes(s, client_id, host.as_str()),
                s.relayed_for(client_id),
            )
        });
        if let Some(allowed) = allowed {
            *control = Some(control.map_or(allowed, |asked| asked.intersect(allowed)));
        }
        *on_behalf_of = Some(relayed);
    }

    // ADR-0007 §4: a satellite-owned target never touches local dispatch.
    if let Some((sat_host, local_command)) = crate::hub::relay::route_to_satellite(&command) {
        handle_satellite_command(ctx, request_id, &sat_host, local_command).await;
        return None;
    }

    // Keyed commands are admitted after routing; a satellite owns
    // the dedupe of what it runs.
    let claim = match super::super::keyed_ops::admit(state, &command).await {
        super::super::keyed_ops::KeyedAdmission::Unkeyed => None,
        super::super::keyed_ops::KeyedAdmission::Owner(claim) => Some(claim),
        super::super::keyed_ops::KeyedAdmission::Answer(result) => {
            reply::send(out_tx, request_id, result).await;
            return None;
        }
    };
    Some((command, claim))
}
