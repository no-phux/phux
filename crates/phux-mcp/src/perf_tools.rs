//! The performance tools `phux_perf` and `phux_perf_trace`: thin transports
//! for `phux perf --json` and `phux perf trace --json`, so the report, the
//! trace capture, and the dev-mode desktop's socket discovery each have one
//! implementation, in the CLI.
//!
//! The argument picking the process is `source`, not `target`: across this
//! catalog `target` is a resource selector and gains a `resource_id` alias
//! (`contracts::enhance`), which a server-or-desktop switch must not.
//!
//! `phux_perf` takes no `reset`: `GET_PERF { reset: true }` needs `BIND`,
//! and a reset would make the tool a write. Zeroing the metrics is left to
//! the operator's `phux perf --reset`; an agent reads an interval by calling
//! twice and subtracting.

#![allow(
    clippy::similar_names,
    reason = "argv and parsed args are deliberately adjacent in thin CLI wrappers"
)]

use std::time::Duration;

use serde_json::{Value, json};

use crate::cli_adapter::{CliAdapter, DEFAULT_CALL_TIMEOUT, enum_string, push_socket};
use crate::cli_tools::schema;
use crate::tools::{ToolError, strict_object};

/// The processes either tool can read; the first is the default.
const SOURCES: [&str; 2] = ["server", "desktop"];

/// The longest trace an agent may ask for, in seconds.
const MAX_TRACE_SECS: f64 = 60.0;

/// The trace length when `seconds` is absent.
const DEFAULT_TRACE_SECS: f64 = 5.0;

/// Slack past the recording itself for connecting, stopping the capture,
/// and writing the file.
const TRACE_SLACK: Duration = Duration::from_secs(60);

/// Shared `socket` description.
const SOCKET_DESC: &str = "Override the UDS path of the server. Defaults to PHUX_SOCKET or the \
    daemon default. With `source: desktop` the dev-mode desktop is found beside this socket.";

/// Every schema in this family, in catalog order.
#[must_use]
pub(crate) fn schemas() -> Vec<Value> {
    vec![perf_schema(), trace_schema()]
}

/// Whether `name` belongs to this family (used by `tools/call` dispatch).
#[must_use]
pub(crate) fn owns(name: &str) -> bool {
    matches!(name, "phux_perf" | "phux_perf_trace")
}

/// Dispatch one performance call.
///
/// # Errors
///
/// Returns [`ToolError`] for an unknown name, a malformed argument, or a
/// canonical-CLI failure.
pub(crate) async fn call(name: &str, args: &Value) -> Result<Value, ToolError> {
    call_with_adapter(name, args, &CliAdapter::for_residue(name)?).await
}

async fn call_with_adapter(
    name: &str,
    args: &Value,
    adapter: &CliAdapter,
) -> Result<Value, ToolError> {
    match name {
        "phux_perf" => run_perf(args, adapter).await,
        "phux_perf_trace" => run_trace(args, adapter).await,
        other => Err(ToolError::new(format!("unknown perf tool: {other}"))),
    }
}

