//! The `AgentSession` app: a producer-fed record stream.

use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use phux_protocol::ids::{OperationId, ResourceKey, Revision};
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode};

use super::{ResourceApp, ResourceSpawn, StreamBootstrap};
use crate::resource::ResourceKind;
use crate::runtime::commands::{AttachResourceFailure, CommandContext};
use crate::runtime::operation_dedupe::OperationClaim;
use crate::state::SharedState;

mod append;
mod attach;
mod native_agent;
pub(super) mod spawn;
pub(crate) mod upgrade;

/// Payload budget for one `BOOTSTRAP_CHUNK` when the connection negotiated
/// none. Records are at most 16 KiB, so this fits several per chunk.
const DEFAULT_AGENT_CHUNK_BYTES: usize = 64 * 1024;

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
                    native_agent::handle(ctx, request.into_request()).await
                }
                Command::AppendResourceOutput { terminal_id, bytes } => {
                    append::handle_append_resource_output(
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
            match attach::attach_agent_session(
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

    fn spawn<'a>(&'a self, spawn: ResourceSpawn<'a>) -> LocalBoxFuture<'a, ()> {
        spawn::spawn_agent_session(spawn).boxed_local()
    }

    fn resumed(&self, state: &SharedState) {
        native_agent::retry_pending_closures(state);
    }

    fn resource_exited<'a>(
        &'a self,
        state: &'a SharedState,
        resource: phux_core::ids::ResourceId,
    ) -> LocalBoxFuture<'a, ()> {
        native_agent::source_exited(state, resource).boxed_local()
    }

    fn claim_relaunch(
        &self,
        ctx: &CommandContext,
        operation: OperationId,
        resource: &ResourceKey,
        expected: Revision,
    ) -> Option<CommandResult> {
        native_agent::claim_relaunch(ctx, operation, resource, expected)
    }
}
