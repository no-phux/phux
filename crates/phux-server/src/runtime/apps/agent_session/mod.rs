//! The `AgentSession` app: a producer-fed record stream.

use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use phux_protocol::ids::{OperationId, ResourceKey, Revision};
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind};

use super::{
    FrameAdmission, ResourceApp, ResourceFrame, ResourceLaunch, ResourceSpawn, StreamBootstrap,
};
use crate::resource::{ResourceHandle, ResourceKind, WrongResourceKind};
use crate::runtime::commands::{AttachResourceFailure, CommandContext};
use crate::runtime::connections::dispatch::FrameFlow;
use crate::runtime::operation_dedupe::OperationClaim;
use crate::state::{ClientId, SharedState};

mod append;
mod attach;
mod frames;
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
    fn supports_organization_launch(&self) -> bool {
        false
    }

    fn launch(&self, _launch: ResourceLaunch<'_>) -> Result<(), String> {
        Err("an agent-session projection must be opened by its producer".to_owned())
    }

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

    fn refresh_for_attach<'a>(&'a self, _state: &'a SharedState) -> LocalBoxFuture<'a, ()> {
        Box::pin(std::future::ready(()))
    }

    fn prepare_attach<'a>(
        &'a self,
        _attach: crate::runtime::session_attach::SessionAttach<'a>,
        _panes: &'a [crate::state::AttachSnapshotPane],
    ) -> LocalBoxFuture<'a, ()> {
        Box::pin(std::future::ready(()))
    }

    fn attach_viewport(
        &self,
        _s: &mut crate::state::ServerState,
        _panes: &[crate::state::AttachSnapshotPane],
        _viewport: phux_protocol::wire::frame::ViewportInfo,
    ) {
        // A record stream has no grid to size.
    }

    fn after_attach<'a>(
        &'a self,
        _attach: crate::runtime::session_attach::SessionAttach<'a>,
        _panes: &'a [crate::state::AttachSnapshotPane],
        _role: phux_protocol::wire::frame::RolePolicy,
    ) -> LocalBoxFuture<'a, ()> {
        // Only a Terminal has an input role to take.
        Box::pin(std::future::ready(()))
    }

    fn abort_attach<'a>(
        &'a self,
        _client: ClientId,
        _handles: &'a [ResourceHandle],
    ) -> LocalBoxFuture<'a, ()> {
        // The stream is deferred to `ATTACH_RESOURCE`: nothing was staged.
        Box::pin(std::future::ready(()))
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

    fn frame_admission(&self, frame: &FrameKind) -> FrameAdmission {
        if matches!(
            frame,
            FrameKind::InputKey { .. }
                | FrameKind::InputMouse { .. }
                | FrameKind::InputFocus { .. }
                | FrameKind::InputPaste { .. }
                | FrameKind::InputTerminalReply { .. }
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

    fn metrics(&self, _state: &SharedState) -> Vec<&'static [phux_perf::Metric]> {
        Vec::new()
    }

    fn release_consumer(&self, _handle: &ResourceHandle, _client: ClientId) {
        // The record stream keeps nothing per consumer in the engine: a
        // client's pump is host-side and `pump::stop_output` ends it.
    }

    fn fence_consumer<'a>(
        &'a self,
        _handle: &'a ResourceHandle,
        _client: ClientId,
    ) -> LocalBoxFuture<'a, ()> {
        // Nothing to fence: the engine emits only to the shared broadcast.
        Box::pin(std::future::ready(()))
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

    fn begin_upgrade(&self, state: &SharedState) -> Option<Box<dyn super::UpgradeCut>> {
        upgrade::begin(state)
    }

    fn resume(&self, _state: &SharedState) -> Box<dyn super::KindResume> {
        Box::new(upgrade::AgentSessionResume)
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

    fn exit_retained(
        &self,
        _s: &mut crate::state::ServerState,
        _wire: &phux_protocol::ids::ResourceId,
    ) -> Option<crate::hooks::HookEvent> {
        // A record stream is removed on exit; its native controller closes
        // through `resource_exited`.
        None
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

    fn start(&self, _state: &SharedState) -> std::io::Result<Option<super::RunningLane>> {
        Ok(None)
    }

    fn bootstrap_capabilities(
        &self,
        _state: &SharedState,
    ) -> Option<phux_protocol::caps::BootstrapCapabilities> {
        None
    }

    fn seed_session<'a>(
        &'a self,
        seed: super::SessionSeed<'a>,
    ) -> Result<LocalBoxFuture<'a, Option<String>>, super::SessionSeed<'a>> {
        // An agent session runs under its producer, never as a session's seed.
        Err(seed)
    }

    fn viewport_resize(
        &self,
        _state: &SharedState,
        _client: ClientId,
        _viewport: &phux_protocol::wire::frame::ViewportInfo,
    ) {
        // A record stream has no grid to size.
    }

    fn client_activity(&self, _state: &SharedState, _client: ClientId) {}

    fn command_refused(
        &self,
        _s: &crate::state::ServerState,
        _client: ClientId,
        _command: &Command,
        _result: &CommandResult,
    ) {
    }

    fn keep_standing(
        &self,
        _state: &SharedState,
        _source: &crate::runtime::MirrorSource,
        _root_token: &tokio_util::sync::CancellationToken,
    ) -> bool {
        false
    }
}