fn perf_schema() -> Value {
    schema(
        "phux_perf",
        "Read the performance telemetry phux keeps about itself: the same document as \
         `phux perf --json`. READ-ONLY: it changes no metric and never starts a server. \
         `source: server` (default) returns the server's report: `{schema_version, role, pid, \
         captured_unix_ms, uptime_ms, process, metrics, rss_bytes?, alloc?, \
         stream_diagnostics?}`. `process` is {cpu_user_us, cpu_system_us, max_rss_bytes, \
         voluntary_ctx_switches, involuntary_ctx_switches} or null; `rss_bytes` is the current \
         resident set. `metrics` is `[{name, unit, kind, value}]`: `kind` is `histogram`, \
         `counter`, or `gauge`, and `unit` (`micros`, `bytes`, `count`) is the unit of every \
         number in `value`. A histogram `value` is `{count, sum, min, max, buckets: [{idx, \
         count}]}` — latencies in MICROSECONDS, sizes in bytes; the mean is sum/count, and the \
         bucket indices are an internal log layout, so do not compute percentiles from them: \
         the human `phux perf` table shows p50/p90/p99. Names follow the pipeline: `pty.*` \
         (child output), `echo.server` (input to first output), `tick.*` and `pump.*` (fan-out \
         to clients), `wire.*` (socket writes), `cmd.*` and `attach.*` (control plane), \
         `consumer.*` (per-client backpressure). \
         HEAP: `alloc` {allocs, alloc_bytes, frees, live_bytes, peak_bytes} appears only when \
         heap accounting is on — a process started with PHUX_DEV=1, or after the operator ran \
         `phux perf alloc on`; its absence is not an error. \
         `source: desktop` reads a phux desktop running in dev mode (started with PHUX_DEV=1) \
         and returns `{report, frames}`: `report` is the desktop's own report in the same shape \
         (`desktop.*` metrics), and `frames` is {fps, frames, build_p50_us, build_p99_us, \
         build_max_us, over_budget, janks, worst_gap_us} over the recent frame window. With no \
         dev-mode desktop the call fails with `no_desktop`; `desktop_unreachable`, \
         `desktop_refused`, and `desktop_protocol` are the other refusals. \
         Figures are cumulative since start (counters and histograms only grow). FOR AN \
         INTERVAL, CALL TWICE AND SUBTRACT: counts, sums, and bucket counts diff cleanly; \
         min and max do not. Use phux_perf_trace for a timeline of individual spans.",
        source_properties(json!({})),
        &[],
    )
}

fn trace_schema() -> Value {
    schema(
        "phux_perf_trace",
        "Record a Perfetto trace for `seconds` and return where it was written: the same \
         document as `phux perf trace --json`. The call blocks for the whole recording. \
         `source: server` (default) traces the server and returns `{path, events, dropped, \
         duration_ms}`; `source: desktop` traces a dev-mode desktop (started with PHUX_DEV=1) \
         and the server onto ONE merged timeline and returns `{ok, path, desktop, server, \
         server_error, events, dropped, duration_ms}` — a non-null `server_error` means the \
         server did not take part and the file holds only the desktop. `dropped` > 0 means the \
         capture buffer overflowed and the trace is missing spans. \
         `path` IS ON THE HOST RUNNING PHUX, not necessarily yours: open it in \
         https://ui.perfetto.dev or read it as JSON. The file is Chrome trace-event JSON: `X` \
         events are spans with `name`, `cat`, `ts` and `dur` in MICROSECONDS; span names are \
         metric names such as `cmd.handle`, `desktop.frame`, `desktop.paint`. `C` events are \
         counter tracks (the desktop's `memory.rss_bytes`, `memory.heap_live_bytes`, \
         `frames.fps`). Recording arms the server's span capture for the duration, so it is \
         not read-only; it ends no process and changes no session. Use phux_perf for \
         cumulative histograms instead of a timeline.",
        source_properties(json!({
            "seconds": {
                "type": "number",
                "exclusiveMinimum": 0,
                "maximum": MAX_TRACE_SECS,
                "description": "How long to record. Default 5; more than 0 and at most 60.",
            },
        })),
        &[],
    )
}

/// `extra` plus the shared `source` and `socket` properties.
fn source_properties(mut extra: Value) -> Value {
    extra["source"] = json!({
        "type": "string",
        "enum": SOURCES,
        "description": "Which process to read: `server` (default) or a dev-mode `desktop`.",
    });
    extra["socket"] =
        json!({ "type": "string", "minLength": 1, "maxLength": 4096, "description": SOCKET_DESC });
    extra
}

/// `--desktop` when `source` asks for the desktop.
fn push_source(argv: &mut Vec<String>, args: &Value) -> Result<(), ToolError> {
    if enum_string(args, "source", &SOURCES, Some(SOURCES[0]))? == "desktop" {
        argv.push("--desktop".to_owned());
    }
    Ok(())
}

/// The validated `seconds`, its default when absent.
fn trace_seconds(args: &Value) -> Result<f64, ToolError> {
    let Some(value) = args.get("seconds") else {
        return Ok(DEFAULT_TRACE_SECS);
    };
    value
        .as_f64()
        .filter(|seconds| *seconds > 0.0 && *seconds <= MAX_TRACE_SECS)
        .ok_or_else(|| ToolError::new("`seconds` must be a number greater than 0 and at most 60"))
}

