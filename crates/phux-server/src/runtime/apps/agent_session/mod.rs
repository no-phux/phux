//! The `AgentSession` app: a producer-fed record stream.

use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use phux_protocol::wire::frame::FrameKind;

use super::{FrameAdmission, ResourceApp, StreamBootstrap};
use crate::runtime::commands::AttachResourceFailure;

pub(crate) use attach::attach_agent_session;
mod attach;
mod frames;

/// Payload budget for one `BOOTSTRAP_CHUNK` when the connection negotiated
/// none. Records are at most 16 KiB, so this fits several per chunk.
const DEFAULT_AGENT_CHUNK_BYTES: usize = 64 * 1024;

/// Serves [`ResourceKind::AgentSession`](crate::resource::ResourceKind::AgentSession).
#[derive(Debug)]
pub(crate) struct AgentSessionApp;

impl ResourceApp for AgentSessionApp {
    fn stage_session<'a>(
        &'a self,
        _capture: super::super::session_attach::SessionCapture<'a>,
        _staging: &'a mut super::super::session_attach::staging::SessionStaging,
        _pane: crate::state::AttachSnapshotPane,
    ) -> LocalBoxFuture<'a, Result<(), super::super::session_attach::staging::CaptureFailure>> {
        // Session ATTACH has no per-agent cursor. Retain catalog membership,
        // but let ATTACH_RESOURCE request the committed suffix instead of
        // overwriting it with an unsolicited full retained bootstrap.
        std::future::ready(Ok(())).boxed_local()
    }

    fn frame_admission(&self, frame: &FrameKind) -> FrameAdmission {
        if matches!(
            frame,
            FrameKind::InputKey { .. }
                | FrameKind::InputMouse { .. }
                | FrameKind::InputFocus { .. }
                | FrameKind::InputPaste { .. }
                | FrameKind::InputTerminalReply { .. }
                | FrameKind::ClipboardReply { .. }
        ) {
            FrameAdmission::Subscription
        } else {
            FrameAdmission::Unrestricted
        }
    }

    /// An agent session serves no stream or input frame: it is raw-profile
    /// with no history beyond its bootstrap (ADR-0103 §4), and input and
    /// geometry are the Terminal facet's. Every refusal keeps the connection.
    fn frame_error(
        &self,
        frame: &FrameKind,
    ) -> Option<(phux_protocol::wire::frame::ErrorCode, String)> {
        Some(frames::error(frame))
    }

    fn bootstrap<'a>(
        &'a self,
        stream: StreamBootstrap<'a>,
    ) -> LocalBoxFuture<'a, Result<(), AttachResourceFailure>> {
        async move {
            let result = attach::attach_agent_session(
                stream.reservation,
                stream.client_id,
                stream.wire,
                stream.handle,
                stream.content_tx,
                stream.limits,
                stream.after_seq,
            )
            .await;
            super::producer_stream::result_to_attach_failure("agent session", result)
        }
        .boxed_local()
    }
}
