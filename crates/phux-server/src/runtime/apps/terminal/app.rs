//! [`TerminalApp`]: the Terminal facet's commands and stream bootstrap.

use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use phux_protocol::wire::frame::{ErrorCode, FrameKind};
use phux_terminal_api::actor::TerminalResource as _;

use crate::runtime::apps::terminal::capture::attach_resource::AttachResourceSession;
use crate::runtime::apps::{FrameAdmission, NO_TERMINAL_PROFILE, ResourceApp, StreamBootstrap};
use crate::runtime::commands::AttachResourceFailure;

/// Serves [`ResourceKind::Terminal`](crate::resource::ResourceKind::Terminal).
#[derive(Debug)]
pub(crate) struct TerminalApp;

impl ResourceApp for TerminalApp {
    fn stage_session<'a>(
        &'a self,
        capture: crate::runtime::session_attach::SessionCapture<'a>,
        staging: &'a mut crate::runtime::session_attach::staging::SessionStaging,
        pane: crate::state::AttachSnapshotPane,
    ) -> LocalBoxFuture<'a, Result<(), crate::runtime::session_attach::staging::CaptureFailure>>
    {
        super::attach::stage_session(capture, staging, pane).boxed_local()
    }

    fn bootstrap<'a>(
        &'a self,
        stream: StreamBootstrap<'a>,
    ) -> LocalBoxFuture<'a, Result<(), AttachResourceFailure>> {
        bootstrap_stream(stream).boxed_local()
    }

    fn frame_admission(&self, frame: &FrameKind) -> FrameAdmission {
        match frame {
            FrameKind::InputKey { .. }
            | FrameKind::InputMouse { .. }
            | FrameKind::InputFocus { .. }
            | FrameKind::InputPaste { .. }
            | FrameKind::ClipboardReply { .. } => FrameAdmission::AttachedInput,
            FrameKind::FrameAck { .. } => FrameAdmission::Subscription,
            _ => FrameAdmission::Unrestricted,
        }
    }

    fn frame_error(
        &self,
        _frame: &FrameKind,
    ) -> Option<(phux_protocol::wire::frame::ErrorCode, String)> {
        None
    }
}

/// Bootstrap one Terminal's content stream into its mailbox without touching
/// the caller's subscription; a failure cancels the generation only.
async fn bootstrap_stream(stream: StreamBootstrap<'_>) -> Result<(), AttachResourceFailure> {
    let StreamBootstrap {
        reservation,
        client_id,
        wire: terminal_id,
        handle,
        content_tx,
        stream_id,
        client_caps,
        profile: bootstrap_profile,
        limits: bootstrap_limits,
        ..
    } = stream;
    let Some(bootstrap_profile) = bootstrap_profile else {
        return Err(AttachResourceFailure {
            generation: None,
            code: ErrorCode::CodecUnavailable,
            message: NO_TERMINAL_PROFILE.to_owned(),
        });
    };
    let Some(stream_profile) =
        crate::runtime::apps::terminal::attach::bootstrap_stream_profile(bootstrap_profile)
    else {
        return Err(AttachResourceFailure {
            generation: None,
            code: ErrorCode::CodecUnavailable,
            message: "ATTACH_RESOURCE selected an unsupported bootstrap profile".to_owned(),
        });
    };

    let terminal = match handle.terminal() {
        Ok(terminal) => terminal.clone(),
        Err(error) => {
            return Err(AttachResourceFailure {
                generation: None,
                code: ErrorCode::WrongResourceKind,
                message: error.to_string(),
            });
        }
    };
    let session = AttachResourceSession {
        out_tx: content_tx,
        terminal_id,
        handle: handle.clone(),
        terminal,
        client_id,
        stream_id,
        client_caps,
        stream_profile,
        bootstrap_limits,
    };

    let generation = session.establish_generation(reservation).await?;
    session.publish_generation(generation).await
}
