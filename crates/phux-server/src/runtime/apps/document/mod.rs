//! The document apps: a producer-fed, replicated document whose newest
//! version is what every viewer wants (`docs/resource-kinds.md`). One
//! generic app serves every [`DocumentApi`]: the `AccessibilityTree` (JSON)
//! and the `PixelSurface` (PNG).

use bytes::Bytes;
use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::ids::{OperationId, ResourceKey, Revision};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode, FrameKind};
use phux_tree::{
    DocumentKind, DocumentResource as _, PublishRejection, PublishRequest, SurfaceDocument,
    TreeDocument,
};
use tokio::sync::oneshot;

use super::{ResourceApp, ResourceFrame, ResourceLaunch, ResourceSpawn, StreamBootstrap};
use crate::resource::{ResourceHandle, ResourceKind, WrongResourceKind};
use crate::runtime::commands::{AttachResourceFailure, CommandContext};
use crate::runtime::connections::dispatch::FrameFlow;
use crate::runtime::operation_dedupe::OperationClaim;
use crate::state::{ClientId, ServerState, SharedState};

mod attach;
mod spawn;

pub(crate) use attach::attach_document;
pub(crate) use spawn::spawn_document;

/// What the host needs from a document kind beyond its engine.
pub(crate) trait DocumentApi: DocumentKind {
    /// The stream profile a viewer's bootstrap names.
    const PROFILE: BootstrapStreamProfile;
    /// The kind on the wire.
    const WIRE_KIND: phux_protocol::ids::ResourceKind;
    /// How a message names the kind, e.g. "accessibility tree".
    const LABEL: &'static str;
    /// Create the registry record under `parent`, running.
    fn create(
        state: &mut ServerState,
        parent: phux_core::ids::ResourceId,
        target: &str,
        actor: &phux_core::actor::Actor,
    ) -> Result<phux_core::ids::ResourceId, phux_core::Refusal>;
}

impl DocumentApi for TreeDocument {
    const PROFILE: BootstrapStreamProfile = BootstrapStreamProfile::AccessibilityTreeJsonV1;
    const WIRE_KIND: phux_protocol::ids::ResourceKind =
        phux_protocol::ids::ResourceKind::AccessibilityTree;
    const LABEL: &'static str = "accessibility tree";

    fn create(
        state: &mut ServerState,
        parent: phux_core::ids::ResourceId,
        target: &str,
        actor: &phux_core::actor::Actor,
    ) -> Result<phux_core::ids::ResourceId, phux_core::Refusal> {
        state.create_document(
            parent,
            phux_core::LaunchSpec::Tree(phux_core::resource::TreeLaunch {
                target: target.to_owned(),
            }),
            actor,
        )
    }
}

impl DocumentApi for SurfaceDocument {
    const PROFILE: BootstrapStreamProfile = BootstrapStreamProfile::PixelSurfacePngV1;
    const WIRE_KIND: phux_protocol::ids::ResourceKind =
        phux_protocol::ids::ResourceKind::PixelSurface;
    const LABEL: &'static str = "pixel surface";

    fn create(
        state: &mut ServerState,
        parent: phux_core::ids::ResourceId,
        target: &str,
        actor: &phux_core::actor::Actor,
    ) -> Result<phux_core::ids::ResourceId, phux_core::Refusal> {
        state.create_document(
            parent,
            phux_core::LaunchSpec::Surface(phux_core::resource::SurfaceLaunch {
                target: target.to_owned(),
            }),
            actor,
        )
    }
}

/// Serves one document kind.
#[derive(Debug)]
pub(crate) struct DocumentApp<A: DocumentApi>(std::marker::PhantomData<A>);

/// Serves [`ResourceKind::AccessibilityTree`].
pub(crate) static TREE_APP: DocumentApp<TreeDocument> = DocumentApp(std::marker::PhantomData);
/// Serves [`ResourceKind::PixelSurface`].
pub(crate) static SURFACE_APP: DocumentApp<SurfaceDocument> = DocumentApp(std::marker::PhantomData);

impl<A: DocumentApi> ResourceApp for DocumentApp<A> {
    fn supports_organization_launch(&self) -> bool {
        false
    }

    fn launch(&self, _launch: ResourceLaunch<'_>) -> Result<(), String> {
        Err(format!(
            "a {} projection must be opened by its producer",
            A::LABEL
        ))
    }

    fn defers_stream_bootstrap(&self) -> bool {
        false
    }

    fn resumes_from_cursor(&self) -> bool {
        false
    }

    fn check_takeover(&self) -> Result<(), WrongResourceKind> {
        Err(WrongResourceKind {
            required: ResourceKind::Terminal,
            actual: A::KIND,
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
        // The producer spawns a fresh document. Ordinary relaunch falls
        // through to the host's app-support refusal before any commit.
        None
    }

    fn spawn<'a>(&'a self, spawn: ResourceSpawn<'a>) -> LocalBoxFuture<'a, ()> {
        spawn_document::<A>(spawn).boxed_local()
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
                    publish::<A>(&ctx.state, ctx.client_id, &terminal_id, Bytes::from(bytes)).await
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
            match attach_document::<A>(
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
                    "{} attach returned an unexpected result: {other:?}",
                    A::LABEL
                ))),
            }
        }
        .boxed_local()
    }

    /// A document serves no stream or input frame: it has no history, no
    /// grid, and takes no input. Every refusal keeps the connection.
    fn frame<'a>(
        &'a self,
        ctx: ResourceFrame<'a>,
        frame: FrameKind,
    ) -> LocalBoxFuture<'a, FrameFlow> {
        async move {
            let message = match frame {
                FrameKind::FrameAck { .. } => {
                    format!("FRAME_ACK is not valid on a {} stream", A::LABEL)
                }
                _ => WrongResourceKind {
                    required: ResourceKind::Terminal,
                    actual: A::KIND,
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

/// `PUBLISH_RESOURCE_STATE`: a local owner connection replaces the
/// document; the reply names the sequence the document carries.
async fn publish<A: DocumentApi>(
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
        return CommandResult::Error {
            code: ErrorCode::Overflow,
            message: format!("the {}'s publish queue is full", A::LABEL),
        };
    }
    match rx.await {
        Ok(Ok(published)) => CommandResult::OkWith(CommandValue::Json(format!(
            "{{\"seq\":{},\"ts_ms\":{}}}",
            published.seq, published.ts_ms
        ))),
        Ok(Err(PublishRejection::Invalid(error))) => CommandResult::Error {
            code: ErrorCode::RecordInvalid,
            message: error,
        },
        Ok(Err(PublishRejection::Overflow)) => CommandResult::Error {
            code: ErrorCode::Overflow,
            message: format!("the {}'s sequence space is exhausted", A::LABEL),
        },
        Err(_) => CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: format!("the {} closed before the publish landed", A::LABEL),
        },
    }
}
