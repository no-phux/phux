//! `phux perf` — the server's performance telemetry, live.
//!
//! One `GET_PERF` per sample. Without `--watch` the report is a lifetime
//! view since the server started (or since the last `--reset`). With
//! `--watch` the verb polls and prints each interval as a delta of two
//! reports, so counters become rates and every histogram covers only that
//! window: a stall shows up in the interval it happened in instead of being
//! averaged away by hours of quiet.
//!
//! `phux perf trace` and `phux perf alloc` drive `PERF_CONTROL`: a span
//! trace written as Chrome trace-event JSON, and the heap-accounting switch.
//! See `docs/development.md` §"Performance" and `docs/troubleshooting.md`
//! §"Lag and crash evidence" for what the rows mean and how to use them.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_perf::PerfReport;
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::wire::frame::PerfOp;
use phux_server::runtime::default_socket_path;
use usage::{Subcommands, ValueEnum};

use crate::commands::json_err::Refusal;
use crate::commands::{JsonOpt, cli_runtime, json_err};

/// Subcommands of `phux perf`. Absent, the verb prints the telemetry report.
#[derive(Debug, Subcommands)]
pub(crate) enum PerfAction {
    /// Record a span trace of the server and save it as Chrome trace JSON
    ///
    /// Arms the server's span capture, waits, then stops it. The server
    /// writes the trace under its state directory, beside the server log,
    /// and this prints the path; open it in <https://ui.perfetto.dev>.
    /// Ctrl-C ends the wait early and still saves the trace.
    Trace {
        /// Seconds to record.
        #[usage(long, value_name = "SECS", default = "5", default_value_t = 5.0)]
        seconds: f64,
        /// Copy the trace here as well.
        #[usage(short = 'o', long = "out", value_name = "PATH")]
        out: Option<PathBuf>,
        /// Trace the dev-mode desktop and the server onto one timeline.
        #[usage(long)]
        desktop: bool,
        #[usage(flatten)]
        json: JsonOpt,
    },
    /// Switch the server's heap accounting on or off
    ///
    /// Heap figures appear in `phux perf` while accounting is on. A server
    /// started with `PHUX_DEV=1` starts with it on. A server binary without
    /// the counting allocator answers `installed: false` and reports no heap.
    Alloc {
        /// `on` or `off`.
        #[usage(value_enum)]
        state: AllocSwitch,
        /// Switch the dev-mode desktop's heap accounting instead.
        #[usage(long)]
        desktop: bool,
        #[usage(flatten)]
        json: JsonOpt,
    },
    /// Show or hide the dev-mode desktop's performance HUD
    ///
    /// The HUD sits in each window's corner: frame rate, frame build time,
    /// jank, CPU, memory, heap, and the server's CPU and memory.
    Hud {
        /// `on` or `off`.
        #[usage(value_enum)]
        state: AllocSwitch,
        #[usage(flatten)]
        json: JsonOpt,
    },
}

/// The heap-accounting switch position for `phux perf alloc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum AllocSwitch {
    On,
    Off,
}

/// Options for `phux perf`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PerfOptions {
    /// Print JSON instead of the table.
    pub(crate) json: bool,
    /// Read the dev-mode desktop instead of the server.
    pub(crate) desktop: bool,
    /// Poll every this many seconds and print interval deltas.
    pub(crate) watch: Option<f64>,
    /// Zero the server's metrics after each snapshot.
    pub(crate) reset: bool,
}

/// Shortest poll interval `--watch` will honour.
pub(crate) const MIN_WATCH: Duration = Duration::from_millis(100);

/// Longest trace `phux perf trace` will ask for: the server stops any trace
/// at its own deadline, so asking for more would only lose the stop.
const MAX_TRACE: Duration = phux_server::runtime::perf_control::TRACE_DEADLINE;

/// What one `phux perf` invocation does once connected.
enum Plan {
    Report(PerfOptions),
    Trace {
        span: Duration,
        out: Option<PathBuf>,
    },
    Alloc(AllocSwitch),
}

