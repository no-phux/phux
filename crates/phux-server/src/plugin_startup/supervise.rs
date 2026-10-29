//! One supervisor task: reap orphans, start the catalog, restart failures,
//! and stop on shutdown, upgrade, or a config reload.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use phux_plugin::run_log::{self, RunRecord};
use phux_plugin::startup_log;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::backoff::{AfterExit, Policy};
use super::plan::{StartupCatalog, StartupKey, StartupSpec};
use super::rotate::CHECK_INTERVAL;
use super::spawn::{self, GroupChild};
use super::stop::stop_children;
use super::{PluginStartupConfig, PluginStartupSource};

/// Commands the accept loop and the upgrade path send the supervisor.
pub(super) enum Cmd {
    /// Re-read the plugin registry and reconcile.
    Reload,
    /// Stop every sidecar and reply once they are gone. Used before upgrade
    /// `exec`. A later [`Cmd::Resume`] starts them again if exec does not
    /// happen.
    Pause(oneshot::Sender<()>),
    /// Start the current catalog after a [`Cmd::Pause`].
    Resume,
}

/// What the supervisor is doing with one spec.
struct Slot {
    spec: StartupSpec,
    child: Option<GroupChild>,
    phase: Phase,
    policy: Policy,
    started: Option<Instant>,
}

#[derive(Clone, Copy)]
enum Phase {
    Live,
    Backoff { until: Instant },
    GaveUp,
    CleanExit,
}

struct Supervisor {
    root: CancellationToken,
    socket: PathBuf,
    state_dir: PathBuf,
    run_log: PathBuf,
    reload: Option<PluginStartupSource>,
    catalog: Vec<StartupSpec>,
    slots: Vec<Slot>,
    paused: bool,
    logged_skips: BTreeSet<StartupKey>,
    origin: Instant,
    rotate_at: Instant,
    blocked: bool,
    /// One permit when any watched child exits. `drive` then polls them all.
    notify: Arc<Notify>,
}

pub(super) async fn run(
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    cfg: PluginStartupConfig,
    socket: PathBuf,
    root: CancellationToken,
) {
    let blocked = phux_config::production::refuse_dev_on_production_state(&cfg.state_dir).is_err();
    if blocked {
        tracing::warn!(
            "startup: not supervising sidecars; development build refused this state dir"
        );
    }
    // The accept loops are already polling. Yield once so a sidecar that
    // dials `PHUX_SOCKET` immediately finds accept in progress.
    tokio::task::yield_now().await;
    if !blocked {
        super::reap::reap_orphans(&cfg.state_dir).await;
    }

    let initial = cfg.catalog;
    let mut supervisor = Supervisor {
        root,
        socket,
        run_log: cfg.state_dir.join(run_log::RUN_LOG_FILE),
        state_dir: cfg.state_dir,
        reload: cfg.reload,
        catalog: Vec::new(),
        slots: Vec::new(),
        paused: false,
        logged_skips: BTreeSet::new(),
        origin: Instant::now(),
        rotate_at: Instant::now() + CHECK_INTERVAL,
        blocked,
        notify: Arc::new(Notify::new()),
    };
    supervisor.apply(initial).await;
    supervisor.serve(&mut rx).await;
}

impl Supervisor {
    async fn serve(&mut self, rx: &mut mpsc::UnboundedReceiver<Cmd>) {
        loop {
            if self.root.is_cancelled() {
                self.stop_all().await;
                break;
            }
            // No deadline and no child: wait for shutdown or a command.
            // An empty catalog must not wake the runtime on a timer.
            let deadline = self.next_deadline();
            let root = self.root.clone();
            let notify = Arc::clone(&self.notify);
            tokio::select! {
                biased;
                () = root.cancelled() => {
                    self.stop_all().await;
                    break;
                }
                cmd = rx.recv() => {
                    match cmd {
                        None => {
                            self.stop_all().await;
                            break;
                        }
                        Some(cmd) => self.command(cmd).await,
                    }
                }
                () = notify.notified() => {}
                () = wait_until(deadline) => {}
            }
            if self.root.is_cancelled() {
                continue;
            }
            self.drive();
            self.rotate_if_due().await;
        }
    }

