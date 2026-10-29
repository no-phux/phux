//! The server's own report, read the way an agent would: `phux perf --json`.
//!
//! The desktop already shells out to the CLI for `status --json`; doing the
//! same here keeps `GET_PERF` out of the runtime's event path and means the
//! HUD, the monitor and the dev socket see exactly what `phux perf` prints.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
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
    /// Run `phux --socket SOCKET ARGS` and return stdout, killing the CLI if
    /// it has not finished within `timeout`.
    ///
    /// # Errors
    ///
    /// The CLI could not run, ran out of time, or exited non-zero (its stderr
    /// is the error).
    pub fn run(&self, args: &[&str], timeout: Duration) -> anyhow::Result<Vec<u8>> {
        self.check_socket()?;
        let mut child = crate::platform::phux_command(&self.phux)
            .arg("--socket")
            .arg(&self.socket)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        // Drain both pipes while waiting, so a large report cannot fill a
        // pipe and deadlock the wait.
        let stdout = drain(child.stdout.take(), STDOUT_CAP);
        let stderr = drain(child.stderr.take(), STDERR_CAP);
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!(
                    "`phux {}` did not finish within {}s",
                    args.join(" "),
                    timeout.as_secs()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let (stdout, stdout_truncated) = stdout.join().unwrap_or_default();
        let (stderr, stderr_truncated) = stderr.join().unwrap_or_default();
        if !status.success() {
            anyhow::bail!(
                "`phux {}` failed: {}{}",
                args.join(" "),
                String::from_utf8_lossy(&stderr).trim(),
                if stderr_truncated {
                    " [stderr truncated]"
                } else {
                    ""
                }
            );
        }
        if stdout_truncated {
            anyhow::bail!(
                "`phux {}` output exceeds {STDOUT_CAP} bytes",
                args.join(" ")
            );
        }
        Ok(stdout)
    }

    /// One `GET_PERF` through the CLI.
    ///
    /// # Errors
    ///
    /// See [`Self::run`]; or the CLI printed something that is not a report.
    pub fn report(&self) -> anyhow::Result<PerfReport> {
        let stdout = self.run(&["perf", "--json"], REPORT_TIMEOUT)?;
        Ok(PerfReport::from_json(
            String::from_utf8_lossy(&stdout).trim(),
        )?)
    }

    /// The socket this CLI talks to.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Apply the shared build-identity guard before delegating to a CLI,
    /// which may itself be an installed binary allowed to reach production.
    ///
    /// # Errors
    ///
    /// This development build is associated with a production socket.
    pub fn check_socket(&self) -> anyhow::Result<()> {
        phux_config_core::socket::refuse_dev_on_production(&self.socket).map_err(anyhow::Error::msg)
    }
}

/// How long one `phux perf --json` may take.
pub const REPORT_TIMEOUT: Duration = Duration::from_secs(10);

// Keep reading after the cap: stopping would block the child on a full pipe.
const STDOUT_CAP: usize = 1024 * 1024;
const STDERR_CAP: usize = 64 * 1024;

/// Read a child's pipe to the end on its own thread, retaining at most `cap` bytes.
fn drain(
    pipe: Option<impl Read + Send + 'static>,
    cap: usize,
) -> std::thread::JoinHandle<(Vec<u8>, bool)> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut truncated = false;
        if let Some(mut pipe) = pipe {
            let mut chunk = [0; 8192];
            while let Ok(n) = pipe.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let keep = n.min(cap.saturating_sub(bytes.len()));
                bytes.extend_from_slice(&chunk[..keep]);
                truncated |= keep < n;
            }
        }
        (bytes, truncated)
    })
}

/// What the HUD shows of the server, copied out without cloning a report.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerSummary {
    pub cpu_percent: Option<f64>,
    pub rss_bytes: Option<u64>,
    /// Why the last read failed, if it did.
    pub error: Option<String>,
    /// Whether any reading has arrived yet.
    pub sampled: bool,
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

    /// The figures the HUD shows, without cloning the report.
    #[must_use]
    pub fn summary(&self) -> ServerSummary {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        ServerSummary {
            cpu_percent: state.latest.as_ref().and_then(|s| s.cpu_percent),
            rss_bytes: state.latest.as_ref().and_then(|s| s.report.rss_bytes),
            error: state.error.clone(),
            sampled: state.latest.is_some(),
        }
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
    fn cli_pipe_capture_drains_but_never_retains_more_than_its_cap() {
        let oversized = std::io::Cursor::new(vec![b'x'; STDOUT_CAP * 4]);
        let (bytes, truncated) = drain(Some(oversized), STDOUT_CAP).join().unwrap();
        assert_eq!(bytes.len(), STDOUT_CAP);
        assert!(bytes.iter().all(|byte| *byte == b'x'));
        assert!(truncated);

        let exact = std::io::Cursor::new(vec![b'y'; STDERR_CAP]);
        let (bytes, truncated) = drain(Some(exact), STDERR_CAP).join().unwrap();
        assert_eq!(bytes.len(), STDERR_CAP);
        assert!(!truncated);
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

    #[test]
    fn dev_desktop_refuses_production_before_spawning_any_cli() {
        let cli = ServerCli {
            phux: PathBuf::from("/nonexistent/installed-phux"),
            socket: PathBuf::from("/run/user/4294967294/phux/phux.sock"),
        };
        assert_eq!(
            phux_config_core::instance::build_kind(),
            phux_config_core::instance::BuildKind::Dev
        );
        let error = cli
            .run(&["perf", "trace"], REPORT_TIMEOUT)
            .expect_err("production is refused");
        assert!(
            error.to_string().contains("production phux socket"),
            "{error:#}"
        );
    }
}
