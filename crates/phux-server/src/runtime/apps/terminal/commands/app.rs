//! [`TerminalApp`]: the Terminal facet's commands and stream bootstrap.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use phux_protocol::ids::{OperationId, ResourceKey, Revision};
use phux_protocol::wire::frame::SpawnError;
use phux_terminal_api::actor::TerminalResource as _;

use super::*;
use crate::resource::ResourceKind;
use crate::runtime::apps::{
    FrameAdmission, KindResume, NO_TERMINAL_PROFILE, ResourceApp, ResourceFrame, ResourceLaunch,
    ResourceSpawn, RunningLane, SessionSeed, StreamBootstrap, UpgradeCut,
};
use crate::runtime::connections::dispatch::FrameFlow;
use crate::runtime::operation_dedupe::OperationClaim;

/// Serves [`ResourceKind::Terminal`].
#[derive(Debug)]
pub(crate) struct TerminalApp;

impl ResourceApp for TerminalApp {
    fn supports_organization_launch(&self) -> bool {
        true
    }

    fn launch(&self, launch: ResourceLaunch<'_>) -> Result<(), String> {
        super::super::launch::execute(launch)
    }

    fn stage_session<'a>(
        &'a self,
        capture: crate::runtime::session_attach::SessionCapture<'a>,
        staging: &'a mut crate::runtime::session_attach::staging::SessionStaging,
        pane: crate::state::AttachSnapshotPane,
    ) -> LocalBoxFuture<'a, Result<(), crate::runtime::session_attach::staging::CaptureFailure>>
    {
        super::super::attach::stage_session(capture, staging, pane).boxed_local()
    }

    fn refresh_for_attach<'a>(&'a self, state: &'a SharedState) -> LocalBoxFuture<'a, ()> {
        super::super::attach::refresh_registry_cwds(state).boxed_local()
    }

    fn prepare_attach<'a>(
        &'a self,
        attach: crate::runtime::session_attach::SessionAttach<'a>,
        panes: &'a [crate::state::AttachSnapshotPane],
    ) -> LocalBoxFuture<'a, ()> {
        async move {
            if attach.same_session {
                let client = crate::runtime::wire_client(attach.client_id);
                super::super::attach::detach_prior_state_sync_consumers(
                    panes,
                    client,
                    attach.client_caps,
                )
                .await;
            }
            let colors = attach.client_caps.default_colors;
            super::super::attach::apply_client_default_colors(panes, colors).await;
        }
        .boxed_local()
    }

    fn attach_viewport(
        &self,
        s: &mut crate::state::ServerState,
        panes: &[crate::state::AttachSnapshotPane],
        viewport: phux_protocol::wire::frame::ViewportInfo,
    ) {
        super::super::attach::apply_panes_viewport(s, panes, viewport);
    }

    fn after_attach<'a>(
        &'a self,
        attach: crate::runtime::session_attach::SessionAttach<'a>,
        panes: &'a [crate::state::AttachSnapshotPane],
        role: phux_protocol::wire::frame::RolePolicy,
    ) -> LocalBoxFuture<'a, ()> {
        let (state, client, was_subscribed) = (attach.state, attach.client_id, attach.same_session);
        super::super::attach::apply_session_role(state, client, panes, role, was_subscribed)
            .boxed_local()
    }

    fn abort_attach<'a>(
        &'a self,
        client: ClientId,
        handles: &'a [ResourceHandle],
    ) -> LocalBoxFuture<'a, ()> {
        super::super::attach::release_aborted_consumers(client, handles).boxed_local()
    }

    fn defers_stream_bootstrap(&self) -> bool {
        true
    }

    fn resumes_from_cursor(&self) -> bool {
        false
    }

    fn check_takeover(&self) -> Result<(), crate::resource::WrongResourceKind> {
        Ok(())
    }

    fn metrics(&self, state: &SharedState) -> Vec<&'static [phux_perf::Metric]> {
        // The injected engine's rows; the app keeps none of its own.
        state
            .terminal_engine()
            .map(|engine| engine.metrics())
            .into_iter()
            .collect()
    }

    fn release_consumer(&self, handle: &ResourceHandle, client: ClientId) {
        release_terminal_consumer(handle, client);
    }

    fn fence_consumer<'a>(
        &'a self,
        handle: &'a ResourceHandle,
        client: ClientId,
    ) -> LocalBoxFuture<'a, ()> {
        fence_terminal_consumer(handle, client).boxed_local()
    }

    fn command<'a>(
        &'a self,
        ctx: &'a CommandContext,
        command: Command,
        claim: Option<&'a OperationClaim>,
    ) -> LocalBoxFuture<'a, CommandResult> {
        serve(ctx, command, claim).boxed_local()
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

    fn frame<'a>(
        &'a self,
        ctx: ResourceFrame<'a>,
        frame: FrameKind,
    ) -> LocalBoxFuture<'a, FrameFlow> {
        super::super::frames::serve_frame(ctx, frame).boxed_local()
    }

    fn spawn<'a>(&'a self, spawn: ResourceSpawn<'a>) -> LocalBoxFuture<'a, ()> {
        super::super::attach::handle_spawn_terminal(spawn).boxed_local()
    }

    fn begin_upgrade(&self, _state: &SharedState) -> Option<Box<dyn UpgradeCut>> {
        Some(Box::<super::super::upgrade::TerminalCut>::default())
    }

    fn resume(&self, state: &SharedState) -> Box<dyn KindResume> {
        super::super::upgrade::resume(state)
    }

    fn resumed(&self, _state: &SharedState) {
        // Terminals come back whole from the blob: nothing waits on a resume.
    }

    fn resource_exited<'a>(
        &'a self,
        _state: &'a SharedState,
        _resource: phux_core::ids::ResourceId,
    ) -> LocalBoxFuture<'a, ()> {
        // The host's pane-exit path retires a Terminal itself.
        Box::pin(std::future::ready(()))
    }

    fn exit_retained(
        &self,
        s: &mut crate::state::ServerState,
        wire: &phux_protocol::ids::ResourceId,
    ) -> Option<crate::hooks::HookEvent> {
        super::super::agent_state::retract_on_retained_exit(s, wire)
    }

    fn claim_relaunch(
        &self,
        _ctx: &CommandContext,
        _operation: OperationId,
        _resource: &ResourceKey,
        _expected: Revision,
    ) -> Option<CommandResult> {
        // A Terminal relaunch is the organization path's.
        None
    }

    fn start(&self, state: &SharedState) -> std::io::Result<Option<RunningLane>> {
        let lane = super::super::input_lane::start(state)?;
        // Off the runtime thread: a large upload directory is a walk.
        let upload_env = state.with(crate::state::ServerState::server_env);
        drop(tokio::task::spawn_blocking(move || {
            super::super::upload::sweep_stale_partials_at_startup(&upload_env);
        }));
        Ok(Some(lane))
    }

    fn bootstrap_capabilities(
        &self,
        state: &SharedState,
    ) -> Option<phux_protocol::caps::BootstrapCapabilities> {
        Some(super::super::bootstrap_capabilities(state))
    }

    fn seed_session<'a>(
        &'a self,
        seed: SessionSeed<'a>,
    ) -> Result<LocalBoxFuture<'a, Option<String>>, SessionSeed<'a>> {
        // The one kind that runs without a producer seeds a session.
        Ok(super::super::attach::resolve_create_if_missing(seed).boxed_local())
    }

    fn viewport_resize(&self, state: &SharedState, client: ClientId, viewport: &ViewportInfo) {
        handle_viewport_resize(state, client, viewport);
    }

    fn client_activity(&self, state: &SharedState, client: ClientId) {
        handle_client_activity(state, client);
    }

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
        state: &SharedState,
        source: &crate::runtime::MirrorSource,
        root_token: &CancellationToken,
    ) -> bool {
        super::super::standing::install(state, source, root_token);
        true
    }
}