    /// Soonest backoff, log rotation, or the one-second fallback for a child
    /// whose exit watch could not be armed. `None` when nothing is running
    /// and nothing is waiting to restart.
    fn next_deadline(&self) -> Option<Instant> {
        let mut deadline: Option<Instant> = None;
        let mut consider = |at: Instant| {
            deadline = Some(match deadline {
                Some(current) if current < at => current,
                _ => at,
            });
        };
        for slot in &self.slots {
            if let Phase::Backoff { until } = slot.phase
                && slot.child.is_none()
            {
                consider(until);
            }
        }
        if self.slots.iter().any(|slot| slot.child.is_some()) {
            consider(self.rotate_at);
            if self.slots.iter().any(|slot| {
                slot.child
                    .as_ref()
                    .is_some_and(|child| !child.has_exit_watch())
            }) {
                consider(Instant::now() + Duration::from_secs(1));
            }
        }
        deadline
    }

    async fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Reload => self.reload().await,
            Cmd::Pause(reply) => {
                self.pause().await;
                let _ = reply.send(());
            }
            Cmd::Resume => self.resume().await,
        }
    }

    async fn reload(&mut self) {
        let Some(source) = self.reload.clone() else {
            return;
        };
        let catalog = match source.read() {
            Ok(catalog) => catalog,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "startup: config reload failed; keeping the running sidecars"
                );
                return;
            }
        };
        self.apply(catalog).await;
    }

    async fn pause(&mut self) {
        self.paused = true;
        self.stop_all().await;
        self.slots.clear();
    }

    async fn resume(&mut self) {
        if self.root.is_cancelled() {
            return;
        }
        self.paused = false;
        let catalog = StartupCatalog {
            admitted: self.catalog.clone(),
            skipped_cap: Vec::new(),
        };
        self.apply(catalog).await;
    }

    async fn stop_all(&mut self) {
        let specs: Vec<StartupSpec> = self
            .slots
            .iter()
            .filter(|slot| slot.child.is_some())
            .map(|slot| slot.spec.clone())
            .collect();
        for spec in &specs {
            self.lifecycle(spec, "stopped");
        }
        let mut children = Vec::new();
        for slot in &mut self.slots {
            if let Some(child) = slot.child.take() {
                children.push(child);
            }
        }
        stop_children(&mut children).await;
    }

    async fn apply(&mut self, catalog: StartupCatalog) {
        self.note_skips(&catalog.skipped_cap);
        self.catalog.clone_from(&catalog.admitted);
        let desired = catalog.admitted;
        let mut retiring = Vec::new();
        let mut keep = Vec::new();
        let mut stopped = Vec::new();
        for slot in self.slots.drain(..) {
            if desired.iter().any(|spec| spec.key() == slot.spec.key()) {
                keep.push(slot);
            } else if let Some(child) = slot.child {
                stopped.push(slot.spec);
                retiring.push(child);
            } else if !matches!(slot.phase, Phase::GaveUp | Phase::CleanExit) {
                stopped.push(slot.spec);
            }
        }
        self.slots = keep;
        for spec in &stopped {
            self.lifecycle(spec, "stopped");
        }
        if !retiring.is_empty() {
            stop_children(&mut retiring).await;
        }
        if self.paused || self.root.is_cancelled() || self.blocked {
            return;
        }
        for spec in desired {
            if self.slots.iter().any(|slot| slot.spec.key() == spec.key()) {
                continue;
            }
            self.start_fresh(spec);
        }
    }

    fn note_skips(&mut self, skipped: &[StartupSpec]) {
        let keys: BTreeSet<_> = skipped.iter().map(StartupSpec::key).collect();
        self.logged_skips.retain(|key| keys.contains(key));
        for spec in skipped {
            if self.logged_skips.insert(spec.key()) {
                self.lifecycle(spec, "skipped: cap");
            }
        }
    }

    fn start_fresh(&mut self, spec: StartupSpec) {
        let slot = Slot {
            spec,
            child: None,
            phase: Phase::Live,
            policy: Policy::new(),
            started: None,
        };
        self.slots.push(slot);
        let index = self.slots.len() - 1;
        self.spawn_slot(index, false);
    }

    fn drive(&mut self) {
        if self.blocked {
            return;
        }
        let mut exited = Vec::new();
        for index in 0..self.slots.len() {
            let Some(child) = self.slots[index].child.as_mut() else {
                continue;
            };
            if child.poll_reaped() {
                exited.push((index, child.exit_code()));
            }
        }
        for (index, code) in exited {
            self.slots[index].child.take();
            self.on_exit(index, code);
        }
        if self.paused || self.root.is_cancelled() {
            return;
        }
        let now = Instant::now();
        let due: Vec<usize> = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| {
                slot.child.is_none()
                    && matches!(slot.phase, Phase::Backoff { until } if now >= until)
            })
            .map(|(index, _)| index)
            .collect();
        for index in due {
            self.spawn_slot(index, true);
        }
    }

    fn on_exit(&mut self, index: usize, code: Option<i32>) {
        let uptime = self.slots[index]
            .started
            .take()
            .map(|started| started.elapsed())
            .unwrap_or_default();
        let spec = self.slots[index].spec.clone();
        let now = self.origin.elapsed();
        match self.slots[index].policy.on_exit(code, uptime, now) {
            AfterExit::Clean => {
                self.slots[index].phase = Phase::CleanExit;
                self.lifecycle(&spec, "exited 0 (not restarting)");
            }
            AfterExit::Restart { delay } => {
                self.slots[index].phase = Phase::Backoff {
                    until: Instant::now() + delay,
                };
            }
            AfterExit::GiveUp => {
                self.slots[index].phase = Phase::GaveUp;
                self.lifecycle(&spec, "gave up");
            }
        }
    }

    fn spawn_slot(&mut self, index: usize, restart: bool) {
        if self.root.is_cancelled() || self.paused || self.blocked {
            return;
        }
        let spec = self.slots[index].spec.clone();
        match spawn::spawn_child(&spec, &self.socket, &self.state_dir, &self.notify) {
            Ok(child) => {
                self.slots[index].child = Some(child);
                self.slots[index].started = Some(Instant::now());
                self.slots[index].phase = Phase::Live;
                self.lifecycle(&spec, if restart { "restarting" } else { "started" });
            }
            Err(err) => {
                tracing::warn!(
                    plugin = %spec.plugin_id,
                    startup = %spec.startup_id,
                    error = %err,
                    "startup: spawn failed"
                );
                self.note_failure(index, Duration::ZERO);
            }
        }
    }

    fn note_failure(&mut self, index: usize, uptime: Duration) {
        let now = self.origin.elapsed();
        let spec = self.slots[index].spec.clone();
        match self.slots[index].policy.on_exit(Some(1), uptime, now) {
            AfterExit::Restart { delay } => {
                self.slots[index].phase = Phase::Backoff {
                    until: Instant::now() + delay,
                };
            }
            AfterExit::GiveUp => {
                self.slots[index].phase = Phase::GaveUp;
                self.lifecycle(&spec, "gave up");
            }
            AfterExit::Clean => self.slots[index].phase = Phase::CleanExit,
        }
    }

    async fn rotate_if_due(&mut self) {
        if Instant::now() < self.rotate_at {
            return;
        }
        if !self.slots.iter().any(|slot| slot.child.is_some()) {
            return;
        }
        self.rotate_at = Instant::now() + CHECK_INTERVAL;
        let paths: Vec<PathBuf> = self
            .slots
            .iter()
            .map(|slot| {
                startup_log::log_path(&self.state_dir, &slot.spec.plugin_id, &slot.spec.startup_id)
            })
            .collect();
        // Copy-then-truncate can be megabytes. Doing it on the `LocalSet`
        // thread stalls every pane until the copy returns.
        let rotated = tokio::task::spawn_blocking(move || {
            for path in &paths {
                if let Err(err) = super::rotate::rotate_log(
                    path,
                    startup_log::LOG_MAX_BYTES,
                    startup_log::LOG_GENERATIONS,
                ) {
                    tracing::debug!(
                        path = %path.display(),
                        error = %err,
                        "startup log rotation failed"
                    );
                }
            }
        })
        .await;
        if let Err(err) = rotated {
            tracing::debug!(error = %err, "startup log rotation task failed");
        }
    }

    fn lifecycle(&self, spec: &StartupSpec, note: &str) {
        let record = RunRecord::startup(&spec.plugin_id, &spec.startup_id, &spec.command, note);
        if let Err(err) = run_log::record_at(&self.run_log, &record) {
            tracing::debug!(error = %err, "startup lifecycle line was not written");
        }
    }
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}
