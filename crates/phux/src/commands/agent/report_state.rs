//! Hook-only ingress for detector-owned lifecycle state.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_protocol::caps::ServerFeature;
use phux_protocol::wire::frame::{Command, CommandResult, ReportedAgentState};

use super::record::with_target_pane;

pub(super) fn run(target: &str, state: &str, socket: Option<PathBuf>) -> ExitCode {
    let state = match state {
        "working" => ReportedAgentState::Working,
        "blocked" => ReportedAgentState::Blocked,
        "done" => ReportedAgentState::Done,
        _ => return ExitCode::FAILURE,
    };
    with_target_pane(
        Some(target),
        socket,
        "agent report-state",
        false,
        move |conn, pane| {
            Box::pin(async move {
                let supported = conn.negotiated_bootstrap().is_some_and(|bootstrap| {
                    bootstrap
                        .server_features
                        .contains(ServerFeature::ReportAgentState)
                });
                if !supported {
                    error!("server does not support REPORT_AGENT_STATE; upgrade it");
                    return Ok(ExitCode::FAILURE);
                }
                let reply = conn
                    .request(
                        100,
                        Command::ReportAgentState {
                            terminal_id: pane,
                            state,
                        },
                    )
                    .await?
                    .into_result_ignoring_interleaved();
                match reply {
                    CommandResult::Ok | CommandResult::OkWith(_) => Ok(ExitCode::SUCCESS),
                    CommandResult::Error { code, message } => {
                        error!("agent state report refused ({code:?}): {message}");
                        Ok(ExitCode::FAILURE)
                    }
                    _ => {
                        error!("unexpected REPORT_AGENT_STATE reply");
                        Ok(ExitCode::FAILURE)
                    }
                }
            })
        },
    )
}
