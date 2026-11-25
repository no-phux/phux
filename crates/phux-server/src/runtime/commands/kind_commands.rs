//! Host command ownership: resolve exact resources before invoking kind engines.
use phux_core::ResourceKind;
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode};
use super::CommandContext;
use crate::runtime::operation_dedupe::OperationClaim;
use crate::runtime::apps::document::engine::{TreeDocument, SurfaceDocument, TypedDocument};

pub(super) fn kind(ctx: &CommandContext, command: &Command) -> Option<ResourceKind> {
    phux_protocol::kinds::command_served_by(command)
        .and_then(crate::id_bridge::core_kind)
        .or_else(|| match command {
            Command::PublishResourceState { terminal_id, .. }
            | Command::SendDocumentInput { terminal_id, .. } => ctx.state.with(|s| {
                let id = s.idspace.resource_from_wire(terminal_id)?;
                Some(s.resources().handle(id)?.kind)
            }),
            _ => None,
        })
}

pub(super) async fn dispatch(
    kind: ResourceKind,
    ctx: &CommandContext,
    command: Command,
    claim: Option<&OperationClaim>,
) -> CommandResult {
    match kind {
        ResourceKind::Terminal => super::terminal::serve(ctx, command, claim).await,
        ResourceKind::AgentSession => match command {
            Command::NativeAgent { request } => super::native_agent::handle(ctx, request.into_request()).await,
            Command::AppendResourceOutput { terminal_id, bytes } => {
                super::agent_append::handle_append_resource_output(&ctx.state, ctx.client_id, &terminal_id, bytes.into()).await
            }
            _ => CommandResult::error(ErrorCode::InvalidCommand, "command not supported by this server"),
        },
        ResourceKind::AccessibilityTree => super::document::serve::<TreeDocument>(ctx, command).await,
        ResourceKind::PixelSurface => super::document::serve::<SurfaceDocument>(ctx, command).await,
        ResourceKind::Document => super::document::serve::<TypedDocument>(ctx, command).await,
    }
}
