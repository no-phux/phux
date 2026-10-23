//! Dispatch: extracted from the shared module.
use super::{
    Command, CommandContext, CommandResult, ErrorCode, FrameKind, Outbound, ResourceId, RolePolicy,
    debug, handle_acquire_input, handle_attach_terminal, handle_close_tab_resources,
    handle_detach_clients, handle_detach_terminal, handle_get_perf, handle_get_screen,
    handle_get_state_federated, handle_get_terminal_state, handle_kill_resource_if,
    handle_kill_terminal, handle_kill_terminals, handle_release_input, handle_report_agent_state,
    handle_report_asked, handle_route_input, handle_satellite_command, handle_shutdown,
    handle_signal_terminal, handle_subscribe_terminal_events, handle_upgrade, invalid_agent_cursor,
    kill_attribution, note_local_use, refuse_invalid_role_policy, subscribe_deferred_attach,
};

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
        Command::Transcribe { .. } => "transcribe",
        Command::AppendResourceOutput { .. } => "append_resource_output",
        _ => "other",
    }
}

/// Queue `result` as the `COMMAND_RESULT` for `request_id`; a closed mailbox
/// means the connection is already gone.
pub(super) async fn send_result(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    request_id: u32,
    result: CommandResult,
) {
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::CommandResult {
            request_id,
            result,
        }))
        .await;
}

/// Dispatch a `COMMAND` and reply with its correlated `COMMAND_RESULT`
/// (SPEC §5). The result may follow frames the command triggered.
#[tracing::instrument(
    level = "info",
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
    send_result(&ctx.out_tx, request_id, result).await;
}

/// Everything that answers a command before local dispatch: the
/// process-level verbs, organization commands, refusals, satellite routing,
/// and keyed-command admission. `None` when the command has been answered;
/// otherwise the command to run here and its keyed claim, if any.
pub(super) async fn route_command(
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
        {
            match state
                .with(|s| super::super::organization::native_relaunch_spec(s, resource, *expected))
            {
                Ok(Some(spec)) => {
                    let result = super::super::native_agent::spawn_relaunch(ctx, spec, *operation);
                    send_result(out_tx, request_id, result).await;
                    return None;
                }
                Err(result) => {
                    send_result(out_tx, request_id, result).await;
                    return None;
                }
                Ok(None) => {}
            }
        }
        let result = super::super::organization::handle(state, client_id, command, root_token);
        send_result(out_tx, request_id, result).await;
        return None;
    }

    // GET_FILE names its host by name, not through a Terminal id: a named
    // satellite reads its own filesystem, and the link refuses one that
    // does not advertise FILE_DOWNLOAD.
    let mut command = command;
    if let Command::GetFile { host, .. } = &mut command
        && let Some(host) = host.take()
    {
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
        send_result(out_tx, request_id, result).await;
        return None;
    }
    if let Command::AttachResource {
        terminal_id,
        agent_after_seq: Some(_),
        ..
    } = &command
        && state.with(|s| {
            s.terminal_from_wire(terminal_id)
                .and_then(|core| s.resource_handle(core))
                .is_some_and(|handle| handle.kind == crate::resource::ResourceKind::Terminal)
        })
    {
        send_result(out_tx, request_id, invalid_agent_cursor()).await;
        return None;
    }

    // ADR-0109: a verb that drives or reads a resource another connection
    // spawned counts as attaching it; noted before the verb runs.
    note_local_use(state, client_id, &command);

    // ADR-0007 §4: a satellite-owned target never touches local dispatch.
    if let Some((sat_host, local_command)) = crate::hub::relay::route_to_satellite(&command) {
        handle_satellite_command(ctx, request_id, &sat_host, local_command).await;
        return None;
    }

    // L1 §5.1.1: keyed commands are admitted after routing; a satellite owns
    // the dedupe of what it runs.
    let claim = match super::super::keyed_ops::admit(state, &command).await {
        super::super::keyed_ops::KeyedAdmission::Unkeyed => None,
        super::super::keyed_ops::KeyedAdmission::Owner(claim) => Some(claim),
        super::super::keyed_ops::KeyedAdmission::Answer(result) => {
            send_result(out_tx, request_id, result).await;
            return None;
        }
    };
    Some((command, claim))
}