pub(crate) fn run_perf(
    opts: PerfOptions,
    action: Option<PerfAction>,
    socket: Option<PathBuf>,
) -> ExitCode {
    use crate::commands::perf_desktop::{self, Ask};

    let socket_path = socket.unwrap_or_else(default_socket_path);
    let (json, plan) = match action {
        None if opts.desktop => {
            return perf_desktop::run(opts.json, Ask::Report(opts), &socket_path);
        }
        None => (opts.json, Plan::Report(opts)),
        Some(PerfAction::Trace {
            seconds,
            out,
            desktop,
            json,
        }) if desktop || opts.desktop => {
            return perf_desktop::run(json.json, Ask::Trace { seconds, out }, &socket_path);
        }
        Some(PerfAction::Trace {
            seconds, out, json, ..
        }) => match trace_span(seconds) {
            Ok(span) => (json.json, Plan::Trace { span, out }),
            Err(refusal) => return refusal.emit(json.json),
        },
        Some(PerfAction::Alloc {
            state,
            desktop,
            json,
        }) if desktop || opts.desktop => {
            return perf_desktop::run(json.json, Ask::Alloc(state), &socket_path);
        }
        Some(PerfAction::Alloc { state, json, .. }) => (json.json, Plan::Alloc(state)),
        Some(PerfAction::Hud { state, json }) => {
            return perf_desktop::run(json.json, Ask::Hud(state), &socket_path);
        }
    };
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    match rt.block_on(run(json, plan, &socket_path)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => json_err::report_no_server(json, &err, &socket_path, "perf"),
    }
}

async fn run(json: bool, plan: Plan, socket_path: &Path) -> Result<(), AttachError> {
    let mut conn = Connection::connect(socket_path).await?;
    match plan {
        Plan::Report(opts) => report(opts, &mut conn).await,
        Plan::Trace { span, out } => {
            require_perf_control(&mut conn).await?;
            trace(json, span, out.as_deref(), &mut conn).await
        }
        Plan::Alloc(state) => {
            require_perf_control(&mut conn).await?;
            alloc(json, state, &mut conn).await
        }
    }
}

/// `PERF_CONTROL` is gated on `features_ext.PERF_CONTROL`: an older server
/// would fail to decode the tag and drop the connection.
async fn require_perf_control(conn: &mut Connection) -> Result<(), AttachError> {
    let features = phux_client::state::probe_hello_features_ext(conn).await?;
    if features.contains(ServerFeatureExt::PerfControl) {
        return Ok(());
    }
    Err(AttachError::Refused(
        "this server predates PERF_CONTROL; upgrade it with `phux upgrade` and try again"
            .to_owned(),
    ))
}

/// `--seconds` as a trace length, checked before anything connects so a bad
/// value reads as a bad argument, not as a server problem.
fn trace_span(seconds: f64) -> Result<Duration, Refusal> {
    Duration::try_from_secs_f64(seconds)
        .ok()
        .filter(|span| !span.is_zero() && *span <= MAX_TRACE)
        .ok_or_else(|| {
            Refusal::new(
                "invalid_argument",
                format!("--seconds {seconds} is not a usable duration"),
                format!(
                    "give more than 0 and at most {} seconds; the server stops any trace there",
                    MAX_TRACE.as_secs()
                ),
                2,
            )
        })
}