/// The Terminal facet of a handle `app_for` routed here. A miss means the
/// handle's kind and facet disagree, which leaks a consumer: say so.
fn routed_terminal(handle: &ResourceHandle) -> Option<&phux_terminal_api::actor::TerminalHandle> {
    handle
        .terminal()
        .inspect_err(|error| tracing::error!(%error, "consumer teardown skipped"))
        .ok()
}

/// Free a client's native cut and consumer entry without blocking: a full
/// mailbox is reaped by `tick_emit` when the client's mailbox closes.
fn release_terminal_consumer(handle: &ResourceHandle, client_id: ClientId) {
    let Some(terminal) = routed_terminal(handle) else {
        return;
    };
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    let _ = terminal
        .native_release
        .try_send(phux_terminal_api::actor::NativeReleaseRequest { owner: client_id.0 });
    let (reply_tx, _reply_rx) = oneshot::channel();
    match terminal
        .consumer_detach
        .try_send(phux_terminal_api::actor::ConsumerDetachRequest {
            client_id: crate::runtime::wire_client(client_id),
            reply: reply_tx,
        }) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            trace!(
                ?client_id,
                "consumer_detach mailbox full; entry reaped by tick_emit when its mailbox closes",
            );
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            trace!(
                ?client_id,
                "consumer_detach: pane actor gone; nothing to free"
            );
        }
    }
}

