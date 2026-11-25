//! Host admission and routing of Terminal facet commands.
pub(crate) mod agent_reports;
pub(crate) mod input_lease;
pub(crate) mod input_route;
pub(crate) mod resize;
pub(crate) mod screen;
pub(crate) mod seed;
pub(crate) mod signal;
pub(crate) mod spawn;
pub(crate) mod terminal_input;
pub(crate) use input_route::*;
pub(crate) use resize::*;
pub(crate) use screen::*;
pub(crate) use spawn::*;
pub(crate) use terminal_input::*;
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode};
use crate::runtime::commands::CommandContext;
use crate::runtime::operation_dedupe::OperationClaim;
use crate::runtime::commands::terminal::agent_reports::{handle_report_agent_state, handle_report_asked};
use crate::runtime::commands::terminal::input_lease::{handle_acquire_input, handle_release_input};


use crate::runtime::commands::terminal::signal::handle_signal_terminal;
/// One flat arm per Terminal facet command keeps the catalog auditable.
#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "one flat dispatch arm per wire command keeps the catalog auditable"
)]
pub(super) async fn serve(
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
            None => CommandResult::error(
                ErrorCode::InternalError,
                "acknowledged input lane unavailable",
            ),
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
            contract,
        } => {
            crate::runtime::apps::terminal::checkout::handle(state, &terminal_id, request, contract)
                .await
        }
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
        _ => CommandResult::error(
            ErrorCode::InvalidCommand,
            "command not supported by this server",
        ),
    }
}