/// Execute `phux perf --json [--desktop]`.
async fn run_perf(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(args, &["source", "socket"], &[])?;
    let mut argv = vec!["perf".to_owned(), "--json".to_owned()];
    push_source(&mut argv, args)?;
    push_socket(&mut argv, args)?;
    adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
}

/// Execute `phux perf trace --seconds S --json [--desktop]`, allowing the
/// recording plus [`TRACE_SLACK`].
async fn run_trace(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(args, &["seconds", "source", "socket"], &[])?;
    let seconds = trace_seconds(args)?;
    let mut argv = vec![
        "perf".to_owned(),
        "trace".to_owned(),
        "--seconds".to_owned(),
        seconds.to_string(),
        "--json".to_owned(),
    ];
    push_source(&mut argv, args)?;
    push_socket(&mut argv, args)?;
    adapter
        .run_json(argv, Duration::from_secs_f64(seconds) + TRACE_SLACK)
        .await
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;
    use crate::cli_adapter::fake;

    /// A fake `phux` that prints `stdout` and `stderr` verbatim and exits
    /// `code`.
    fn scripted_cli(stdout: &str, stderr: &str, code: i32) -> (TempDir, CliAdapter, PathBuf) {
        let made = fake::cli(&format!(
            "cat '{{dir}}/stdout'\ncat '{{dir}}/stderr' >&2\nexit {code}\n"
        ));
        std::fs::write(made.0.path().join("stdout"), stdout).unwrap();
        std::fs::write(made.0.path().join("stderr"), stderr).unwrap();
        made
    }

    fn server_report() -> Value {
        json!({
            "schema_version": 1,
            "role": "server",
            "pid": 42,
            "captured_unix_ms": 1_700_000_000_000_u64,
            "uptime_ms": 5000,
            "process": {
                "cpu_user_us": 10, "cpu_system_us": 5, "max_rss_bytes": 4096,
                "voluntary_ctx_switches": 1, "involuntary_ctx_switches": 0,
            },
            "metrics": [
                { "name": "cmd.handle", "unit": "micros", "kind": "histogram",
                  "value": { "count": 2, "sum": 30, "min": 10, "max": 20,
                             "buckets": [{ "idx": 3, "count": 2 }] } },
                { "name": "input.events", "unit": "count", "kind": "counter", "value": 7 },
            ],
            "rss_bytes": 4096,
            "alloc": { "allocs": 3, "alloc_bytes": 96, "frees": 1, "live_bytes": 64,
                       "peak_bytes": 96 },
        })
    }

    async fn assert_argv(
        adapter: &CliAdapter,
        log: &Path,
        name: &str,
        args: Value,
        expected: &[&str],
    ) -> Value {
        let result = call_with_adapter(name, &args, adapter)
            .await
            .unwrap_or_else(|err| panic!("{name} {args} failed: {err:?}"));
        assert_eq!(fake::logged(log), expected, "{name} {args}");
        result
    }

    /// `phux_perf` runs the canonical argv for each source and returns the
    /// report unchanged, and the document satisfies the published schema.
    #[tokio::test]
    async fn perf_executes_canonical_argv_for_each_source() {
        let report = server_report();
        let (_temp, adapter, log) = scripted_cli(&format!("{report}\n"), "", 0);
        for (args, expected) in [
            (json!({}), &["perf", "--json"][..]),
            (json!({ "source": "server" }), &["perf", "--json"][..]),
            (
                json!({ "source": "desktop", "socket": "/sock" }),
                &["perf", "--json", "--desktop", "--socket", "/sock"][..],
            ),
        ] {
            let result = assert_argv(&adapter, &log, "phux_perf", args, expected).await;
            assert_eq!(result, report, "the report passes through unchanged");
            crate::contract_tests::validate_output("phux_perf", &result);
        }

        let desktop = json!({
            "report": report,
            "frames": { "fps": 60.0, "frames": 240, "build_p50_us": 900, "build_p99_us": 4000,
                        "build_max_us": 9000, "over_budget": 1, "janks": 0, "worst_gap_us": 17000 },
        });
        crate::contract_tests::validate_output("phux_perf", &desktop);
    }

    /// `phux_perf_trace` always names `--seconds`, defaulting to 5, and
    /// `source: desktop` asks for the merged timeline.
    #[tokio::test]
    async fn trace_executes_canonical_argv_for_each_source() {
        let server = json!({ "path": "/state/trace.json", "events": 12, "dropped": 0,
                             "duration_ms": 5001 });
        let (_temp, adapter, log) = scripted_cli(&format!("{server}\n"), "", 0);
        for (args, expected) in [
            (
                json!({}),
                &["perf", "trace", "--seconds", "5", "--json"][..],
            ),
            (
                json!({ "seconds": 0.5, "socket": "/sock" }),
                &[
                    "perf",
                    "trace",
                    "--seconds",
                    "0.5",
                    "--json",
                    "--socket",
                    "/sock",
                ][..],
            ),
            (
                json!({ "seconds": 60, "source": "desktop" }),
                &["perf", "trace", "--seconds", "60", "--json", "--desktop"][..],
            ),
        ] {
            let result = assert_argv(&adapter, &log, "phux_perf_trace", args, expected).await;
            assert_eq!(result, server);
            crate::contract_tests::validate_output("phux_perf_trace", &result);
        }

        let merged = json!({ "ok": true, "path": "/state/merged.json", "desktop": 30,
                             "server": 12, "server_error": null, "events": 42, "dropped": 0,
                             "duration_ms": 5002 });
        crate::contract_tests::validate_output("phux_perf_trace", &merged);
    }

    /// A desktop refusal arrives as the CLI's stderr error contract.
    #[tokio::test]
    async fn a_missing_desktop_reports_the_cli_error_contract() {
        let contract = r#"{"schema_version":1,"error":{"code":"no_desktop","message":"no phux desktop in dev mode is serving /run"}}"#;
        let (_temp, adapter, _log) = scripted_cli("", &format!("{contract}\n"), 1);
        for name in ["phux_perf", "phux_perf_trace"] {
            let err = call_with_adapter(name, &json!({ "source": "desktop" }), &adapter)
                .await
                .expect_err("a missing desktop is refused");
            assert!(err.0.contains("no_desktop"), "{name}: {err:?}");
        }
    }

    /// Validation happens before any subprocess: an adapter pointed at a
    /// program that cannot exist proves nothing was executed.
    #[tokio::test]
    async fn malformed_arguments_are_rejected_before_execution() {
        let adapter = CliAdapter::new("must-not-execute");
        for (name, args) in [
            // Resetting would need BIND; the tool stays read-only.
            ("phux_perf", json!({ "reset": true })),
            ("phux_perf", json!({ "target": "desktop" })),
            ("phux_perf", json!({ "source": "tui" })),
            ("phux_perf", json!({ "source": 1 })),
            ("phux_perf", json!({ "socket": "" })),
            ("phux_perf", json!({ "seconds": 5 })),
            ("phux_perf_trace", json!({ "seconds": 0 })),
            ("phux_perf_trace", json!({ "seconds": -1 })),
            ("phux_perf_trace", json!({ "seconds": 60.5 })),
            ("phux_perf_trace", json!({ "seconds": "5" })),
            ("phux_perf_trace", json!({ "out": "/tmp/t.json" })),
            ("phux_perf_trace", json!({ "source": "both" })),
            ("phux_perf_flame", json!({})),
        ] {
            assert!(
                call_with_adapter(name, &args, &adapter).await.is_err(),
                "{name} accepted {args}",
            );
        }
    }

    /// The descriptions carry the rules an agent needs to read the numbers.
    #[test]
    fn the_descriptions_teach_units_heap_and_intervals() {
        let perf = perf_schema();
        let perf = perf["description"].as_str().unwrap();
        for needle in [
            "MICROSECONDS",
            "sum/count",
            "p50/p90/p99",
            "PHUX_DEV=1",
            "phux perf alloc on",
            "CALL TWICE AND SUBTRACT",
            "`no_desktop`",
            "READ-ONLY",
        ] {
            assert!(perf.contains(needle), "phux_perf lacks {needle}: {perf}");
        }
        let trace = trace_schema();
        let trace = trace["description"].as_str().unwrap();
        for needle in [
            "ON THE HOST RUNNING PHUX",
            "ui.perfetto.dev",
            "`cmd.handle`",
            "`memory.rss_bytes`",
            "MICROSECONDS",
            "merged timeline",
        ] {
            assert!(
                trace.contains(needle),
                "phux_perf_trace lacks {needle}: {trace}"
            );
        }
    }
}