/// Run a routed command on this server and return its result.
#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "one flat dispatch arm per wire command keeps the catalog auditable"
)]
pub(super) async fn dispatch_local(
    ctx: &CommandContext,
    command: Command,
    claim: Option<&super::super::operation_dedupe::OperationClaim>,
) -> CommandResult {
    let CommandContext {
        state,
        client_id,
        out_tx,
        root_token,
        ..
    } = ctx;
    let client_id = *client_id;
    let input_lane = ctx.input_lane.as_ref();

    match command {
        Command::NativeAgent { request } => {
            super::super::native_agent::handle(ctx, request.into_request()).await
        }
        Command::AttachResource {
            terminal_id,
            role_policy,
            agent_after_seq,
        } => {
            dispatch_attach_resource(
                ctx,
                &terminal_id,
                role_policy.unwrap_or_default(),
                agent_after_seq,
            )
            .await
        }
        Command::DetachResource { terminal_id } => {
            handle_detach_terminal(state, client_id, &terminal_id).await
        }
        Command::GetState { scope } => {
            handle_get_state_federated(state, client_id, &scope, out_tx).await
        }
        Command::GetPerf { reset } => handle_get_perf(state, reset),
        Command::DescribeApi => super::super::api::describe(state),
        Command::PerfControl { op } => super::super::perf_control::handle(op).await,
        Command::Transcribe {
            upload_id,
            terminal_id,
        } => {
            super::super::voice::handle_transcribe(
                state,
                client_id,
                upload_id,
                &terminal_id,
                input_lane,
            )
            .await
        }
        Command::GetScreen {
            terminal_id,
            request_scrollback,
            cells,
            format,
        } => handle_get_screen(state, &terminal_id, request_scrollback, cells, format).await,
        Command::RouteInput { terminal_id, event } => match input_lane {
            // A held `ROUTE_INPUT` waits for a pane input credit exactly as a
            // direct one does (ADR-0144).
            Some(lane) => match super::super::input_lane::acquire_credit(state, &terminal_id).await
            {
                Ok(credit) => {
                    lane.begin_route(state, client_id, terminal_id, event, credit)
                        .await
                }
                Err(super::super::input_lane::InputStalled) => {
                    super::super::input_lane::InputStalled::result(&terminal_id)
                }
            },
            None => handle_route_input(state, client_id, &terminal_id, event),
        },
        Command::ApplyInput {
            operation_id,
            terminal_id,
            events,
        } => match input_lane {
            Some(lane) => {
                lane.begin_apply(state, client_id, operation_id, terminal_id, events)
                    .await
            }
            None => CommandResult::Error {
                code: ErrorCode::InternalError,
                message: "acknowledged input lane unavailable".to_owned(),
            },
        },
        Command::KillResources { ids, operation_id } => {
            handle_kill_terminals(state, client_id, &ids, operation_id).await
        }
        Command::CloseTabResources { ids } => handle_close_tab_resources(state, &ids),
        Command::DetachClients { session } => handle_detach_clients(state, session.as_deref()),
        Command::KillResource {
            terminal_id,
            operation_id,
        } => handle_kill_terminal(
            state,
            &terminal_id,
            kill_attribution(client_id, operation_id),
        ),
        Command::KillResourceIf {
            terminal_id,
            precondition,
            operation_id,
        } => handle_kill_resource_if(
            state,
            &terminal_id,
            &precondition,
            kill_attribution(client_id, operation_id),
        ),
        Command::OpenListener {
            transport,
            port_range,
            linger_secs,
        } => super::super::ephemeral_listener::handle_open_listener(
            state,
            client_id,
            super::super::ephemeral_listener::OpenRequest {
                transport,
                port_range,
                linger_secs,
            },
            input_lane,
            root_token,
        ),
        Command::DelegateAgent {
            operation,
            session,
            ttl_secs,
        } => super::super::delegation::handle_delegate_agent(
            state, client_id, operation, &session, ttl_secs, input_lane, root_token,
        ),
        Command::RevokeDelegation { credential_id, .. } => {
            super::super::delegation::handle_revoke_delegation(state, &credential_id)
        }
        Command::GetTerminalState {
            terminal_id,
            include_scrollback,
            max_scrollback_lines,
        } => {
            handle_get_terminal_state(
                state,
                &terminal_id,
                include_scrollback,
                max_scrollback_lines,
            )
            .await
        }
        Command::SubscribeResourceEvents {
            terminal_id,
            event_types,
        } => handle_subscribe_terminal_events(state, client_id, &terminal_id, event_types, out_tx),
        Command::AcquireInput {
            terminal_id,
            mode,
            ttl_ms,
        } => handle_acquire_input(state, client_id, &terminal_id, mode, ttl_ms).await,
        Command::ReleaseInput { terminal_id } => {
            handle_release_input(state, client_id, &terminal_id).await
        }
        Command::SignalTerminal {
            terminal_id,
            signal,
            operation_id,
        } => {
            handle_signal_terminal(state, client_id, &terminal_id, signal, operation_id, claim)
                .await
        }
        Command::PutFile {
            upload_id,
            terminal_id,
            extension,
            offset,
            data,
            final_chunk,
            sha256,
        } => {
            super::super::upload::handle_put_file(
                state,
                super::super::upload::PutFileChunk {
                    upload_id,
                    terminal_id,
                    extension,
                    offset,
                    data,
                    final_chunk,
                    sha256,
                    principal: state.with(|s| super::super::upload::upload_principal(s, client_id)),
                },
            )
            .await
        }
        Command::Checkout {
            terminal_id,
            request,
        } => super::super::checkout::handle(state, &terminal_id, request).await,
        Command::GetFile {
            host: _,
            path,
            offset,
            max_bytes,
        } => {
            super::super::download::handle_get_file(super::super::download::GetFileRequest {
                path,
                offset,
                max_bytes,
            })
            .await
        }
        Command::ReportAsked {
            terminal_id,
            id,
            question,
            suggestions,
            elapsed_seconds,
        } => handle_report_asked(
            state,
            &terminal_id,
            id,
            question,
            suggestions,
            elapsed_seconds,
        ),
        Command::ReportAgentState {
            terminal_id,
            state: reported,
        } => handle_report_agent_state(state, &terminal_id, reported).await,
        Command::AppendResourceOutput { terminal_id, bytes } => {
            crate::runtime::resource_commands::handle_append_resource_output(
                state,
                client_id,
                &terminal_id,
                bytes::Bytes::from(bytes),
            )
            .await
        }
        // A known-but-unwired tag of the `#[non_exhaustive]` catalog (SPEC §5).
        _ => CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message: "command not supported by this server".to_owned(),
        },
    }
}

pub(super) async fn dispatch_attach_resource(
    ctx: &CommandContext,
    terminal_id: &ResourceId,
    role: RolePolicy,
    agent_after_seq: Option<u64>,
) -> CommandResult {
    // AgentSession streams retain their replay cursor; only Terminals defer
    // content subscription until STREAM_BIND. Kind resolution precedes routing.
    let is_terminal = ctx.state.with(|s| {
        s.terminal_from_wire(terminal_id)
            .and_then(|core| s.resource_handle(core))
            .is_some_and(|handle| handle.kind == crate::resource::ResourceKind::Terminal)
    });
    if ctx.defer_subscription && is_terminal {
        subscribe_deferred_attach(&ctx.state, ctx.client_id, terminal_id, &ctx.out_tx, role).await
    } else {
        handle_attach_terminal(ctx, terminal_id, role, agent_after_seq).await
    }
}
