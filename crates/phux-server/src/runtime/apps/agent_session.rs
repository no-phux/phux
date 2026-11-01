//! The `AgentSession` app: a producer-fed record stream.

use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind};

use super::{ResourceApp, StreamBootstrap, StreamFrameContext};
use crate::resource::ResourceKind;
use crate::runtime::commands::{AttachResourceFailure, CommandContext};
use crate::runtime::connections::dispatch::FrameFlow;
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

    fn check_takeover(&self) -> Result<(), crate::resource::WrongResourceKind> {
        // The input lease is a Terminal-facet concept (L1 §1.1).
        Err(crate::resource::WrongResourceKind {
            required: ResourceKind::Terminal,
            actual: ResourceKind::AgentSession,
        })
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

    fn stream_frame<'a>(
        &'a self,
        ctx: StreamFrameContext<'a>,
        frame: FrameKind,
    ) -> LocalBoxFuture<'a, FrameFlow> {
        let Some((code, message)) = stream_refusal(&frame) else {
            // Input and RESIZE_TERMINAL keep the Terminal path, whose
            // attachment checks precede its WRONG_RESOURCE_KIND refusal.
            return super::terminal::TerminalApp.stream_frame(ctx, frame);
        };
        async move {
            crate::runtime::send_error(&ctx.plumbing.out_tx, code, message).await;
            FrameFlow::Continue(())
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

/// ADR-0103 §4: an agent-session stream is raw-profile with no history
/// beyond its bootstrap. Both refusals keep the connection.
const fn stream_refusal(frame: &FrameKind) -> Option<(ErrorCode, &'static str)> {
    match frame {
        FrameKind::FrameAck { .. } => Some((
            ErrorCode::MalformedMessage,
            "FRAME_ACK is not valid on an agent-session stream",
        )),
        FrameKind::HistoryRequest { .. } => Some((
            ErrorCode::WrongResourceKind,
            "an agent-session stream retains no history beyond its bootstrap",
        )),
        _ => None,
    }
}
