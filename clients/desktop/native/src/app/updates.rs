//! One process-wide updater; window subscriptions repaint on its activity.

#[cfg(test)]
mod tests;
mod view;

pub(crate) use view::page;

use std::path::PathBuf;
use std::time::Duration;

use crate::updates::{self, Preferences, Release, RuntimeRelease};
use gpui::{Context, Subscription, Task};

pub(crate) struct Updater {
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
    _automatic: Option<Task<()>>,
    _quit: Option<Subscription>,
}

/// A downloaded, verified desktop update ready to swap in.
#[derive(Clone, Debug)]
pub(crate) struct Staged {
    pub app: PathBuf,
    pub release: Release,
}

/// The Desktop row's primary action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DesktopAction {
    Download,
    Restart,
    /// The update changes the wire protocol: update phux first, then restart.
    RuntimeThenRestart,
}

impl DesktopAction {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Download => "Download update",
            Self::Restart => "Restart to update",
            Self::RuntimeThenRestart => "Update phux & restart",
        }
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
            _automatic: None,
            _quit: None,
        }
    }

    /// Quitting with a staged update installs it once this process has exited.
    pub fn watch_quit(&mut self, cx: &mut Context<Self>) {
        self._quit = Some(cx.on_app_quit(|updater, _| {
            updater.apply_staged();
            async {}
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

    /// The Desktop row's primary action, if any.
    pub fn desktop_action(&self) -> Option<DesktopAction> {
        if self.busy || self.development {
            return None;
        }
        if self.staged.is_some() {
            return Some(if self.couples_runtime() {
                DesktopAction::RuntimeThenRestart
            } else {
                DesktopAction::Restart
            });
        }
        self.desktop_available().then_some(DesktopAction::Download)
    }

    pub fn desktop_act(&mut self, cx: &mut Context<Self>) {
        if cfg!(target_os = "linux") {
            self.message =
                updates::linux_install_remedy(self.prefs.desktop, self.bundle.as_deref());
            cx.notify();
            return;
        }
        match self.desktop_action() {
            Some(DesktopAction::Download) => self.download_desktop(cx),
            Some(DesktopAction::Restart) => self.restart_to_update(cx),
            Some(DesktopAction::RuntimeThenRestart) => self.run_runtime(cx, true),
            None => {}
        }
    }

    /// The release a staged or checked update would bring in.
    fn incoming(&self) -> Option<&Release> {
        self.staged
            .as_ref()
            .map(|staged| &staged.release)
            .or(self.target.as_ref().filter(|_| self.desktop_available()))
    }

    /// `(speaking now, incoming)` when the incoming desktop speaks another wire
    /// protocol than the local server. Without a runtime check, the server is
    /// assumed to speak what this desktop links.
    pub fn protocol_change(&self) -> Option<(String, String)> {
        let incoming = self.incoming()?;
        let now = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.protocol_current.clone())
            .unwrap_or_else(updates::own_protocol);
        (incoming.protocol != now).then(|| (now, incoming.protocol.clone()))
    }

    /// Restart must first move phux: the protocol changes and phux is managed here.
    fn couples_runtime(&self) -> bool {
        let Some((_, incoming)) = self.protocol_change() else {
            return false;
        };
        // Updating phux helps only when it lands on the incoming protocol.
        let lands = self.runtime.as_ref().is_some_and(|runtime| {
            runtime
                .protocol_target
                .as_deref()
                .is_none_or(|target| target == incoming)
        });
        lands && self.prefs.manage_runtime && self.runtime_available()
    }

    fn desktop_status(&self) -> (String, Tone) {
        if self.busy {
            return ("Working…".to_owned(), Tone::Quiet);
        }
        if self.development {
            return ("Development".to_owned(), Tone::Quiet);
        }
        if failure(&self.message, "Desktop:").is_some() {
            return ("Check failed".to_owned(), Tone::Bad);
        }
        if let Some(staged) = &self.staged {
            return (
                format!("{} ready to install", staged.release.version),
                Tone::Ready,
            );
        }
        match &self.target {
            Some(target) if self.desktop_available() => {
                (format!("{} available", target.version), Tone::Ready)
            }
            Some(_) => ("Up to date".to_owned(), Tone::Good),
            None => ("Not checked".to_owned(), Tone::Quiet),
        }
    }

    fn runtime_status(&self) -> (String, Tone) {
        let (status, tone) = if !self.prefs.manage_runtime {
            ("Not managed", Tone::Quiet)
        } else if failure(&self.message, "phux:").is_some() {
            ("Check failed", Tone::Bad)
        } else if self.runtime_available() {
            ("Update available", Tone::Ready)
        } else if self.runtime.is_some() {
            ("Up to date", Tone::Good)
        } else {
            ("Not checked", Tone::Quiet)
        };
        (status.to_owned(), tone)
    }

    fn desktop_detail(&self) -> Option<(String, bool)> {
        if let Some(failure) = failure(&self.message, "Desktop:") {
            return Some((failure, true));
        }
        if self.development {
            return Some((
                "Launched from a checkout or an explicit target, so installing is off.".to_owned(),
                false,
            ));
        }
        let protocol = self.protocol_change().map(|(now, next)| {
            format!(
                "This update changes the wire protocol from {now} to {next}; phux on this computer must be updated too."
            )
        });
        let staged = self.staged.as_ref().map(|staged| {
            format!(
                "Downloaded and verified {}. Restart to update, or it installs when you quit.",
                staged.release.version
            )
        });
        let text = [staged, protocol]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        (!text.is_empty()).then_some((text, false))
    }

    fn download_desktop(&mut self, cx: &mut Context<Self>) {
        let Some((bundle, target)) = self.bundle.clone().zip(self.target.clone()) else {
            return;
        };
        self.busy = true;
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
                updater.record_stage(result);
                cx.notify();
            });
        })
        .detach();
    }

    fn record_stage(&mut self, result: anyhow::Result<Staged>) {
        self.busy = false;
        match result {
            Ok(staged) => {
                self.message = format!("Desktop {} is ready to install", staged.release.version);
                self.staged = Some(staged);
            }
            Err(error) => self.message = format!("Desktop: Update failed: {error:#}"),
        }
    }

    fn restart_to_update(&mut self, cx: &mut Context<Self>) {
        if self.staged.is_none() || self.development {
            return;
        }
        self.relaunch = true;
        cx.quit();
    }

    pub fn install_runtime(&mut self, cx: &mut Context<Self>) {
        self.run_runtime(cx, false);
    }

    fn run_runtime(&mut self, cx: &mut Context<Self>, then_restart: bool) {
        if self.busy || self.development || !self.prefs.manage_runtime {
            return;
        }
        if !self.runtime_available() {
            return;
        }
        let phux = self.phux.clone();
        let socket = self.socket.clone();
        let channel = self.prefs.runtime;
        let retry = self.pending_handoff;
        let restart = then_restart && self.staged.is_some();
        self.busy = true;
        self.message = "Updating phux and handing off the live server…".into();
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
                updater.busy = false;
                updater.message = match result {
                    Ok(result) => {
                        updater.runtime = None;
                        updater.pending_handoff = result.retry_handoff;
                        if result.reconnect {
                            updater.runtime_generation += 1;
                        }
                        // The staged desktop needs the server it just handed off to.
                        if restart && result.reconnect {
                            updater.relaunch = true;
                            cx.quit();
                        }
                        result.message
                    }
                    Err(error) => format!("phux update failed: {error:#}"),
                };
                cx.notify();
            });
        })
        .detach();
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
