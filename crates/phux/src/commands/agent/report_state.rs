//! Hook-only ingress for detector-owned lifecycle state.

use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;

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
    let refused = Rc::new(std::cell::Cell::new(false));
    let mark_refused = Rc::clone(&refused);
    let result = with_target_pane(
        Some(target),
        socket,
        "agent report-state",
        move |conn, pane| {
            Box::pin(async move {
                let supported = conn.negotiated_bootstrap().is_some_and(|bootstrap| {
                    bootstrap
                        .server_features
                        .contains(ServerFeature::ReportAgentState)
                });
                if !supported {
                    error!("server does not support REPORT_AGENT_STATE; upgrade it");
                    mark_refused.set(true);
                    return Ok(());
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
                    CommandResult::Ok | CommandResult::OkWith(_) => Ok(()),
                    CommandResult::Error { code, message } => {
                        error!("agent state report refused ({code:?}): {message}");
                        mark_refused.set(true);
                        Ok(())
                    }
                    _ => {
                        error!("unexpected REPORT_AGENT_STATE reply");
                        mark_refused.set(true);
                        Ok(())
                    }
                }
            })
        },
    );
    if result == ExitCode::SUCCESS && refused.get() {
        ExitCode::FAILURE
    } else {
        result
    }
}
