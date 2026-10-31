//! Local command dispatch.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use super::*;
use crate::runtime::apps::ResourceApp;

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

    // Facet commands belong to the kind the catalog says owns them.
    if let Some(app) = crate::runtime::apps::app_owning(&command) {
        return app.command(ctx, command, claim).await;
    }

    match command {
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
        Command::SubscribeResourceEvents {
            terminal_id,
            event_types,
        } => handle_subscribe_terminal_events(state, client_id, &terminal_id, event_types, out_tx),
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
        // A known-but-unwired tag of the `#[non_exhaustive]` catalog (SPEC §5).
        _ => CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message: "command not supported by this server".to_owned(),
        },
    }
}

async fn dispatch_attach_resource(
    ctx: &CommandContext,
    terminal_id: &ResourceId,
    role: RolePolicy,
    agent_after_seq: Option<u64>,
) -> CommandResult {
    // A deferring kind's content subscription waits for STREAM_BIND. Kind
    // resolution precedes routing.
    let defers = ctx.state.with(|s| {
        s.terminal_from_wire(terminal_id)
            .and_then(|core| s.resource_handle(core))
            .and_then(|handle| crate::runtime::apps::app_for(handle.kind))
            .is_some_and(ResourceApp::defers_stream_bootstrap)
    });
    if ctx.defer_subscription && defers {
        subscribe_deferred_attach(&ctx.state, ctx.client_id, terminal_id, &ctx.out_tx, role).await
    } else {
        handle_attach_terminal(ctx, terminal_id, role, agent_after_seq).await
    }
}
