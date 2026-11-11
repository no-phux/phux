//! The document apps: a producer-fed, replicated document whose newest
//! version is what every viewer wants (`docs/resource-kinds.md`). One
//! generic app serves every [`DocumentApi`]: the `AccessibilityTree` (JSON),
//! the `PixelSurface` (PNG or JPEG) and the `Document` (any content type).

use bytes::Bytes;
use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::ids::{OperationId, ResourceKey, Revision};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode, FrameKind};
use phux_tree::{
    DocumentKind, DocumentResource as _, PublishRejection, PublishRequest, SurfaceDocument,
    TreeDocument, TypedDocument,
};
use tokio::sync::oneshot;

use super::{
    FrameAdmission, ResourceApp, ResourceFrame, ResourceLaunch, ResourceSpawn, StreamBootstrap,
};
use crate::resource::{ResourceHandle, ResourceKind, WrongResourceKind};
use crate::runtime::commands::{AttachResourceFailure, CommandContext};
use crate::runtime::connections::dispatch::FrameFlow;
use crate::runtime::operation_dedupe::OperationClaim;
use crate::state::{ClientId, ServerState, SharedState};

mod attach;
pub(crate) mod audit;
mod input;
mod spawn;
pub(crate) mod upgrade;

pub(crate) use attach::attach_document;
pub(crate) use spawn::spawn_document;

/// What the host needs from a document kind beyond its engine.
pub(crate) trait DocumentApi: DocumentKind {
    /// Validate a captured payload of a resource opened on `target` without
    /// constructing a second document.
    fn validate_bootstrap(target: &str, bytes: &[u8]) -> Result<(), String>;
    /// A viewer's bootstrap payload for the newest `document`: the document
    /// itself unless the kind's codec frames it.
    fn bootstrap_payload(_target: &str, document: Option<&Bytes>) -> Bytes {
        document.cloned().unwrap_or_default()
    }
    /// The stream profile a viewer's bootstrap names.
    const PROFILE: BootstrapStreamProfile;
    /// The kind on the wire.
    const WIRE_KIND: phux_protocol::ids::ResourceKind;
    /// How a message names the kind, e.g. "accessibility tree".
    const LABEL: &'static str;
    /// The target a record of this kind was opened on; `None` for a
    /// record of another kind.
    fn launch_target(launch: &phux_core::LaunchSpec) -> Option<&str>;
    /// Create the registry record under `parent`, running, named `name`;
    /// a tree or surface is named for what it observes.
    fn create(
        state: &mut ServerState,
        parent: phux_core::ids::ResourceId,
        target: &str,
        name: Option<&str>,
        actor: &phux_core::actor::Actor,
    ) -> Result<phux_core::ids::ResourceId, phux_core::Refusal>;
}

/// What a tree or surface observes (`--app Safari`, a page title), as its
/// record name, so every client's tab reads it. A target a name may not
/// carry (a control character) leaves the resource unnamed.
fn target_name(target: &str) -> Option<String> {
    (!target.trim().is_empty() && !target.chars().any(char::is_control)).then(|| target.to_owned())
}

impl DocumentApi for TreeDocument {
    fn validate_bootstrap(_target: &str, bytes: &[u8]) -> Result<(), String> {
        serde_json::from_slice::<serde::de::IgnoredAny>(bytes)
            .map(|_| ())
            .map_err(|error| format!("the document is not JSON: {error}"))
    }

    const PROFILE: BootstrapStreamProfile = BootstrapStreamProfile::AccessibilityTreeJsonV1;
    const WIRE_KIND: phux_protocol::ids::ResourceKind =
        phux_protocol::ids::ResourceKind::AccessibilityTree;
    const LABEL: &'static str = "accessibility tree";

    fn launch_target(launch: &phux_core::LaunchSpec) -> Option<&str> {
        match launch {
            phux_core::LaunchSpec::Tree(tree) => Some(&tree.target),
            _ => None,
        }
    }

    fn create(
        state: &mut ServerState,
        parent: phux_core::ids::ResourceId,
        target: &str,
        _name: Option<&str>,
        actor: &phux_core::actor::Actor,
    ) -> Result<phux_core::ids::ResourceId, phux_core::Refusal> {
        state.create_named_document(
            parent,
            phux_core::LaunchSpec::Tree(phux_core::resource::TreeLaunch {
                target: target.to_owned(),
            }),
            target_name(target),
            actor,
        )
    }
}

impl DocumentApi for SurfaceDocument {
    fn validate_bootstrap(_target: &str, bytes: &[u8]) -> Result<(), String> {
        phux_tree::image_size(bytes).map(|_| ())
    }

    const PROFILE: BootstrapStreamProfile = BootstrapStreamProfile::PixelSurfacePngV1;
    const WIRE_KIND: phux_protocol::ids::ResourceKind =
        phux_protocol::ids::ResourceKind::PixelSurface;
    const LABEL: &'static str = "pixel surface";