/// Drop a consumer and wait for the actor's reply, so nothing it emitted can
/// follow what the caller queues next.
async fn fence_terminal_consumer(handle: &ResourceHandle, client_id: ClientId) {
    let Some(terminal) = routed_terminal(handle) else {
        return;
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    let _ = terminal
        .consumer_detach
        .send(phux_terminal_api::actor::ConsumerDetachRequest {
            client_id: crate::runtime::wire_client(client_id),
            reply: reply_tx,
        })
        .await;
    let _ = reply_rx.await;
}

/// One flat arm per Terminal facet command keeps the catalog auditable.
#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "one flat dispatch arm per wire command keeps the catalog auditable"
)]
async fn serve(
    ctx: &CommandContext,
    command: Command,
    claim: Option<&OperationClaim>,
) -> CommandResult {
    let CommandContext {
        state, client_id, ..
    } = ctx;
    let client_id = *client_id;
    let input_lane = ctx
        .input_lane
        .as_ref()
        .and_then(super::super::input_lane::handle_of);

    match command {
        Command::Transcribe {
            upload_id,
            terminal_id,
        } => {
            crate::runtime::apps::terminal::voice::handle_transcribe(
                state,
                client_id,
                upload_id,
                &terminal_id,
                input_lane,
            )
            .await
        }
        Command::GetScreen {
            terminal_id,
            request_scrollback,
            cells,
            format,
        } => handle_get_screen(state, &terminal_id, request_scrollback, cells, format).await,
        Command::RouteInput { terminal_id, event } => match input_lane {
            // A held `ROUTE_INPUT` waits for a pane input credit exactly as a
            // direct one does (ADR-0144).
            Some(lane) => {
                match crate::runtime::apps::terminal::input_lane::acquire_credit(
                    state,
                    &terminal_id,
                )
                .await
                {
                    Ok(credit) => {
                        lane.begin_route(state, client_id, terminal_id, event, credit)
                            .await
                    }
                    Err(crate::runtime::apps::terminal::input_lane::InputStalled) => {
                        crate::runtime::apps::terminal::input_lane::InputStalled::result(
                            &terminal_id,
                        )
                    }
                }
            }
            None => handle_route_input(state, client_id, &terminal_id, event),
        },
        Command::ApplyInput {
            operation_id,
            terminal_id,
            events,
        } => match input_lane {
            Some(lane) => {
                lane.begin_apply(state, client_id, operation_id, terminal_id, events)
                    .await
            }
            None => CommandResult::Error {
                code: ErrorCode::InternalError,
                message: "acknowledged input lane unavailable".to_owned(),
            },
        },
        Command::GetTerminalState {
            terminal_id,
            include_scrollback,
            max_scrollback_lines,
        } => {
            handle_get_terminal_state(
                state,
                &terminal_id,
                include_scrollback,
                max_scrollback_lines,
            )
            .await
        }
        Command::AcquireInput {
            terminal_id,
            mode,
            ttl_ms,
        } => handle_acquire_input(state, client_id, &terminal_id, mode, ttl_ms).await,
        Command::ReleaseInput { terminal_id } => {
            handle_release_input(state, client_id, &terminal_id).await
        }
        Command::SignalTerminal {
            terminal_id,
            signal,
            operation_id,
        } => {
            handle_signal_terminal(state, client_id, &terminal_id, signal, operation_id, claim)
                .await
        }
        Command::PutFile {
            upload_id,
            terminal_id,
            extension,
            offset,
            data,
            final_chunk,
            sha256,
        } => {
            crate::runtime::apps::terminal::upload::handle_put_file(
                state,
                crate::runtime::apps::terminal::upload::PutFileChunk {
                    upload_id,
                    terminal_id,
                    extension,
                    offset,
                    data,
                    final_chunk,
                    sha256,
                    principal: state.with(|s| {
                        crate::runtime::apps::terminal::upload::upload_principal(s, client_id)
                    }),
                },
            )
            .await
        }
        Command::Checkout {
            terminal_id,
            request,
        } => crate::runtime::apps::terminal::checkout::handle(state, &terminal_id, request).await,
        Command::ReportAsked {
            terminal_id,
            id,
            question,
            suggestions,
            elapsed_seconds,
        } => handle_report_asked(
            state,
            &terminal_id,
            id,
            question,
            suggestions,
            elapsed_seconds,
        ),
        Command::ReportAgentState {
            terminal_id,
            state: reported,
        } => handle_report_agent_state(state, &terminal_id, reported).await,
        _ => CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message: "command not supported by this server".to_owned(),
        },
    }
}

