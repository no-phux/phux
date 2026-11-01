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
        // The decoder refuses an undefined op first; one built in process
        // gets the same answer.
        PerfOp::Unknown(value) => refuse(
            ErrorCode::Unsupported,
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

/// A trace nobody stops (the CLI was killed, the connection dropped) is
/// stopped and saved after this long, so it cannot hold its buffers and
/// refuse every later trace until the server restarts.
pub const TRACE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(600);

/// Trace files kept in the traces directory; older ones are removed.
const KEEP_TRACES: usize = 20;

fn trace_start() -> CommandResult {
    if !trace::start(trace::DEFAULT_MAX_EVENTS) {
        return refuse(
            ErrorCode::InvalidCommand,
            "PERF_CONTROL: a trace is already recording",
        );
    }
    let generation = trace::generation();
    tokio::spawn(async move {
        tokio::time::sleep(TRACE_DEADLINE).await;
        let Some(capture) = trace::stop_generation(generation) else {
            return;
        };
        match tokio::task::spawn_blocking(move || write_capture(&capture)).await {
            Ok(Ok(reply)) => tracing::warn!(
                path = %reply["path"],
                "perf trace was never stopped; stopped it at the deadline and saved it"
            ),
            Ok(Err(message)) => tracing::warn!(%message, "abandoned perf trace not written"),
            Err(err) => tracing::warn!(%err, "abandoned perf trace writer failed"),
        }
    });
    tracing::info!("perf trace started");
    ok_json(&json!({ "tracing": true, "deadline_secs": TRACE_DEADLINE.as_secs() }))
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
    static WRITER: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _writer = WRITER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = traces_dir();
    phux_config::production::refuse_dev_on_production_state(&dir)?;
    std::fs::create_dir_all(&dir)
        .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    prune_traces(&dir, KEEP_TRACES.saturating_sub(1));
    let (path, file) = trace::create_trace_file(&dir, "server")
        .map_err(|err| format!("cannot create trace in {}: {err}", dir.display()))?;
    capture
        .write_chrome_json(
            std::io::BufWriter::new(file),
            PROCESS_NAME,
            &crate::perf::tables(),
        )
        .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    tracing::info!(path = %path.display(), events = capture.event_count(), "perf trace written");
    Ok(json!({
        "path": path.display().to_string(),
        "events": capture.event_count(),
        "dropped": capture.dropped(),
        "duration_ms": u64::try_from(capture.duration().as_millis()).unwrap_or(u64::MAX),
    }))
}

/// Keep only the newest `keep` server traces in `dir`, by name (the names
/// start with a timestamp). Failures are ignored: retention is
/// housekeeping, never a reason to lose the trace being written.
fn prune_traces(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut traces: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "json")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("server-"))
        })
        .collect();
    traces.sort();
    let excess = traces.len().saturating_sub(keep);
    for old in traces.into_iter().take(excess) {
        let _ = std::fs::remove_file(old);
    }
}

/// `<state dir>/traces`, beside the server log.
fn traces_dir() -> PathBuf {
    crate::telemetry::state_dir().join("traces")
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
    fn retention_keeps_the_newest_server_traces_only() {
        let dir = std::env::temp_dir().join(format!("phux-trace-prune-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
        for name in [
            "server-100-1.json",
            "server-200-1.json",
            "server-300-1.json",
            "other.json",
        ] {
            std::fs::write(dir.join(name), "{}").unwrap_or_else(|e| panic!("{e}"));
        }
        prune_traces(&dir, 2);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{e}"))
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            ["other.json", "server-200-1.json", "server-300-1.json"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
