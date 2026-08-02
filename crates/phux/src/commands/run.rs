use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use phux_client::attach::AttachError;
use phux_client::run::RunOutcome;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target};

/// Default `run` timeout when `--timeout` is unset. Bounds the poll so an
/// interactive or never-returning command does not hang forever; users opt
/// back into unbounded waits with `--timeout 0`.
const RUN_DEFAULT_TIMEOUT_SECS: u64 = 600;

/// Exit code `run` returns when it gives up waiting for the sentinel.
/// Distinct from a mirrored child code (the wrapper-failure convention).
const RUN_TIMEOUT_EXIT_CODE: u8 = 125;

/// `phux run TARGET CMD...` — run a command in a pane and report its exit
/// code, output, and duration (ADR-0022 §3). The process exits with the
/// command's own code, so `phux run … && next` composes like a shell.
///
/// `TARGET` is the full selector grammar (`docs/consumers/tui.md` §3):
/// `session`, `session:window`, `session:window.pane`, `@id`, `.`. It
/// is resolved client-side to a single pane (the selected one — the focused
/// pane when the selector spans several), then the command runs in exactly
/// that pane (phux-n95).
pub(crate) fn run_run(
    target: &str,
    command: &[String],
    timeout: Option<u64>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let selector = match parse_selector(Some(target)) {
        Ok(sel) => sel,
        Err(code) => return code,
    };
    let cmd = command.join(" ");
    // `run` polls until the command's sentinel appears; an interactive or
    // never-returning command would otherwise hang forever. Default to a
    // generous cap; `--timeout 0` opts back into waiting indefinitely.
    let timeout = match timeout {
        None => Some(Duration::from_secs(RUN_DEFAULT_TIMEOUT_SECS)),
        Some(0) => None,
        Some(secs) => Some(Duration::from_secs(secs)),
    };
    let nonce = run_nonce();
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    rt.block_on(async move {
        let pane = match resolve_target(&socket_path, &selector, "run", json).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        match phux_client::run::run_in(&socket_path, pane, &cmd, &nonce, timeout).await {
            Ok(RunOutcome::Completed(result)) => {
                if json {
                    match serde_json::to_string_pretty(&result) {
                        Ok(s) => outln!("{s}"),
                        Err(err) => {
                            eprintln!("phux: failed to serialize run result: {err}");
                            return ExitCode::FAILURE;
                        }
                    }
                } else {
                    print_run_result(&result);
                }
                // Mirror the command's exit code (clamped to the 0..=255
                // process-exit range; negative/large codes saturate to 255).
                ExitCode::from(u8::try_from(result.exit_code).unwrap_or(255))
            }
            Ok(RunOutcome::TimedOut {
                command,
                duration_ms,
                ..
            }) => {
                eprintln!("phux: '{command}' did not finish within {duration_ms}ms");
                // 125, not 124: `run` mirrors the child's code into 0..=255,
                // and 124 is a code real commands (notably GNU `timeout`)
                // produce. 125 is the wrapper-failure convention (env/timeout),
                // so a caller can distinguish "phux gave up" from the child.
                ExitCode::from(RUN_TIMEOUT_EXIT_CODE)
            }
            Err(err @ AttachError::Io(_)) => {
                json_err::report_no_server(json, &err, &socket_path, "run")
            }
            Err(AttachError::Refused(msg)) => {
                eprintln!("phux: cannot run in '{target}': {msg} (try `phux ls`)");
                ExitCode::FAILURE
            }
            Err(err) => {
                eprintln!("phux: run failed: {err}");
                ExitCode::FAILURE
            }
        }
    })
}

/// Human-readable rendering of a `run` result.
pub(crate) fn print_run_result(result: &phux_client::run::RunResult) {
    if !result.output.is_empty() {
        outln!("{}", result.output);
    }
    let trunc = if result.truncated {
        " (output truncated; needs scrollback)"
    } else {
        ""
    };
    outln!(
        "exit={} ({}ms){trunc}",
        result.exit_code,
        result.duration_ms
    );
}

/// A per-invocation sentinel nonce, unique across `run` calls.
///
/// Three components: the pid disambiguates concurrent processes; the
/// epoch-nanos make a residual sentinel from an *earlier* process unable to
/// collide with this one; and a process-global monotonic counter guarantees
/// uniqueness between two calls in the same process even when they fall in a
/// single clock tick (`SystemTime` resolution is coarser than nanoseconds, so
/// back-to-back calls — or an MCP host firing rapid `phux_run`s — could
/// otherwise share a timestamp).
pub(crate) fn run_nonce() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{}x{nanos}x{seq}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::run_nonce;

    #[test]
    fn run_nonce_is_unique_across_invocations() {
        // The pid is stable within a process; the time component must still
        // make two nonces differ (defends the stale-sentinel fix).
        assert_ne!(run_nonce(), run_nonce());
    }
}
