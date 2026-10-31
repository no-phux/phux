//! The `AgentSession` app: a producer-fed record stream.

use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode};

use super::{ResourceApp, StreamBootstrap};
use crate::resource::ResourceKind;
use crate::runtime::commands::{AttachResourceFailure, CommandContext};
use crate::runtime::operation_dedupe::OperationClaim;

/// Serves [`ResourceKind::AgentSession`].
#[derive(Debug)]
pub(crate) struct AgentSessionApp;

impl ResourceApp for AgentSessionApp {
    fn kind(&self) -> ResourceKind {
        ResourceKind::AgentSession
    }

    fn defers_stream_bootstrap(&self) -> bool {
        false
    }

    fn resumes_from_cursor(&self) -> bool {
        true
    }

    fn command<'a>(
        &'a self,
        ctx: &'a CommandContext,
        command: Command,
        _claim: Option<&'a OperationClaim>,
    ) -> LocalBoxFuture<'a, CommandResult> {
        async move {
            match command {
                Command::NativeAgent { request } => {
                    crate::runtime::native_agent::handle(ctx, request.into_request()).await
                }
                Command::AppendResourceOutput { terminal_id, bytes } => {
                    crate::runtime::resource_commands::handle_append_resource_output(
                        &ctx.state,
                        ctx.client_id,
                        &terminal_id,
                        bytes::Bytes::from(bytes),
                    )
                    .await
                }
                _ => CommandResult::Error {
                    code: ErrorCode::InvalidCommand,
                    message: "command not supported by this server".to_owned(),
                },
            }
        }
        .boxed_local()
    }

    fn bootstrap<'a>(
        &'a self,
        stream: StreamBootstrap<'a>,
    ) -> LocalBoxFuture<'a, Result<(), AttachResourceFailure>> {
        async move {
            match crate::runtime::resource_commands::attach_agent_session(
                stream.state,
                stream.client_id,
                stream.wire,
                stream.core,
                stream.handle,
                stream.content_tx,
                stream.limits,
                stream.connection_token,
                stream.after_seq,
            )
            .await
            {
                CommandResult::Ok | CommandResult::OkWith(_) => Ok(()),
                CommandResult::Error { code, message } => {
                    Err(AttachResourceFailure { code, message })
                }
                other => Err(AttachResourceFailure::internal(&format!(
                    "agent session attach returned an unexpected result: {other:?}"
                ))),
            }
        }
        .boxed_local()
    }
}
