//! One process-wide updater; window subscriptions repaint on its activity.

mod progress;
mod restart;
mod status;
#[cfg(test)]
mod tests;
mod toast;
mod view;

#[cfg(test)]
use progress::drain_progress;
pub(crate) use toast::{card, trigger};
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
    /// What `busy` is doing.
    pub work: Work,
    /// Transfer measurements from the installer, never a simulated percentage.
    pub progress: Option<updates::StageProgress>,
    pub card_expanded: bool,
    /// Keep a successful server-only update visible until dismissed.
    pub completed: bool,
    _progress: Option<Task<()>>,
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

/// The one update action, in the corner card and on the Updates page. A click
/// prepares every pending product. Restarting Desktop is a separate click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpdateAction {
    /// Only Desktop moves: download, or explicitly restart an already staged build.
    Desktop { staged: bool },
    /// Only phux moves; the desktop is current, so nothing restarts.
    Phux { retry: bool },
    /// Prepare both: Desktop staged and phux handed off; restart stays a separate action.
    Both,
}

impl UpdateAction {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Desktop { staged: false } => "Download update",
            Self::Desktop { staged: true } => "Restart now",
            Self::Phux { retry: false } => "Update server",
            Self::Phux { retry: true } => "Retry server handoff",
            Self::Both => "Download Desktop",
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
            work: Work::Checking,
            progress: None,
            card_expanded: true,
            completed: false,
            _progress: None,
            _automatic: None,
            _quit: None,
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
        self.completed = false;
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
        self.completed = false;
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
        self.card_expanded = true;
        self.target = None;
        // A failed recheck must not erase a staged Desktop's known server
        // prerequisite and accidentally turn a combined restart into Desktop-only.
        let staged_runtime = self.runtime.take().filter(|_| self.staged.is_some());
        let mut failures = Vec::new();
        match desktop {
            Ok(target) => self.target = Some(target),
            Err(error) => failures.push(format!("Desktop: {error:#}")),
        }
        match runtime {
            Some(Ok(runtime)) => self.runtime = Some(runtime),
            Some(Err(error)) => {
                self.runtime = staged_runtime;
                failures.push(format!("phux: {error:#}"));
            }
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
        self.card_expanded = true;
        self.completed = false;
        if action.desktop() && cfg!(target_os = "linux") {
            self.message =
                updates::linux_install_remedy(self.prefs.desktop, self.bundle.as_deref());
            cx.notify();
            return;
        }
        if action.desktop() && self.staged.is_none() {
            self.download_desktop(cx);
            return;
        }
        self.finish_update(cx);
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
        self.progress = None;
        self.message = "Downloading and verifying desktop update…".into();
        let (progress, receiver) = std::sync::mpsc::channel();
        self.watch_progress(receiver, cx);
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
                let app = updates::stage(&bundle, &target, progress)?;
                anyhow::Ok(Staged {
                    app,
                    release: target,
                })
            })
            .await;
            let _ = this.update(cx, |updater, cx| {
                updater.record_stage(result);
                cx.notify();
            });
        })
        .detach();
    }

    /// A verified download waits for a separate restart click. The local server
    /// stays on this Desktop's protocol until that click, too.
    fn record_stage(&mut self, result: anyhow::Result<Staged>) {
        self.busy = false;
        self.progress = None;
        self._progress = None;
        match result {
            Ok(staged) => {
                self.message = format!("Desktop {} is ready to install", staged.release.version);
                self.staged = Some(staged);
            }
            Err(error) => {
                self.message = format!("Desktop: Update failed: {error:#}");
            }
        }
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
