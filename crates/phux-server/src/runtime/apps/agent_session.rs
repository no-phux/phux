//! The `AgentSession` app: a producer-fed record stream.

use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind};

use super::{ResourceApp, ResourceFrame, StreamBootstrap};
use crate::resource::{ResourceKind, WrongResourceKind};
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

    fn check_takeover(&self) -> Result<(), WrongResourceKind> {
        // The input lease is a Terminal-facet concept (L1 §1.1).
        Err(WrongResourceKind {
            required: ResourceKind::Terminal,
            actual: ResourceKind::AgentSession,
        })
    }

    /// An agent session serves no stream or input frame: it is raw-profile
    /// with no history beyond its bootstrap (ADR-0103 §4), and input and
    /// geometry are the Terminal facet's. Every refusal keeps the connection.
    fn frame<'a>(
        &'a self,
        ctx: ResourceFrame<'a>,
        frame: FrameKind,
    ) -> LocalBoxFuture<'a, FrameFlow> {
        async move {
            let (code, message) = match frame {
                FrameKind::FrameAck { .. } => (
                    ErrorCode::MalformedMessage,
                    "FRAME_ACK is not valid on an agent-session stream".to_owned(),
                ),
                FrameKind::HistoryRequest { .. } => (
                    ErrorCode::WrongResourceKind,
                    "an agent-session stream retains no history beyond its bootstrap".to_owned(),
                ),
                _ => (
                    ErrorCode::WrongResourceKind,
                    WrongResourceKind {
                        required: ResourceKind::Terminal,
                        actual: ResourceKind::AgentSession,
                    }
                    .to_string(),
                ),
            };
            crate::runtime::send_error(&ctx.plumbing.out_tx, code, &message).await;
            FrameFlow::Continue(())
        }
        .boxed_local()
    }

    fn metrics(&self) -> &'static [phux_perf::Metric] {
        &[]
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
