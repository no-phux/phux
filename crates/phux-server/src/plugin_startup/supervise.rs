//! One supervisor task: reap orphans, start the catalog, restart failures,
//! and stop on shutdown, upgrade, or a config reload.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Instant;

use phux_plugin::run_log::{self, RunRecord};
use phux_plugin::startup_log;
use tokio::sync::{mpsc, oneshot};
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
        rotate_at: Instant::now(),
        blocked,
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
            let tick = tokio::time::sleep(std::time::Duration::from_millis(50));
            tokio::select! {
                biased;
                () = self.root.cancelled() => {
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
                () = tick => {}
            }
            self.drive();
            self.rotate_if_due();
        }
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
        let mut slot = Slot {
            spec,
            child: None,
            phase: Phase::Live,
            policy: Policy::new(),
            started: None,
        };
        self.spawn_into(&mut slot, false);
        self.slots.push(slot);
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
            self.spawn_index(index, true);
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

    fn spawn_index(&mut self, index: usize, restart: bool) {
        let spec = self.slots[index].spec.clone();
        match self.spawn_spec(&spec) {
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
                self.note_failure(index, std::time::Duration::ZERO);
            }
        }
    }

    fn spawn_into(&self, slot: &mut Slot, restart: bool) {
        if self.root.is_cancelled() || self.paused || self.blocked {
            return;
        }
        let spec = slot.spec.clone();
        match self.spawn_spec(&spec) {
            Ok(child) => {
                slot.child = Some(child);
                slot.started = Some(Instant::now());
                slot.phase = Phase::Live;
                self.lifecycle(&spec, if restart { "restarting" } else { "started" });
            }
            Err(err) => {
                tracing::warn!(
                    plugin = %spec.plugin_id,
                    startup = %spec.startup_id,
                    error = %err,
                    "startup: spawn failed"
                );
                let uptime = std::time::Duration::ZERO;
                let now = self.origin.elapsed();
                match slot.policy.on_exit(Some(1), uptime, now) {
                    AfterExit::Restart { delay } => {
                        slot.phase = Phase::Backoff {
                            until: Instant::now() + delay,
                        };
                    }
                    AfterExit::GiveUp => {
                        slot.phase = Phase::GaveUp;
                        self.lifecycle(&spec, "gave up");
                    }
                    AfterExit::Clean => slot.phase = Phase::CleanExit,
                }
            }
        }
    }

    fn spawn_spec(&self, spec: &StartupSpec) -> std::io::Result<GroupChild> {
        spawn::spawn_child(spec, &self.socket, &self.state_dir)
    }

    fn note_failure(&mut self, index: usize, uptime: std::time::Duration) {
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

    fn rotate_if_due(&mut self) {
        if self.rotate_at.elapsed() < CHECK_INTERVAL {
            return;
        }
        self.rotate_at = Instant::now();
        for slot in &self.slots {
            let path =
                startup_log::log_path(&self.state_dir, &slot.spec.plugin_id, &slot.spec.startup_id);
            if let Err(err) = super::rotate::rotate_log(
                &path,
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
    }

    fn lifecycle(&self, spec: &StartupSpec, note: &str) {
        let record = RunRecord::startup(&spec.plugin_id, &spec.startup_id, &spec.command, note);
        if let Err(err) = run_log::record_at(&self.run_log, &record) {
            tracing::debug!(error = %err, "startup lifecycle line was not written");
        }
    }
}
