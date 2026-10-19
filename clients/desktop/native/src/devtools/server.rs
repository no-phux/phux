//! The server's own report, read the way an agent would: `phux perf --json`.
//!
//! The desktop already shells out to the CLI for `status --json`; doing the
//! same here keeps `GET_PERF` out of the runtime's event path and means the
//! HUD, the monitor and the dev socket see exactly what `phux perf` prints.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use phux_perf::PerfReport;

/// How the desktop reaches its server's CLI.
#[derive(Debug, Clone)]
pub struct ServerCli {
    pub phux: PathBuf,
    pub socket: PathBuf,
}

impl ServerCli {
    /// Run `phux --socket SOCKET ARGS` and return stdout.
    ///
    /// # Errors
    ///
    /// The CLI could not run, or it exited non-zero (its stderr is the error).
    pub fn run(&self, args: &[&str]) -> anyhow::Result<Vec<u8>> {
        let output = Command::new(&self.phux)
            .arg("--socket")
            .arg(&self.socket)
            .args(args)
            .output()?;
        if !output.status.success() {
            anyhow::bail!(
                "`phux {}` failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output.stdout)
    }

    /// One `GET_PERF` through the CLI.
    ///
    /// # Errors
    ///
    /// See [`Self::run`]; or the CLI printed something that is not a report.
    pub fn report(&self) -> anyhow::Result<PerfReport> {
        let stdout = self.run(&["perf", "--json"])?;
        Ok(PerfReport::from_json(
            String::from_utf8_lossy(&stdout).trim(),
        )?)
    }

    /// The socket this CLI talks to.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

/// The server's latest report and its CPU over the interval before it.
#[derive(Debug, Clone)]
pub struct ServerSample {
    pub report: PerfReport,
    /// Server CPU over the last interval; one core is 100%.
    pub cpu_percent: Option<f64>,
    pub at: Instant,
}

/// A background reader of the server's report, sampling every `interval`
/// while someone holds a [`Watch`].
#[derive(Debug, Default)]
pub struct ServerPoller {
    state: Arc<Mutex<PollState>>,
}

#[derive(Debug, Default)]
struct PollState {
    watchers: usize,
    running: bool,
    latest: Option<ServerSample>,
    error: Option<String>,
}

/// Keeps the poller sampling while alive.
#[derive(Debug)]
pub struct Watch {
    state: Arc<Mutex<PollState>>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.watchers = state.watchers.saturating_sub(1);
    }
}

/// How often a watched poller reads the server.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

impl ServerPoller {
    /// Sample `cli` every [`POLL_INTERVAL`] until the returned watch and every
    /// other watch drop. The thread exits when nobody watches.
    #[must_use]
    pub fn watch(&self, cli: &ServerCli) -> Watch {
        let start = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.watchers += 1;
            !std::mem::replace(&mut state.running, true)
        };
        if start {
            let state = Arc::clone(&self.state);
            let cli = cli.clone();
            let spawned = std::thread::Builder::new()
                .name("phux-devtools-server".to_owned())
                .spawn(move || poll(&state, &cli));
            if let Err(error) = spawned {
                log::warn!("devtools: server poller did not start: {error}");
                self.state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .running = false;
            }
        }
        Watch {
            state: Arc::clone(&self.state),
        }
    }

    /// The latest sample, if any, and the last error, if the last read failed.
    #[must_use]
    pub fn latest(&self) -> (Option<ServerSample>, Option<String>) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        (state.latest.clone(), state.error.clone())
    }
}

fn poll(state: &Mutex<PollState>, cli: &ServerCli) {
    loop {
        let read = cli.report();
        {
            let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
            match read {
                Ok(report) => {
                    let at = Instant::now();
                    let cpu_percent = state
                        .latest
                        .as_ref()
                        .and_then(|before| cpu_between(before, &report, at));
                    state.latest = Some(ServerSample {
                        report,
                        cpu_percent,
                        at,
                    });
                    state.error = None;
                }
                Err(error) => state.error = Some(format!("{error:#}")),
            }
            if state.watchers == 0 {
                state.running = false;
                return;
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Server CPU between two samples; `None` across a restart or a failed read.
#[allow(
    clippy::cast_precision_loss,
    reason = "CPU microseconds over seconds, for a percentage"
)]
fn cpu_between(before: &ServerSample, now: &PerfReport, at: Instant) -> Option<f64> {
    if before.report.pid != now.pid {
        return None;
    }
    let (Some(a), Some(b)) = (before.report.process, now.process) else {
        return None;
    };
    let elapsed = at.checked_duration_since(before.at)?.as_secs_f64();
    let cpu_us = b.cpu_total_us().checked_sub(a.cpu_total_us())?;
    (elapsed > 0.0).then(|| cpu_us as f64 / 1_000_000.0 / elapsed * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_perf::ProcessStats;

    fn report(pid: u32, cpu_us: u64) -> PerfReport {
        PerfReport {
            schema_version: phux_perf::SCHEMA_VERSION,
            role: "server".to_owned(),
            pid,
            captured_unix_ms: 0,
            uptime_ms: 0,
            process: Some(ProcessStats {
                cpu_user_us: cpu_us,
                ..ProcessStats::default()
            }),
            metrics: Vec::new(),
            stream_diagnostics: None,
            rss_bytes: None,
            alloc: None,
        }
    }

    #[test]
    fn cpu_is_interval_cpu_and_resets_across_a_restart() {
        let start = Instant::now();
        let before = ServerSample {
            report: report(7, 1_000_000),
            cpu_percent: None,
            at: start,
        };
        let later = start + Duration::from_secs(2);
        let cpu = cpu_between(&before, &report(7, 2_000_000), later).unwrap_or_default();
        assert!((cpu - 50.0).abs() < 1e-9, "{cpu}");
        assert_eq!(cpu_between(&before, &report(8, 2_000_000), later), None);
    }
}
