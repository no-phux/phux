//! `phux perf` — the server's performance telemetry, live.
//!
//! One `GET_PERF` per sample. Without `--watch` the report is a lifetime
//! view since the server started (or since the last `--reset`). With
//! `--watch` the verb polls and prints each interval as a delta of two
//! reports, so counters become rates and every histogram covers only that
//! window: a stall shows up in the interval it happened in instead of being
//! averaged away by hours of quiet. See `docs/operations.md` §"Performance
//! observability" for what each row means and what healthy looks like.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_perf::PerfReport;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err};

/// Options for `phux perf`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PerfOptions {
    /// Print JSON instead of the table.
    pub(crate) json: bool,
    /// Poll every this many seconds and print interval deltas.
    pub(crate) watch: Option<f64>,
    /// Zero the server's metrics after each snapshot.
    pub(crate) reset: bool,
}

/// Shortest poll interval `--watch` will honour.
const MIN_WATCH: Duration = Duration::from_millis(100);

pub(crate) fn run_perf(opts: PerfOptions, socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    match rt.block_on(run(opts, &socket_path)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => json_err::report_no_server(opts.json, &err, &socket_path, "perf"),
    }
}

async fn run(opts: PerfOptions, socket_path: &Path) -> Result<(), AttachError> {
    let mut conn = Connection::connect(socket_path).await?;
    // L1.md §5.1: a client MUST observe the GET_PERF feature bit before
    // sending the command; an older server would treat tag 0x18 as a decode
    // error and drop the connection, which reads as a crash, not an answer.
    if let Some(features) = phux_client::state::probe_hello_features(&mut conn).await?
        && !features.contains(phux_protocol::caps::ServerFeature::GetPerf)
    {
        return Err(AttachError::Refused(
            "this server predates GET_PERF (phux 0.24.0 or older); upgrade it with `phux upgrade` and try again"
                .to_owned(),
        ));
    }
    let Some(every) = opts.watch else {
        let report = phux_client::state::get_perf_on(&mut conn, opts.reset).await?;
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
    let mut prev = phux_client::state::get_perf_on(&mut conn, opts.reset).await?;
    loop {
        tokio::time::sleep(every).await;
        let cur = phux_client::state::get_perf_on(&mut conn, opts.reset).await?;
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
