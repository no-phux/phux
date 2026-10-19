//! `PERF_CONTROL`: start and stop a span trace, switch heap accounting.
//!
//! A stopped trace is written as Chrome trace-event JSON under
//! `<state dir>/traces/`, the directory the server log lives in, and the
//! reply names the file: a trace can run to tens of megabytes, far past
//! what one frame carries.

use std::path::{Path, PathBuf};

use phux_perf::{alloc, trace};
use phux_protocol::wire::frame::{CommandResult, CommandValue, ErrorCode, PerfOp};
use serde_json::json;

/// The process track name in a server trace.
const PROCESS_NAME: &str = "phux server";

/// Handle one `PERF_CONTROL`.
pub(super) async fn handle(op: PerfOp) -> CommandResult {
    match op {
        PerfOp::TraceStart => trace_start(),
        PerfOp::TraceStop => trace_stop().await,
        PerfOp::AllocOn => alloc_switch(true),
        PerfOp::AllocOff => alloc_switch(false),
        PerfOp::Unknown(value) => refuse(
            ErrorCode::InvalidCommand,
            format!("PERF_CONTROL: unknown op {value:#04x}"),
        ),
    }
}

fn refuse(code: ErrorCode, message: impl Into<String>) -> CommandResult {
    CommandResult::Error {
        code,
        message: message.into(),
    }
}

fn ok_json(value: &serde_json::Value) -> CommandResult {
    CommandResult::OkWith(CommandValue::Json(value.to_string()))
}

fn trace_start() -> CommandResult {
    if !trace::start(trace::DEFAULT_MAX_EVENTS) {
        return refuse(
            ErrorCode::InvalidCommand,
            "PERF_CONTROL: a trace is already recording",
        );
    }
    tracing::info!("perf trace started");
    ok_json(&json!({ "tracing": true }))
}

async fn trace_stop() -> CommandResult {
    let Some(capture) = trace::stop() else {
        return refuse(
            ErrorCode::InvalidCommand,
            "PERF_CONTROL: no trace is recording",
        );
    };
    // Serializing and writing a full capture takes long enough to stall the
    // connection's runtime thread; do it off it.
    let written = tokio::task::spawn_blocking(move || write_capture(&capture)).await;
    match written {
        Ok(Ok(reply)) => ok_json(&reply),
        Ok(Err(message)) => {
            tracing::warn!(%message, "perf trace not written");
            refuse(ErrorCode::InternalError, format!("PERF_CONTROL: {message}"))
        }
        Err(err) => refuse(
            ErrorCode::InternalError,
            format!("PERF_CONTROL: trace writer failed: {err}"),
        ),
    }
}

/// Write `capture` to a fresh file under the traces directory and describe it.
fn write_capture(capture: &trace::Capture) -> Result<serde_json::Value, String> {
    let path = trace_path(&traces_dir());
    phux_config::production::refuse_dev_on_production_state(&path)?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)
        .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let body = capture.to_chrome_json(PROCESS_NAME, &[crate::perf::TABLE]);
    std::fs::write(&path, body).map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    tracing::info!(path = %path.display(), events = capture.event_count(), "perf trace written");
    Ok(json!({
        "path": path.display().to_string(),
        "events": capture.event_count(),
        "dropped": capture.dropped(),
        "duration_ms": u64::try_from(capture.duration().as_millis()).unwrap_or(u64::MAX),
    }))
}

/// `<state dir>/traces`, beside the server log.
fn traces_dir() -> PathBuf {
    crate::telemetry::state_dir().join("traces")
}

/// `server-<unix ms>-<pid>.json` under `dir`.
fn trace_path(dir: &Path) -> PathBuf {
    let unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    dir.join(format!("server-{unix_ms}-{}.json", std::process::id()))
}

fn alloc_switch(on: bool) -> CommandResult {
    if on {
        alloc::enable();
    } else {
        alloc::disable();
    }
    ok_json(&json!({ "alloc": alloc::enabled(), "installed": alloc::installed() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_path_names_the_process_and_lands_in_the_dir() {
        let path = trace_path(Path::new("/x/traces"));
        assert_eq!(path.parent(), Some(Path::new("/x/traces")));
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        assert!(name.starts_with("server-"), "{name}");
        assert!(
            name.ends_with(&format!("-{}.json", std::process::id())),
            "{name}"
        );
    }
}
