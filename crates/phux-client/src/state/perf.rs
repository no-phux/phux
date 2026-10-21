//! `GET_PERF` and `PERF_CONTROL`: server performance telemetry and tracing.

use phux_protocol::wire::frame::{Command, CommandResult, CommandValue};

use crate::attach::AttachError;
use crate::attach::connection::Connection;

/// `GET_PERF`: the server's performance telemetry; `reset` zeroes it after
/// the snapshot. A server that predates it refuses.
pub async fn get_perf_on(
    conn: &mut Connection,
    reset: bool,
) -> Result<phux_perf::PerfReport, AttachError> {
    const REQUEST_ID: u32 = 0;
    let (result, _interleaved) = conn
        .request(REQUEST_ID, Command::GetPerf { reset })
        .await?
        .into_parts();
    match result {
        CommandResult::OkWith(CommandValue::Json(json)) => phux_perf::PerfReport::from_json(&json)
            .map_err(|err| {
                AttachError::Protocol(format!("GET_PERF returned unparseable JSON: {err}"))
            }),
        CommandResult::Error { message, .. } => Err(AttachError::Refused(message)),
        other => Err(AttachError::Protocol(crate::explain::explain_unexpected(
            "GET_PERF", &other,
        ))),
    }
}

/// `PERF_CONTROL`: start or stop a span trace, or switch heap accounting.
/// The reply is the server's JSON object (for a stopped trace, the path it
/// wrote). Send only after seeing `features_ext.PERF_CONTROL`.
///
/// # Errors
///
/// Transport failure, a refusal (no trace recording, one already
/// recording, an unwritable state directory), or an unexpected reply.
pub async fn perf_control_on(
    conn: &mut Connection,
    op: phux_protocol::wire::frame::PerfOp,
) -> Result<serde_json::Value, AttachError> {
    const REQUEST_ID: u32 = 0;
    let (result, _interleaved) = conn
        .request(REQUEST_ID, Command::PerfControl { op })
        .await?
        .into_parts();
    match result {
        CommandResult::OkWith(CommandValue::Json(json)) => {
            serde_json::from_str(&json).map_err(|err| {
                AttachError::Protocol(format!("PERF_CONTROL returned unparseable JSON: {err}"))
            })
        }
        CommandResult::Error { message, .. } => Err(AttachError::Refused(message)),
        other => Err(AttachError::Protocol(crate::explain::explain_unexpected(
            "PERF_CONTROL",
            &other,
        ))),
    }
}