    fn launch_target(launch: &phux_core::LaunchSpec) -> Option<&str> {
        match launch {
            phux_core::LaunchSpec::Surface(surface) => Some(&surface.target),
            _ => None,
        }
    }

    fn create(
        state: &mut ServerState,
        parent: phux_core::ids::ResourceId,
        target: &str,
        _name: Option<&str>,
        actor: &phux_core::actor::Actor,
    ) -> Result<phux_core::ids::ResourceId, phux_core::Refusal> {
        state.create_named_document(
            parent,
            phux_core::LaunchSpec::Surface(phux_core::resource::SurfaceLaunch {
                target: target.to_owned(),
            }),
            target_name(target),
            actor,
        )
    }
}

/// A typed document's `target` is its content type, which every viewer's
/// bootstrap names before the newest document.
impl DocumentApi for TypedDocument {
    fn validate_bootstrap(content_type: &str, bytes: &[u8]) -> Result<(), String> {
        <Self as DocumentKind>::validate(content_type, bytes)
    }

    fn bootstrap_payload(content_type: &str, document: Option<&Bytes>) -> Bytes {
        let document = document.map_or(&[][..], |bytes| &bytes[..]);
        phux_protocol::wire::frame::typed_document::encode_bootstrap(content_type, document).into()
    }

    const PROFILE: BootstrapStreamProfile = BootstrapStreamProfile::DocumentTypedV1;
    const WIRE_KIND: phux_protocol::ids::ResourceKind = phux_protocol::ids::ResourceKind::Document;
    const LABEL: &'static str = "document";

    fn launch_target(launch: &phux_core::LaunchSpec) -> Option<&str> {
        match launch {
            phux_core::LaunchSpec::Document(document) => Some(&document.content_type),
            _ => None,
        }
    }

    fn create(
        state: &mut ServerState,
        parent: phux_core::ids::ResourceId,
        content_type: &str,
        name: Option<&str>,
        actor: &phux_core::actor::Actor,
    ) -> Result<phux_core::ids::ResourceId, phux_core::Refusal> {
        state.create_named_document(
            parent,
            phux_core::LaunchSpec::Document(phux_core::resource::DocumentLaunch {
                content_type: content_type.to_owned(),
            }),
            name.map(str::to_owned),
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
/// Serves [`ResourceKind::Document`].
pub(crate) static DOCUMENT_APP: DocumentApp<TypedDocument> = DocumentApp(std::marker::PhantomData);

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

    fn stage_session<'a>(
        &'a self,
        capture: super::super::session_attach::SessionCapture<'a>,
        staging: &'a mut super::super::session_attach::staging::SessionStaging,
        pane: crate::state::AttachSnapshotPane,
    ) -> LocalBoxFuture<'a, Result<(), super::super::session_attach::staging::CaptureFailure>> {
        attach::stage_session::<A>(capture, staging, pane).boxed_local()
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

    fn begin_upgrade(&self, _state: &SharedState) -> Option<Box<dyn super::UpgradeCut>> {
        Some(upgrade::DocumentCut::<A>::begin())
    }

    fn resume(&self, _state: &SharedState) -> Box<dyn super::KindResume> {
        upgrade::DocumentResume::<A>::begin()
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
                    publish::<A>(ctx, &terminal_id, Bytes::from(bytes)).await
                }
                Command::SendDocumentInput {
                    terminal_id,
                    intent,
                    control,
                    on_behalf_of,
                } => {
                    let audited = intent.clone();
                    let relayed = on_behalf_of.as_ref();
                    let result =
                        input::relay::<A>(ctx, &terminal_id, intent, control, relayed).await;
                    ctx.state.with(|s| {
                        audit::record(s, ctx.client_id, relayed, &terminal_id, &audited, &result);
                    });
                    result
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

    fn frame_admission(&self, _frame: &FrameKind) -> FrameAdmission {
        // Preserve the document app's unconditional wrong-kind refusals.
        FrameAdmission::Unrestricted
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
        return CommandResult::Error {
            code: ErrorCode::NotProducer,
            message: "publishing a resource's state requires a local owner connection".to_owned(),
        };
    }
    let Some((core, handle)) = state.with(|s| {
        let core = s.resource_from_wire(terminal_id)?;
        Some((core, s.resource_handle(core)?.clone()))
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
        Ok(Ok(published)) => {
            state.with_mut(|s| {
                if s.resource_producer(core).is_none() {
                    s.record_spawn(core, client_id);
                    s.subscribe_resource(client_id, core, Some(ctx.out_tx.clone()));
                }
            });
            CommandResult::OkWith(CommandValue::Json(format!(
                "{{\"seq\":{},\"ts_ms\":{}}}",
                published.seq, published.ts_ms
            )))
        }
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
