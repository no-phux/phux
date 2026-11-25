//! The document apps: a producer-fed, replicated document whose newest
//! version is what every viewer wants (`docs/resource-kinds.md`). One
//! generic app serves every [`DocumentApi`]: the `AccessibilityTree` (JSON),
//! the `PixelSurface` (PNG or JPEG) and the `Document` (any content type).

use bytes::Bytes;
use engine::{DocumentKind, SurfaceDocument, TreeDocument, TypedDocument};
use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::wire::frame::{ErrorCode, FrameKind};

use super::{FrameAdmission, ResourceApp, StreamBootstrap};
use crate::resource::{ResourceHandle, ResourceKind, WrongResourceKind};
use crate::runtime::commands::AttachResourceFailure;

mod attach;
pub(crate) mod engine;
mod kinds;

pub(crate) use attach::attach_document;

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
    /// Pure creation intent; the host owns parent authority and registration.
    fn creation(target: &str, name: Option<&str>) -> phux_core::NewResource;
}

/// `handle` carrying the control class its producer declared, which the host
/// reads as the resource's control capability; none controls nothing.
pub(crate) fn with_control(
    handle: ResourceHandle,
    control: Option<phux_protocol::wire::frame::ControlClass>,
) -> ResourceHandle {
    match control {
        Some(class) => handle.with_companion(class),
        None => handle,
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
    fn stage_session<'a>(
        &'a self,
        capture: super::super::session_attach::SessionCapture<'a>,
        staging: &'a mut super::super::session_attach::staging::SessionStaging,
        pane: crate::state::AttachSnapshotPane,
    ) -> LocalBoxFuture<'a, Result<(), super::super::session_attach::staging::CaptureFailure>> {
        attach::stage_session::<A>(capture, staging, pane).boxed_local()
    }


    fn bootstrap<'a>(
        &'a self,
        stream: StreamBootstrap<'a>,
    ) -> LocalBoxFuture<'a, Result<(), AttachResourceFailure>> {
        async move {
            let result = attach_document::<A>(
                stream.reservation,
                stream.client_id,
                stream.wire,
                stream.handle,
                stream.content_tx,
                stream.limits,
            )
            .await;
            super::producer_stream::result_to_attach_failure(A::LABEL, result)
        }
        .boxed_local()
    }

    fn frame_admission(&self, _frame: &FrameKind) -> FrameAdmission {
        // Preserve the document app's unconditional wrong-kind refusals.
        FrameAdmission::Unrestricted
    }

    /// A document serves no stream or input frame: it has no history, no
    /// grid, and takes no input. Every refusal keeps the connection.
    fn frame_error(&self, frame: &FrameKind) -> Option<(ErrorCode, String)> {
        let message = match frame {
            FrameKind::FrameAck { .. } => {
                format!("FRAME_ACK is not valid on a {} stream", A::LABEL)
            }
            _ => WrongResourceKind {
                required: ResourceKind::Terminal,
                actual: A::KIND,
            }.to_string(),
        };
        Some((ErrorCode::WrongResourceKind, message))
    }
}

