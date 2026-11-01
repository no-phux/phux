//! [`TerminalApp`]: the Terminal facet's commands and stream bootstrap.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use phux_terminal::actor::TerminalResource as _;

use super::*;
use crate::resource::ResourceKind;
use crate::runtime::apps::{ResourceApp, StreamBootstrap, StreamFrameContext};
use crate::runtime::connections::dispatch::FrameFlow;
use crate::runtime::operation_dedupe::OperationClaim;

/// Serves [`ResourceKind::Terminal`].
#[derive(Debug)]
pub(crate) struct TerminalApp;

impl ResourceApp for TerminalApp {
    fn kind(&self) -> ResourceKind {
        ResourceKind::Terminal
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

    fn stream_frame<'a>(
        &'a self,
        ctx: StreamFrameContext<'a>,
        frame: FrameKind,
    ) -> LocalBoxFuture<'a, FrameFlow> {
        crate::runtime::apps::terminal::frames::serve(ctx, frame).boxed_local()
    }

    fn bootstrap<'a>(
        &'a self,
        stream: StreamBootstrap<'a>,
    ) -> LocalBoxFuture<'a, Result<(), AttachResourceFailure>> {
        bootstrap_stream(stream).boxed_local()
    }
}

/// Free a client's native cut and consumer entry without blocking: a full
/// mailbox is reaped by `tick_emit` when the client's mailbox closes.
fn release_terminal_consumer(handle: &ResourceHandle, client_id: ClientId) {
    let Ok(terminal) = handle.terminal() else {
        return;
    };
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    let _ = terminal
        .native_release
        .try_send(phux_terminal::actor::NativeReleaseRequest { owner: client_id.0 });
    let (reply_tx, _reply_rx) = oneshot::channel();
    match terminal
        .consumer_detach
        .try_send(phux_terminal::actor::ConsumerDetachRequest {
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
    let Ok(terminal) = handle.terminal() else {
        return;
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    let _ = terminal
        .consumer_detach
        .send(phux_terminal::actor::ConsumerDetachRequest {
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
    let input_lane = ctx.input_lane.as_ref();

    match command {
        Command::Transcribe {
            upload_id,
            terminal_id,
        } => {
            crate::runtime::voice::handle_transcribe(
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
                match crate::runtime::input_lane::acquire_credit(state, &terminal_id).await {
                    Ok(credit) => {
                        lane.begin_route(state, client_id, terminal_id, event, credit)
                            .await
                    }
                    Err(crate::runtime::input_lane::InputStalled) => {
                        crate::runtime::input_lane::InputStalled::result(&terminal_id)
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
            crate::runtime::upload::handle_put_file(
                state,
                crate::runtime::upload::PutFileChunk {
                    upload_id,
                    terminal_id,
                    extension,
                    offset,
                    data,
                    final_chunk,
                    sha256,
                    principal: state
                        .with(|s| crate::runtime::upload::upload_principal(s, client_id)),
                },
            )
            .await
        }
        Command::Checkout {
            terminal_id,
            request,
        } => crate::runtime::checkout::handle(state, &terminal_id, request).await,
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
    let Some(stream_profile) = crate::runtime::attach::bootstrap_stream_profile(bootstrap_profile)
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
