//! The `AgentSession` app: a producer-fed record stream.

use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind};

use super::{FrameAdmission, ResourceApp, ResourceFrame, ResourceSpawn, StreamBootstrap};
use crate::runtime::commands::{AttachResourceFailure, CommandContext};
use crate::runtime::connections::dispatch::FrameFlow;
use crate::runtime::operation_dedupe::OperationClaim;
use crate::state::SharedState;

mod append;
mod attach;
mod frames;
mod native_agent;
pub(super) mod spawn;
pub(crate) mod upgrade;

pub(crate) use native_agent::{claim_relaunch, retry_pending_closures, source_exited};

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
    fn frame<'a>(
        &'a self,
        ctx: ResourceFrame<'a>,
        frame: FrameKind,
    ) -> LocalBoxFuture<'a, FrameFlow> {
        std::future::ready(frames::serve(&ctx, &frame)).boxed_local()
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
                _ => CommandResult::error(
                    ErrorCode::InvalidCommand,
                    "command not supported by this server",
                ),
            }
        }
        .boxed_local()
    }

    fn bootstrap<'a>(
        &'a self,
        stream: StreamBootstrap<'a>,
    ) -> LocalBoxFuture<'a, Result<(), AttachResourceFailure>> {
        async move {
            let result = attach::attach_agent_session(
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
            .await;
            super::producer_stream::result_to_attach_failure("agent session", result)
        }
        .boxed_local()
    }

    fn spawn<'a>(&'a self, spawn: ResourceSpawn<'a>) -> LocalBoxFuture<'a, ()> {
        spawn::spawn_agent_session(spawn).boxed_local()
    }

    fn begin_upgrade(&self, state: &SharedState) -> Option<Box<dyn super::UpgradeCut>> {
        upgrade::begin(state)
    }

    fn resume(&self, _state: &SharedState) -> Box<dyn super::KindResume> {
        Box::new(upgrade::AgentSessionResume)
    }
}
