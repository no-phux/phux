//! Window and application mirrors run as this app's own children.
//!
//! macOS grants Screen Recording and Accessibility to the app that owns a
//! process. A producer the server spawns runs under the server's launchd job,
//! which no grant reaches, so its captures are refused. Run by the desktop,
//! the producer and the `screencapture` it calls are attributed to the app the
//! person already granted.
//!
//! A mirror that fails is restarted with backoff, and each is given this
//! app's pid so it exits if the app dies without dropping it
//! (`docs/resource-kinds.md#supervision`).
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use gpui::{Context, WeakEntity};

use super::super::desk::Desk;
use super::super::notifications::Kind;

/// A running producer. Dropping it stops the process.
pub(crate) struct LocalMirror {
    id: u64,
    child: Child,
    log: PathBuf,
    started: Instant,
}

impl Drop for LocalMirror {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Waits before each restart of a failed mirror; once spent, it stays stopped.
const BACKOFF: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(4),
    Duration::from_secs(16),
];

/// A mirror that ran this long before failing starts its retries afresh.
const HEALTHY_RUN: Duration = Duration::from_secs(60);

/// After a mirror that ran for `ran` failed with `retries` used: the retries
/// used once it restarts and the wait first, or `None` to give up.
fn next_restart(ran: Duration, retries: usize) -> Option<(usize, Duration)> {
    let retries = if ran >= HEALTHY_RUN { 0 } else { retries };
    BACKOFF.get(retries).map(|wait| (retries + 1, *wait))
}

/// The producer argv with the parent pane (the desktop is not in a pane, so
/// the mirror cannot infer one) and this app's pid, so the mirror exits
/// within a second of the app dying however it dies.
pub(super) fn local_argv(mut argv: Vec<String>, pane: &str) -> Vec<String> {
    argv.extend([
        "--pane".into(),
        pane.into(),
        "--parent-pid".into(),
        std::process::id().to_string(),
    ]);
    argv
}

impl Desk {
    /// Start `argv` as a child of this app and watch it until it exits,
    /// restarting it with backoff while it fails.
    pub(super) fn start_local_mirror(
        &mut self,
        argv: &[String],
        label: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let mut id = self.spawn_mirror(argv, label)?;
        let (argv, label) = (argv.to_vec(), label.to_owned());
        cx.spawn(async move |this: WeakEntity<Desk>, cx| {
            let mut retries = 0;
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let failure = match this.update(cx, |desk, _| desk.reap_mirror(id)) {
                    Ok(Reaped::Running) => continue,
                    Ok(Reaped::Failed { reason, ran }) => (reason, ran),
                    Ok(Reaped::Done) | Err(_) => return,
                };
                let (reason, ran) = failure;
                let Some((used, wait)) = next_restart(ran, retries) else {
                    let _ = this.update(cx, |desk, cx| {
                        desk.mirror_failed(&label, &reason, retries, cx);
                    });
                    return;
                };
                retries = used;
                log::info!("local mirror of {label} restarts in {wait:?} (retry {retries})");
                cx.background_executor().timer(wait).await;
                match this.update(cx, |desk, _| desk.spawn_mirror(&argv, &label)) {
                    Ok(Ok(next)) => id = next,
                    Ok(Err(error)) => {
                        let _ = this.update(cx, |desk, cx| {
                            desk.mirror_failed(&label, &error, retries, cx);
                        });
                        return;
                    }
                    Err(_) => return,
                }
            }
        })
        .detach();
        Ok(())
    }

    /// Run `argv` as a child of this app, stderr to a log; its mirror id.
    fn spawn_mirror(&mut self, argv: &[String], label: &str) -> Result<u64, String> {
        let (program, args) = argv.split_first().ok_or("empty producer argv")?;
        let id = self
            .mirrors
            .iter()
            .map(|mirror| mirror.id)
            .max()
            .unwrap_or(0)
            + 1;
        let log = std::env::temp_dir().join(format!("phux-mirror-{}-{id}.log", std::process::id()));
        let stderr = std::fs::File::create(&log).map_err(|error| error.to_string())?;
        let child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .map_err(|error| format!("Could not run {program}: {error}"))?;
        log::info!("local mirror {id} started: {label}");
        self.mirrors.push(LocalMirror {
            id,
            child,
            log,
            started: Instant::now(),
        });
        Ok(id)
    }

    /// Whether mirror `id` still runs, ended, or failed and why.
    fn reap_mirror(&mut self, id: u64) -> Reaped {
        let Some(index) = self.mirrors.iter().position(|mirror| mirror.id == id) else {
            return Reaped::Done;
        };
        let Ok(Some(status)) = self.mirrors[index].child.try_wait() else {
            return Reaped::Running;
        };
        let mirror = self.mirrors.remove(index);
        let reason = std::fs::read_to_string(&mirror.log).unwrap_or_default();
        let _ = std::fs::remove_file(&mirror.log);
        if status.success() {
            return Reaped::Done;
        }
        log::warn!("local mirror {id} exited {status}: {reason}");
        let last = reason.lines().rev().find(|line| !line.trim().is_empty());
        Reaped::Failed {
            reason: last.unwrap_or("The producer exited.").to_owned(),
            ran: mirror.started.elapsed(),
        }
    }

    /// Say that the mirror of `label` stopped for good, after `retries`.
    fn mirror_failed(&mut self, label: &str, reason: &str, retries: usize, cx: &mut Context<Self>) {
        let detail = if retries == 0 {
            reason.to_owned()
        } else {
            format!("{reason} (restarted {retries} times, then gave up)")
        };
        self.post(
            Kind::Error,
            &format!("Mirror of {label} stopped"),
            Some(detail),
        );
        cx.notify();
    }
}

/// One look at a mirror's process.
enum Reaped {
    Running,
    /// Dropped by this app, or exited cleanly: nothing to restart.
    Done,
    Failed {
        reason: String,
        ran: Duration,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_mirror_names_its_parent_pane_and_pid() {
        let argv = local_argv(vec!["/bin/phux".into(), "agent".into()], "@7");
        let pid = std::process::id().to_string();
        assert_eq!(
            argv,
            ["/bin/phux", "agent", "--pane", "@7", "--parent-pid", &pid]
        );
    }

    #[test]
    fn a_failing_mirror_restarts_three_times_with_backoff() {
        let quick = Duration::from_secs(2);
        let waits: Vec<_> = (0..4).map(|n| next_restart(quick, n)).collect();
        assert_eq!(
            waits,
            [
                Some((1, Duration::from_secs(1))),
                Some((2, Duration::from_secs(4))),
                Some((3, Duration::from_secs(16))),
                None,
            ]
        );
        // A long healthy run earns its retries back.
        assert_eq!(
            next_restart(HEALTHY_RUN, 3),
            Some((1, Duration::from_secs(1)))
        );
    }
}
