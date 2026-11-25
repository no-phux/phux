//! `phux seen` — clear a pane's unseen mark on every client
//! (`docs/scripting.md#unseen-marks`).

use std::path::PathBuf;
use std::process::ExitCode;

use super::record::with_targets;
use crate::commands::json_err::{self, CliError, codes};
use crate::commands::warn_interleaved_degradation;
use crate::exit_codes::EXIT_FAILURE;

/// Mark every pane TARGET names seen (one pane, or each pane of a session,
/// group or tag), confirming each with a read-back.
pub(crate) fn run_seen(target: &str, json: bool, socket: Option<PathBuf>) -> ExitCode {
    with_targets(
        Some(target),
        socket,
        "seen",
        None,
        json,
        true,
        move |conn, panes| {
            Box::pin(async move {
                let mut seen = Vec::with_capacity(panes.len());
                let mut request_id = 100_u32;
                for pane in panes {
                    let (cleared, degradation) =
                        phux_client::session_list::unseen::clear(conn, request_id, &pane).await?;
                    request_id = request_id.wrapping_add(2);
                    warn_interleaved_degradation(&degradation);
                    let selector = phux_client::selector::format_terminal_id(&pane);
                    if !cleared {
                        return Ok(json_err::emit(
                            json,
                            &CliError::new(
                                codes::AGENT_RECORD_UNCONFIRMED,
                                format!("{selector} could not be marked seen"),
                                "retry; it may have printed again since",
                            ),
                            EXIT_FAILURE,
                        ));
                    }
                    seen.push(selector);
                }
                if json {
                    return Ok(crate::output::json(&serde_json::json!({
                        "schema_version": 1,
                        "terminals": seen,
                    })));
                }
                Ok(ExitCode::SUCCESS)
            })
        },
    )
}
