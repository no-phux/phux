//! One process-wide updater; window subscriptions repaint on its activity.

mod status;
#[cfg(test)]
mod tests;
mod view;

pub(crate) use view::page;

use std::path::PathBuf;
use std::time::Duration;

use crate::updates::{self, Preferences, Release, RuntimeRelease};
use gpui::{Context, Subscription, Task};

pub(crate) struct Updater {
    pub(super) desktop: Option<gpui::WeakEntity<super::session::Desktop>>,
    pub prefs: Preferences,
    pub busy: bool,
    pub message: String,
    pub target: Option<Release>,
    pub runtime: Option<RuntimeRelease>,
    pub current: Option<Release>,
    pub bundle: Option<PathBuf>,
    phux: PathBuf,
    socket: PathBuf,
    pub development: bool,
    pub runtime_generation: u64,
    pub pending_handoff: bool,
    /// A verified bundle waiting beside the app; installed after exit (ADR-0175).
    pub staged: Option<Staged>,
    /// Reopen the app once the staged bundle is in place.
    relaunch: bool,
    /// The post-exit helper was already handed the staged bundle.
    handed_off: bool,
    /// The running update goes on to phux and the restart once the desktop is staged.
    chained: bool,
    /// What `busy` is doing.
    pub work: Work,
    _automatic: Option<Task<()>>,
    _quit: Option<Subscription>,
}

/// A downloaded, verified desktop update ready to swap in.
#[derive(Clone, Debug)]
pub(crate) struct Staged {
    pub app: PathBuf,
    pub release: Release,
}

/// The step a busy updater is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Work {
    Checking,
    /// The desktop bundle downloads and verifies.
    Downloading,
    /// phux installs and hands its server off.
    Installing,
}

/// The one update button, in the titlebar and on the Updates page. A click
/// carries every pending product: the desktop downloads and verifies, phux
/// updates and hands the server off, then the desktop restarts into its update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpdateAction {
    /// Only the desktop moves: download if needed, then restart.
    Desktop { staged: bool },
    /// Only phux moves; the desktop is current, so nothing restarts.
    Phux { retry: bool },
    /// Both move together: desktop staged, phux handed off, then restart.
    Both,
}

impl UpdateAction {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Desktop { staged: false } => "Update Desktop",
            Self::Desktop { staged: true } => "Restart to update",
            Self::Phux { retry: false } => "Update phux",
            Self::Phux { retry: true } => "Retry server handoff",
            Self::Both => "Update phux & Desktop",
        }
    }

    const fn desktop(self) -> bool {
        !matches!(self, Self::Phux { .. })
    }
}

/// A status pill's tone on the Updates page.
#[derive(Clone, Copy)]
enum Tone {
    Quiet,
    Good,
    Ready,
    Bad,
}

/// Prefixes a phux update that installed but could not hand the server off.
const HANDOFF_NOTE: &str = "Handoff:";

/// Whether two `major.minor` wire protocols interoperate: the major is the
/// compatibility line, so any minor on it speaks to any other. Text without
/// a `.` is compared whole, so an unknown shape is treated as a change.
fn same_protocol_line(a: &str, b: &str) -> bool {
    fn major(protocol: &str) -> &str {
        protocol
            .split_once('.')
            .map_or(protocol, |(major, _)| major)
    }
    major(a) == major(b)
}

/// The failure line a check left for `prefix`, without its prefix.
fn failure(message: &str, prefix: &str) -> Option<String> {
    message
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .map(|line| line.trim().to_owned())
}

impl Updater {
    pub fn new() -> Self {
        let bundle = std::env::current_exe()
            .ok()
            .and_then(|exe| updates::app_bundle(&exe));
        let current = bundle
            .as_deref()
            .and_then(|bundle| updates::installed_release(bundle).ok());
        Self {
            desktop: None,
            prefs: Preferences::load(
                current
                    .as_ref()
                    .map_or(updates::Channel::Stable, |release| release.channel),
            ),
            busy: false,
            message: "Not checked yet".into(),
            target: None,
            runtime: None,
            current,
            bundle,
            phux: PathBuf::new(),
            socket: PathBuf::new(),
            development: true,
            runtime_generation: 0,
            pending_handoff: false,
            staged: None,
            relaunch: false,
            handed_off: false,
            chained: false,
            work: Work::Checking,
            _automatic: None,
            _quit: None,
        }
    }

