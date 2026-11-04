//! The `AccessibilityTree` app: a producer-fed, replicated document whose
//! newest version is what every viewer wants (`docs/resource-kinds.md`).

use bytes::Bytes;
use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use phux_protocol::ids::{OperationId, ResourceKey, Revision};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode, FrameKind};
use phux_tree::{PublishRejection, PublishRequest, TreeResource as _};
use tokio::sync::oneshot;

use super::{ResourceApp, ResourceFrame, ResourceSpawn, StreamBootstrap};
use crate::resource::{ResourceHandle, ResourceKind, WrongResourceKind};
use crate::runtime::commands::{AttachResourceFailure, CommandContext};
use crate::runtime::connections::dispatch::FrameFlow;
use crate::runtime::operation_dedupe::OperationClaim;
use crate::state::{ClientId, SharedState};

mod attach;
mod spawn;

pub(crate) use attach::attach_tree;
pub(crate) use spawn::spawn_tree;

/// Serves [`ResourceKind::AccessibilityTree`].
#[derive(Debug)]
pub(crate) struct AccessibilityTreeApp;

impl ResourceApp for AccessibilityTreeApp {
    fn defers_stream_bootstrap(&self) -> bool {
        false
    }

    fn resumes_from_cursor(&self) -> bool {
        false
    }

    fn check_takeover(&self) -> Result<(), WrongResourceKind> {
        Err(WrongResourceKind {
            required: ResourceKind::Terminal,
            actual: ResourceKind::AccessibilityTree,
        })
    }

    fn metrics(&self) -> &'static [phux_perf::Metric] {
        &[]
    }

    fn release_consumer(&self, _handle: &ResourceHandle, _client: ClientId) {
        // The engine keeps nothing per consumer: every viewer reads the one
        // newest document, and a client's pump is host-side.
    }

    fn fence_consumer<'a>(
        &'a self,
        _handle: &'a ResourceHandle,
        _client: ClientId,
    ) -> LocalBoxFuture<'a, ()> {
        // Nothing to fence: the engine emits only to the shared watch.
        Box::pin(std::future::ready(()))
    }

    fn resumed(&self, _state: &SharedState) {}

    fn resource_exited<'a>(
        &'a self,
        _state: &'a SharedState,
        _resource: phux_core::ids::ResourceId,
    ) -> LocalBoxFuture<'a, ()> {
        Box::pin(std::future::ready(()))
    }

    fn claim_relaunch(
        &self,
        _ctx: &CommandContext,
        _operation: OperationId,
        _resource: &ResourceKey,
        _expected: Revision,
    ) -> Option<CommandResult> {
        // A tree is never relaunched: its producer spawns a fresh one.
        None
    }

    fn spawn<'a>(&'a self, spawn: ResourceSpawn<'a>) -> LocalBoxFuture<'a, ()> {
        spawn_tree(spawn).boxed_local()
    }

    fn command<'a>(
        &'a self,
        ctx: &'a CommandContext,
        command: Command,
        _claim: Option<&'a OperationClaim>,
    ) -> LocalBoxFuture<'a, CommandResult> {
        async move {
            match command {
                Command::PublishResourceState { terminal_id, bytes } => {
                    publish(&ctx.state, ctx.client_id, &terminal_id, Bytes::from(bytes)).await
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
            match attach_tree(
                stream.state,
                stream.client_id,
                stream.wire,
                stream.core,
                stream.handle,
                stream.content_tx,
                stream.limits,
                stream.connection_token,
            )
            .await
            {
                CommandResult::Ok | CommandResult::OkWith(_) => Ok(()),
                CommandResult::Error { code, message } => {
                    Err(AttachResourceFailure { code, message })
                }
                other => Err(AttachResourceFailure::internal(&format!(
                    "accessibility tree attach returned an unexpected result: {other:?}"
                ))),
            }
        }
        .boxed_local()
    }

    /// A tree serves no stream or input frame: it has no history, no grid,
    /// and takes no input. Every refusal keeps the connection.
    fn frame<'a>(
        &'a self,
        ctx: ResourceFrame<'a>,
        frame: FrameKind,
    ) -> LocalBoxFuture<'a, FrameFlow> {
        async move {
            let message = match frame {
                FrameKind::FrameAck { .. } => {
                    "FRAME_ACK is not valid on an accessibility-tree stream".to_owned()
                }
                _ => WrongResourceKind {
                    required: ResourceKind::Terminal,
                    actual: ResourceKind::AccessibilityTree,
                }
                .to_string(),
            };
            crate::runtime::send_error(
                &ctx.plumbing.out_tx,
                ErrorCode::WrongResourceKind,
                &message,
            )
            .await;
            FrameFlow::Continue(())
        }
        .boxed_local()
    }
}

/// `PUBLISH_RESOURCE_STATE`: a local owner connection replaces the tree's
/// document; the reply names the sequence the document carries.
async fn publish(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    bytes: Bytes,
) -> CommandResult {
    if !state.with(|s| s.client_may_produce(client_id)) {
        return CommandResult::Error {
            code: ErrorCode::NotProducer,
            message: "publishing a resource's state requires a local owner connection".to_owned(),
        };
    }
    let Some(handle) = state.with(|s| {
        s.resource_from_wire(terminal_id)
            .and_then(|core| s.resource_handle(core).cloned())
    }) else {
        return CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: format!("no such resource: {terminal_id}"),
        };
    };
    let tree = match handle.tree() {
        Ok(tree) => tree,
        Err(error) => return crate::runtime::commands::wrong_resource_kind(error),
    };
    let (reply, rx) = oneshot::channel();
    if tree
        .publish
        .try_send(PublishRequest { bytes, reply })
        .is_err()
    {
        return CommandResult::Error {
            code: ErrorCode::Overflow,
            message: "the tree's publish queue is full".to_owned(),
        };
    }
    match rx.await {
        Ok(Ok(published)) => CommandResult::OkWith(CommandValue::Json(format!(
            "{{\"seq\":{},\"ts_ms\":{}}}",
            published.seq, published.ts_ms
        ))),
        Ok(Err(PublishRejection::Invalid(error))) => CommandResult::Error {
            code: ErrorCode::RecordInvalid,
            message: format!("the document is not JSON: {error}"),
        },
        Ok(Err(PublishRejection::Overflow)) => CommandResult::Error {
            code: ErrorCode::Overflow,
            message: "the tree's sequence space is exhausted".to_owned(),
        },
        Err(_) => CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: "the tree closed before the publish landed".to_owned(),
        },
    }
}
