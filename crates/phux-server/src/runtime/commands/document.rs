//! Host-owned producer authority, registration and resource evidence.
use super::CommandContext;
use crate::runtime::apps::document::{DocumentApi, engine};
use bytes::Bytes;
use engine::{DocumentResource as _, PublishRejection, PublishRequest};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode};
use tokio::sync::oneshot;
pub(crate) mod audit;
mod input;
mod mirror;

pub(super) async fn serve<A: DocumentApi>(ctx: &CommandContext, command: Command) -> CommandResult {
    match command {
        Command::PublishResourceState { terminal_id, bytes } => {
            publish::<A>(ctx, &terminal_id, bytes.into()).await
        }
        Command::SendDocumentInput {
            terminal_id,
            intent,
            control,
            on_behalf_of,
        } => {
            let audited = intent.clone();
            let relayed = on_behalf_of.as_ref();
            let result = input::relay::<A>(ctx, &terminal_id, intent, control, relayed).await;
            ctx.state.with(|s| {
                audit::record(s, ctx.client_id, relayed, &terminal_id, &audited, &result);
            });
            result
        }
        Command::Mirror { op, .. } => mirror::handle(&ctx.state, op).await,
        Command::MirrorProvide { targets } => mirror::provide(&ctx.state, &targets),
        _ => CommandResult::error(
            ErrorCode::InvalidCommand,
            "command not supported by this server",
        ),
    }
}
/// `PUBLISH_RESOURCE_STATE`: a local owner connection replaces the
/// document; the reply names the sequence the document carries. A publish
/// to a document whose producer is gone (it redialed after a server
/// upgrade) makes this connection its producer again, so viewers' input
/// reaches it.
async fn publish<A: DocumentApi>(
    ctx: &CommandContext,
    terminal_id: &phux_protocol::ids::ResourceId,
    bytes: Bytes,
) -> CommandResult {
    let (state, client_id) = (&ctx.state, ctx.client_id);
    if !state.with(|s| s.client_may_produce(client_id)) {
        return CommandResult::error(
            ErrorCode::NotProducer,
            "publishing a resource's state requires a local owner connection",
        );
    }
    let Some((core, handle)) = state.with(|s| {
        let core = s.idspace.resource_from_wire(terminal_id)?;
        Some((core, s.resources().handle(core)?.clone()))
    }) else {
        return CommandResult::error(
            ErrorCode::TerminalNotFound,
            format!("no such resource: {terminal_id}"),
        );
    };
    let document = match handle.document::<A>() {
        Ok(document) => document,
        Err(error) => return crate::runtime::commands::wrong_resource_kind(error),
    };
    let (reply, rx) = oneshot::channel();
    if document
        .publish
        .try_send(PublishRequest { bytes, reply })
        .is_err()
    {
        return CommandResult::error(
            ErrorCode::Overflow,
            format!("the {}'s publish queue is full", A::LABEL),
        );
    }
    match rx.await {
        Ok(Ok(published)) => {
            state.with_mut(|s| {
                if s.resource_producer(core).is_none() {
                    s.resources_mut().record_spawn(core, client_id);
                    s.subscribe_resource(client_id, core, Some(ctx.out_tx.clone()));
                }
            });
            CommandResult::OkWith(CommandValue::Json(format!(
                "{{\"seq\":{},\"ts_ms\":{}}}",
                published.seq, published.ts_ms
            )))
        }
        Ok(Err(PublishRejection::Invalid(error))) => {
            CommandResult::error(ErrorCode::RecordInvalid, error)
        }
        Ok(Err(PublishRejection::Overflow)) => CommandResult::error(
            ErrorCode::Overflow,
            format!("the {}'s sequence space is exhausted", A::LABEL),
        ),
        Err(_) => CommandResult::error(
            ErrorCode::TerminalNotFound,
            format!("the {} closed before the publish landed", A::LABEL),
        ),
    }
}