/// Bootstrap one Terminal's content stream into its mailbox without touching
/// the caller's subscription; a failure cancels the generation only.
async fn bootstrap_stream(stream: StreamBootstrap<'_>) -> Result<(), AttachResourceFailure> {
    let StreamBootstrap {
        state,
        client_id,
        wire: terminal_id,
        core,
        handle,
        content_tx,
        stream_id,
        client_caps,
        profile: bootstrap_profile,
        limits: bootstrap_limits,
        connection_token,
        ..
    } = stream;
    let Some(bootstrap_profile) = bootstrap_profile else {
        return Err(AttachResourceFailure {
            code: ErrorCode::CodecUnavailable,
            message: NO_TERMINAL_PROFILE.to_owned(),
        });
    };
    let Some(stream_profile) =
        crate::runtime::apps::terminal::attach::bootstrap_stream_profile(bootstrap_profile)
    else {
        return Err(AttachResourceFailure {
            code: ErrorCode::CodecUnavailable,
            message: "ATTACH_RESOURCE selected an unsupported bootstrap profile".to_owned(),
        });
    };

    let Some(bootstrap_id) = state.with_mut(|s| s.next_attach_terminal_bootstrap_id(client_id))
    else {
        return Err(AttachResourceFailure {
            code: ErrorCode::ResourceExhausted,
            message: "ATTACH_RESOURCE bootstrap id space exhausted".to_owned(),
        });
    };

    let terminal = match handle.terminal() {
        Ok(terminal) => terminal.clone(),
        Err(error) => {
            state.with_mut(|s| s.cancel_attach_terminal_pump(client_id, core));
            return Err(AttachResourceFailure {
                code: ErrorCode::WrongResourceKind,
                message: error.to_string(),
            });
        }
    };
    let session = AttachResourceSession {
        state,
        out_tx: content_tx,
        connection_token,
        terminal_id,
        core,
        handle: handle.clone(),
        terminal,
        client_id,
        stream_id,
        client_caps,
        stream_profile,
        bootstrap_limits,
    };

    let mut generation = match session.establish_generation(bootstrap_id).await {
        Ok(generation) => generation,
        Err(failure) => return Err(session.failed(failure)),
    };

    let finished = if let Some(state_sync) = generation.state_sync_bootstrap.take() {
        session.finish_state_sync(&generation, state_sync).await
    } else {
        session.finish_bootstrap(&mut generation).await
    };
    finished.map_err(|failure| session.failed(failure))
}