async fn trace(
    json: bool,
    span: Duration,
    out: Option<&Path>,
    conn: &mut Connection,
) -> Result<(), AttachError> {
    phux_client::state::perf_control_on(conn, PerfOp::TraceStart).await?;
    if !json {
        info!(
            "recording a server trace for {}s (Ctrl-C stops early)",
            span.as_secs_f64()
        );
    }
    tokio::select! {
        () = tokio::time::sleep(span) => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    let mut reply = phux_client::state::perf_control_on(conn, PerfOp::TraceStop).await?;
    if let Some(dest) = out {
        copy_trace(&reply, dest)?;
        reply["out"] = serde_json::Value::String(dest.display().to_string());
    }
    if json {
        outln!("{reply}");
        return Ok(());
    }
    let path = reply["out"].as_str().or_else(|| reply["path"].as_str());
    outln!("{}", path.unwrap_or_default());
    info!(
        "{} events, {} dropped; open it in https://ui.perfetto.dev",
        reply["events"], reply["dropped"]
    );
    Ok(())
}

/// Copy the server's trace file to `dest`. The server shares this host, so
/// the path it named is readable here.
fn copy_trace(reply: &serde_json::Value, dest: &Path) -> Result<(), AttachError> {
    let Some(src) = reply["path"].as_str() else {
        return Err(AttachError::Protocol(
            "PERF_CONTROL named no trace path".to_owned(),
        ));
    };
    std::fs::copy(src, dest).map(drop).map_err(|err| {
        AttachError::Refused(format!(
            "cannot copy the trace {src} to {}: {err}",
            dest.display()
        ))
    })
}

async fn alloc(json: bool, state: AllocSwitch, conn: &mut Connection) -> Result<(), AttachError> {
    let op = match state {
        AllocSwitch::On => PerfOp::AllocOn,
        AllocSwitch::Off => PerfOp::AllocOff,
    };
    let reply = phux_client::state::perf_control_on(conn, op).await?;
    if json {
        outln!("{reply}");
        return Ok(());
    }
    let on = reply["alloc"].as_bool().unwrap_or(false);
    outln!("heap accounting {}", if on { "on" } else { "off" });
    if reply["installed"].as_bool() == Some(false) {
        info!("this server binary has no counting allocator, so it reports no heap figures");
    }
    Ok(())
}

async fn report(opts: PerfOptions, conn: &mut Connection) -> Result<(), AttachError> {
    // docs/protocol.md: a client MUST observe the GET_PERF feature bit before
    // sending the command; an older server would treat tag 0x18 as a decode
    // error and drop the connection, which reads as a crash, not an answer.
    if let Some(features) = phux_client::state::probe_hello_features(conn).await?
        && !features.contains(phux_protocol::caps::ServerFeature::GetPerf)
    {
        return Err(AttachError::Refused(
            "this server predates GET_PERF (phux 0.24.0 or older); upgrade it with `phux upgrade` and try again"
                .to_owned(),
        ));
    }
    let Some(every) = opts.watch else {
        let report = phux_client::state::get_perf_on(conn, opts.reset).await?;
        print_report(opts.json, &report, None);
        return Ok(());
    };
    let Some(every) = Duration::try_from_secs_f64(every)
        .ok()
        .map(|d| d.max(MIN_WATCH))
    else {
        return Err(AttachError::Refused(format!(
            "--watch {every} is not a usable interval; give a finite number of seconds"
        )));
    };
    let mut prev = phux_client::state::get_perf_on(conn, opts.reset).await?;
    loop {
        tokio::time::sleep(every).await;
        let cur = phux_client::state::get_perf_on(conn, opts.reset).await?;
        // The header (interval length, CPU consumed) is always the delta of
        // the two reports. With --reset the server already zeroed the
        // metrics after `prev`, so `cur`'s metrics are the interval as-is;
        // otherwise fold the two lifetime tables into one.
        let mut interval = cur.delta(&prev);
        if opts.reset {
            interval.metrics.clone_from(&cur.metrics);
        }
        if !opts.json {
            // Home and clear between samples so the table reads like `top`.
            out!("\x1b[H\x1b[2J");
        }
        print_report(opts.json, &interval, Some(every));
        prev = cur;
    }
}

fn print_report(json: bool, report: &PerfReport, interval: Option<Duration>) {
    if json {
        outln!("{}", report.to_json());
    } else {
        out!("{}", phux_perf::render_report(report, interval));
    }
}