    /// Quitting with a staged update installs it once this process has exited.
    pub fn watch_quit(&mut self, cx: &mut Context<Self>) {
        self._quit = Some(cx.on_app_quit(|updater, cx| {
            if updater.staged.is_some()
                && !updater.relaunch
                && !updater.development
                && let Err(error) = updater.save_restart(cx)
            {
                log::error!("Could not save desktop restart: {error:#}");
                return std::future::ready(());
            }
            updater.apply_staged();
            std::future::ready(())
        }));
    }

    fn apply_staged(&mut self) {
        if self.handed_off || self.development {
            return;
        }
        let (Some(bundle), Some(staged)) = (self.bundle.as_deref(), self.staged.as_ref()) else {
            return;
        };
        match updates::apply_on_exit(bundle, &staged.app, self.relaunch) {
            Ok(()) => self.handed_off = true,
            Err(error) => log::error!("Could not hand off the desktop update: {error:#}"),
        }
    }

    pub fn start(&mut self, phux: PathBuf, socket: PathBuf, cx: &mut Context<Self>) {
        if self._automatic.is_some() {
            return;
        }
        self.phux = phux;
        self.socket = socket;
        // Explicit targets belong to development/qualification, not release management.
        self.development = self.bundle.is_none() || explicit_target();
        if self.current.is_none() {
            self.message = "Development build: self-update is unavailable".into();
        }
        if let Some(bundle) = self.bundle.as_deref().filter(|_| !self.development) {
            updates::clear_stale_stage(bundle);
        }
        if self.prefs.automatic && !self.development {
            self.check(cx);
        }
        self._automatic = Some(cx.spawn(async move |this, cx| {
            loop {
                smol::Timer::after(Duration::from_secs(6 * 60 * 60)).await;
                if this
                    .update(cx, |updater, cx| {
                        if updater.prefs.automatic && !updater.development {
                            updater.check(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    pub fn preferences(&mut self, change: impl FnOnce(&mut Preferences), cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let mut prefs = self.prefs.clone();
        change(&mut prefs);
        if let Err(error) = prefs.save() {
            self.message = format!("Could not save update settings: {error:#}");
            cx.notify();
            return;
        }
        let recheck = needs_recheck(&self.prefs, &prefs);
        self.adopt_preferences(prefs);
        if recheck {
            self.check(cx);
        }
        cx.notify();
    }

    fn adopt_preferences(&mut self, prefs: Preferences) {
        if prefs.desktop != self.prefs.desktop {
            self.target = None;
            self.staged = None;
        }
        if prefs.runtime != self.prefs.runtime || !prefs.manage_runtime {
            self.runtime = None;
            self.pending_handoff = false;
        }
        self.prefs = prefs;
    }

    pub fn check(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.work = Work::Checking;
        self.message = "Checking releases…".into();
        let prefs = self.prefs.clone();
        let phux = self.phux.clone();
        let manage = prefs.manage_runtime && !self.development;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (desktop, runtime) = smol::unblock(move || {
                let desktop = updates::latest(prefs.desktop);
                let runtime = manage.then(|| updates::runtime_check(&phux, prefs.runtime));
                (desktop, runtime)
            })
            .await;
            let _ = this.update(cx, |updater, cx| {
                updater.record_check(desktop, runtime);
                cx.notify();
            });
        })
        .detach();
    }

    fn record_check(
        &mut self,
        desktop: anyhow::Result<Release>,
        runtime: Option<anyhow::Result<RuntimeRelease>>,
    ) {
        self.busy = false;
        self.target = None;
        self.runtime = None;
        let mut failures = Vec::new();
        match desktop {
            Ok(target) => self.target = Some(target),
            Err(error) => failures.push(format!("Desktop: {error:#}")),
        }
        match runtime {
            Some(Ok(runtime)) => self.runtime = Some(runtime),
            Some(Err(error)) => failures.push(format!("phux: {error:#}")),
            None => {}
        }
        if !failures.is_empty() {
            self.message = failures.join("\n");
            return;
        }
        self.message = if self.current.is_none() {
            "Release checked. Development builds cannot self-update.".into()
        } else if self.desktop_available()
            || self
                .runtime
                .as_ref()
                .is_some_and(|runtime| runtime.available)
        {
            "An update is ready".into()
        } else {
            "You’re up to date".into()
        };
    }

    pub fn desktop_available(&self) -> bool {
        self.current
            .as_ref()
            .zip(self.target.as_ref())
            .is_some_and(|(current, target)| updates::available(current, target))
    }

    /// Whether a click should move phux: managed here and an update is waiting.
    pub fn phux_due(&self) -> bool {
        self.prefs.manage_runtime && self.runtime_available()
    }

    /// Whether the desktop has an update this click can carry.
    fn desktop_due(&self) -> bool {
        self.staged.is_some() || self.desktop_available()
    }

    /// The update button's action, if any product has one.
    pub fn action(&self) -> Option<UpdateAction> {
        if self.busy || self.development {
            return None;
        }
        // Linux reruns the installer for the desktop, so phux moves alone
        // there and the page names the installer command.
        let desktop = self.desktop_due() && !cfg!(target_os = "linux");
        match (desktop, self.phux_due()) {
            (true, true) => Some(UpdateAction::Both),
            (true, false) => Some(UpdateAction::Desktop {
                staged: self.staged.is_some(),
            }),
            (false, true) => Some(UpdateAction::Phux {
                retry: self.pending_handoff,
            }),
            (false, false) if self.desktop_due() => Some(UpdateAction::Desktop { staged: false }),
            (false, false) => None,
        }
    }

    pub fn act(&mut self, cx: &mut Context<Self>) {
        let Some(action) = self.action() else {
            return;
        };
        if action.desktop() && cfg!(target_os = "linux") {
            self.message =
                updates::linux_install_remedy(self.prefs.desktop, self.bundle.as_deref());
            cx.notify();
            return;
        }
        if action.desktop() && self.staged.is_none() {
            self.chained = true;
            self.download_desktop(cx);
            return;
        }
        self.finish_update(cx);
    }

    /// Everything after the desktop is staged: phux first, so the new desktop
    /// restarts onto the server it speaks to, then the restart itself.
    fn finish_update(&mut self, cx: &mut Context<Self>) {
        self.chained = false;
        if self.phux_due() {
            self.run_runtime(cx, true);
        } else {
            self.restart_to_update(cx);
        }
    }

    /// The release a staged or checked update would bring in.
    fn incoming(&self) -> Option<&Release> {
        self.staged
            .as_ref()
            .map(|staged| &staged.release)
            .or(self.target.as_ref().filter(|_| self.desktop_available()))
    }

    /// `(speaking now, incoming)` when the incoming desktop is on another wire
    /// protocol major than the local server. A minor difference on the same
    /// major interoperates and is not a change. Without a runtime check, the
    /// server is assumed to speak what this desktop links.
    pub fn protocol_change(&self) -> Option<(String, String)> {
        let incoming = self.incoming()?;
        let now = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.protocol_current.clone())
            .unwrap_or_else(updates::own_protocol);
        (!same_protocol_line(&incoming.protocol, &now)).then(|| (now, incoming.protocol.clone()))
    }

    fn download_desktop(&mut self, cx: &mut Context<Self>) {
        let Some((bundle, target)) = self.bundle.clone().zip(self.target.clone()) else {
            return;
        };
        self.busy = true;
        self.work = Work::Downloading;
        self.message = "Downloading and verifying desktop update…".into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || {
                // Next retains only recent revisions. Resolve again at the
                // click, then pin that exact artifact for the whole stage.
                let target = if target.channel == updates::Channel::Next {
                    updates::latest(updates::Channel::Next)?
                } else {
                    target
                };
                let app = updates::stage(&bundle, &target)?;
                anyhow::Ok(Staged {
                    app,
                    release: target,
                })
            })
            .await;
            let _ = this.update(cx, |updater, cx| {
                if updater.record_stage(result) {
                    updater.finish_update(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Record a finished download; true when the click goes on to phux and the restart.
    fn record_stage(&mut self, result: anyhow::Result<Staged>) -> bool {
        self.busy = false;
        let chained = std::mem::take(&mut self.chained);
        match result {
            Ok(staged) => {
                self.message = format!("Desktop {} is ready to install", staged.release.version);
                self.staged = Some(staged);
                chained
            }
            Err(error) => {
                self.message = format!("Desktop: Update failed: {error:#}");
                false
            }
        }
    }

    fn restart_to_update(&mut self, cx: &mut Context<Self>) {
        if self.staged.is_none() || self.development {
            return;
        }
        // The click may still hold its Desk. Snapshot only after that lease
        // ends, before quit observers can release any window's terminal views.
        self.busy = true;
        let updater = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = updater.update(cx, |updater, cx| {
                updater.busy = false;
                if let Err(error) = updater.save_restart(cx) {
                    updater.message = format!("Could not save desktop restart: {error:#}");
                    cx.notify();
                    return;
                }
                updater.relaunch = true;
                cx.quit();
            });
        });
    }

    fn save_restart(&self, cx: &mut gpui::App) -> anyhow::Result<()> {
        self.desktop
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No desktop to restore"))?
            .update(cx, |desktop, cx| desktop.save_restart(cx))?
    }

    fn run_runtime(&mut self, cx: &mut Context<Self>, then_restart: bool) {
        if self.busy || self.development || !self.phux_due() {
            return;
        }
        let phux = self.phux.clone();
        let socket = self.socket.clone();
        let channel = self.prefs.runtime;
        let retry = self.pending_handoff;
        let restart = then_restart && self.staged.is_some();
        self.busy = true;
        self.work = Work::Installing;
        self.message = "Updating phux and handing the live server off…".into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || {
                if retry {
                    updates::runtime_retry(&phux, &socket)
                } else {
                    updates::runtime_install(&phux, &socket, channel)
                }
            })
            .await;
            let _ = this.update(cx, |updater, cx| {
                // The staged desktop needs the server phux just handed off to.
                if updater.record_runtime(result) && restart {
                    updater.restart_to_update(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Record a finished phux update; true once the live server was handed off.
    fn record_runtime(&mut self, result: anyhow::Result<updates::RuntimeInstalled>) -> bool {
        self.busy = false;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                self.message = format!("phux: Update failed: {error:#}");
                return false;
            }
        };
        self.pending_handoff = result.retry_handoff;
        // What was the target is installed now, unless only the handoff ran.
        self.runtime = self.runtime.take().map(|runtime| RuntimeRelease {
            current: runtime.target.clone(),
            available: false,
            protocol_current: runtime.protocol_target.clone().or(runtime.protocol_current),
            ..runtime
        });
        if result.reconnect {
            self.runtime_generation += 1;
            self.message = "phux is updated and the server was handed off".into();
        } else {
            self.message = format!("{HANDOFF_NOTE} {}", result.message);
        }
        result.reconnect
    }

    pub fn runtime_available(&self) -> bool {
        self.pending_handoff
            || self
                .runtime
                .as_ref()
                .is_some_and(|runtime| runtime.available && runtime.mutable)
    }
}

fn explicit_target() -> bool {
    let command_line = std::env::args()
        .skip(1)
        .any(|arg| matches!(arg.as_str(), "--phux" | "--socket" | "--smoke"));
    let environment = ["PHUX_BIN", "PHUX_SOCKET", "PHUX_PROFILE"]
        .iter()
        .any(|key| std::env::var_os(key).is_some());
    command_line || environment
}

fn needs_recheck(before: &Preferences, after: &Preferences) -> bool {
    let channel_changed = after.desktop != before.desktop || after.runtime != before.runtime;
    let runtime_enabled = after.manage_runtime && !before.manage_runtime;
    let automatic_enabled = after.automatic && !before.automatic;
    channel_changed || runtime_enabled || automatic_enabled
}
