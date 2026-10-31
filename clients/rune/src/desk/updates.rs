//! The updater's state: one worker at a time, its results drained on Rune's
//! poll. Downloading never restarts anything; that stays a separate click.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use rune::prefs::Prefs;
use rune::{Tx, Waker};

use super::Desk;
use crate::diag;
use crate::launch::Resolved;
use crate::updates::install::{self, Progress};
use crate::updates::phux::{self, Cli, Handoff};
use crate::updates::{self, Release};

const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const COPIED_FOR: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Work {
    Idle,
    Checking,
    Downloading(u8),
    Verifying,
    /// phux installs and hands its server off.
    Server,
}

/// The one update button. A click prepares everything due; restarting the
/// app is always its own click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Download,
    Restart,
    UpdateServer {
        retry: bool,
    },
    /// The app downloads now; the server moves at the restart.
    DownloadForBoth,
    RestartAndUpdateServer,
}

impl Action {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Download => "Download update",
            Self::Restart => "Restart now",
            Self::UpdateServer { retry: false } => "Update server",
            Self::UpdateServer { retry: true } => "Retry server handoff",
            Self::DownloadForBoth => "Download Phux Rune",
            Self::RestartAndUpdateServer => "Restart & update server",
        }
    }
}

enum Done {
    Tick,
    Checked(Result<Release, String>, Option<Result<Cli, String>>),
    Progress(Progress),
    Staged(Result<Release, String>),
    Server(Result<Handoff, String>, bool),
    CopiedShown,
}

pub struct Updater {
    pub automatic: bool,
    pub manage_phux: bool,
    /// The installed app, or why this copy can't replace itself.
    pub install: Result<PathBuf, &'static str>,
    /// The CLI and socket this window may update; absent when aimed by hand.
    pub(super) target: Option<(PathBuf, PathBuf)>,
    pub work: Work,
    pub release: Option<Release>,
    pub cli: Option<Cli>,
    /// The version waiting for this process to exit.
    pub staged: Option<String>,
    pub pending_handoff: bool,
    pub message: Option<String>,
    /// Someone asked: the card answers even when the answer is "up to date".
    asked: bool,
    /// The version whose card was closed; a newer one reopens it.
    dismissed: Option<String>,
    /// Diagnostics just went to the clipboard; the button says so for a moment.
    pub copied: bool,
    done: (Sender<Done>, Receiver<Done>),
    waker: Option<Waker>,
}

impl Updater {
    pub fn new(prefs: &Prefs, resolved: &Resolved) -> Self {
        let install = std::env::current_exe()
            .map_err(|_| "This copy can't find itself.")
            .and_then(|exe| updates::installation(&exe));
        let socket = resolved.socket.as_ref().ok().cloned();
        Self {
            automatic: prefs.get("update_automatic").unwrap_or(true),
            manage_phux: prefs.get("update_phux").unwrap_or(true),
            target: resolved
                .phux
                .clone()
                .zip(socket)
                .filter(|_| install.is_ok() && !resolved.aimed),
            install,
            work: Work::Idle,
            release: None,
            cli: None,
            staged: None,
            pending_handoff: false,
            message: None,
            asked: false,
            dismissed: None,
            copied: false,
            done: channel(),
            waker: None,
        }
    }

    fn worker(&self, job: impl FnOnce(&dyn Fn(Done)) + Send + 'static) {
        let sender = self.done.0.clone();
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let send = |done| {
                let _ = sender.send(done);
                if let Some(waker) = &waker {
                    waker();
                }
            };
            job(&send);
        });
    }

    /// Rune's setup hook: clear what a reboot left, check now, and every six
    /// hours. Nothing runs for a copy that can't update.
    pub fn start(&mut self, waker: Waker) {
        self.waker = Some(waker);
        let Ok(path) = self.install.clone() else {
            return;
        };
        self.worker(move |send| {
            install::clear_stale(&path);
            loop {
                send(Done::Tick);
                std::thread::sleep(CHECK_EVERY);
            }
        });
    }

    fn show_copied(&mut self, shown_for: Duration) {
        self.copied = true;
        self.worker(move |send| {
            std::thread::sleep(shown_for);
            send(Done::CopiedShown);
        });
    }

    pub fn busy(&self) -> bool {
        self.work != Work::Idle
    }

    /// A check someone asked for, from Settings or the palette.
    pub fn check_now(&mut self) {
        self.asked = true;
        self.dismissed = None;
        self.check();
    }

    pub fn check(&mut self) {
        if self.busy() {
            return;
        }
        if let Err(reason) = self.install {
            self.message = Some(reason.to_owned());
            return;
        }
        self.work = Work::Checking;
        self.message = None;
        let cli = self.managed().map(|(phux, _)| phux);
        self.worker(move |send| {
            let release = updates::latest();
            let cli = cli.map(|phux| phux::check(&phux));
            send(Done::Checked(release, cli));
        });
    }

    fn managed(&self) -> Option<(PathBuf, PathBuf)> {
        self.target.clone().filter(|_| self.manage_phux)
    }

    /// Whether this window updates phux at all, whatever the switch says.
    pub fn can_manage_phux(&self) -> bool {
        self.target.is_some()
    }

    pub fn app_due(&self) -> bool {
        self.install.is_ok()
            && self
                .release
                .as_ref()
                .is_some_and(Release::newer_than_installed)
    }

    pub fn phux_due(&self) -> bool {
        self.managed().is_some()
            && (self.pending_handoff || self.cli.as_ref().is_some_and(|c| c.available && c.mutable))
    }

    pub fn action(&self) -> Option<Action> {
        if self.busy() {
            return None;
        }
        match (self.staged.is_some(), self.app_due(), self.phux_due()) {
            (true, _, true) => Some(Action::RestartAndUpdateServer),
            (true, _, false) => Some(Action::Restart),
            (false, true, true) => Some(Action::DownloadForBoth),
            (false, true, false) => Some(Action::Download),
            (false, false, true) => Some(Action::UpdateServer {
                retry: self.pending_handoff,
            }),
            (false, false, false) => None,
        }
    }

    /// `(the server now, incoming)` when the new app speaks another protocol
    /// major and this update won't move the server there.
    pub fn protocol_warning(&self) -> Option<(String, String)> {
        let incoming = self.release.as_ref().filter(|_| self.app_due())?;
        let cli = self.cli.as_ref();
        let now = cli
            .and_then(|cli| cli.protocol_current.clone())
            .unwrap_or_else(updates::own_protocol);
        if updates::same_protocol_line(&now, &incoming.protocol) {
            return None;
        }
        let lands = cli
            .and_then(|cli| cli.protocol_target.as_deref())
            .is_some_and(|target| updates::same_protocol_line(target, &incoming.protocol));
        (!(lands && self.phux_due())).then(|| (now, incoming.protocol.clone()))
    }

    fn download(&mut self) {
        let (Ok(path), Some(release)) = (self.install.clone(), self.release.clone()) else {
            return;
        };
        self.work = Work::Downloading(0);
        self.message = None;
        self.worker(move |send| {
            let report = |progress| send(Done::Progress(progress));
            let result = install::stage(&path, &release, &report).map(|()| release);
            send(Done::Staged(result));
        });
    }

    fn update_server(&mut self, restart_after: bool) {
        let Some((phux, socket)) = self.managed() else {
            return;
        };
        let retry = self.pending_handoff;
        self.work = Work::Server;
        self.message = None;
        self.worker(move |send| {
            let result = if retry {
                phux::retry(&phux, &socket)
            } else {
                phux::install(&phux, &socket)
            };
            send(Done::Server(result, restart_after));
        });
    }

    pub fn card_open(&self) -> bool {
        let closed = self.dismissed.is_some() && self.dismissed == self.card_version();
        let live = self.action().is_some() || !matches!(self.work, Work::Idle | Work::Checking);
        (live || self.asked) && !closed
    }

    fn card_version(&self) -> Option<String> {
        let cli = self.cli.as_ref().map(|cli| cli.target.clone());
        self.release
            .as_ref()
            .map(|release| release.version.clone())
            .or(cli)
    }

    pub fn dismiss(&mut self) {
        self.asked = false;
        self.dismissed = self.card_version();
    }

    pub fn reopen(&mut self) {
        self.dismissed = None;
    }

    /// A staged app stays out while the server still needs moving, so a plain
    /// quit can't land a new app on an old server.
    fn sync_hold(&self) {
        if let (Ok(path), Some(_)) = (&self.install, &self.staged)
            && let Err(error) = install::hold(path, self.phux_due())
        {
            eprintln!("{error}");
        }
    }

    fn checked(&mut self, release: Result<Release, String>, cli: Option<Result<Cli, String>>) {
        self.work = Work::Idle;
        let mut failures = Vec::new();
        match release {
            Ok(release) => self.release = Some(release),
            Err(error) => failures.push(format!("Phux Rune: {error}")),
        }
        match cli {
            Some(Ok(cli)) => self.cli = Some(cli),
            Some(Err(error)) => failures.push(format!("phux: {error}")),
            None => {}
        }
        self.message = (!failures.is_empty()).then(|| failures.join("\n"));
        if let Some(message) = &self.message {
            diag::note(diag::EVENTS, &format!("update check failed: {message}"));
        }
        self.sync_hold();
    }

    fn server_done(&mut self, result: Result<Handoff, String>) -> bool {
        self.work = Work::Idle;
        let handoff = match result {
            Ok(handoff) => handoff,
            Err(error) => {
                diag::note(diag::EVENTS, &format!("server update failed: {error}"));
                self.message = Some(format!("phux: {error}"));
                return false;
            }
        };
        diag::note(
            diag::EVENTS,
            &format!("server update: done={} {}", handoff.done, handoff.detail),
        );
        self.pending_handoff = handoff.retry;
        if let Some(cli) = &mut self.cli {
            cli.current = cli.target.clone();
            cli.available = false;
            cli.protocol_current = cli.protocol_target.clone().or(cli.protocol_current.take());
        }
        self.message = (!handoff.done).then(|| {
            format!(
                "phux is installed, but the server stayed put: {}. No session was closed.",
                handoff.detail
            )
        });
        self.sync_hold();
        handoff.done
    }
}

impl Desk {
    /// Drain finished work. True when the window should repaint.
    pub(super) fn pump_updates(&mut self, tx: &mut Tx) -> bool {
        let mut changed = false;
        while let Ok(done) = self.updater.done.1.try_recv() {
            changed = true;
            let updater = &mut self.updater;
            match done {
                Done::Tick => {
                    if updater.automatic {
                        updater.check();
                    }
                }
                Done::Checked(release, cli) => updater.checked(release, cli),
                Done::Progress(Progress::Downloading(percent)) => {
                    updater.work = Work::Downloading(percent);
                }
                Done::Progress(Progress::Verifying) => updater.work = Work::Verifying,
                Done::Staged(result) => {
                    updater.work = Work::Idle;
                    match result {
                        Ok(release) => {
                            updater.staged = Some(release.version);
                            updater.sync_hold();
                        }
                        Err(error) => updater.message = Some(format!("Phux Rune: {error}")),
                    }
                }
                Done::CopiedShown => updater.copied = false,
                Done::Server(result, restart_after) => {
                    if updater.server_done(result) && restart_after {
                        self.restart(tx);
                    }
                }
            }
        }
        changed
    }

    /// Reopens on the session this window shows; the server keeps its panes.
    fn restart(&mut self, tx: &mut Tx) {
        let Ok(path) = self.updater.install.clone() else {
            return;
        };
        let session = self.selected_session_name();
        match install::relaunch_after_exit(&path, session.as_deref()) {
            Ok(()) => tx.quit(),
            Err(error) => self.updater.message = Some(error),
        }
    }

    pub fn update_act(&mut self, tx: &mut Tx) {
        match self.updater.action() {
            Some(Action::Download | Action::DownloadForBoth) => self.updater.download(),
            Some(Action::Restart) => self.restart(tx),
            Some(Action::UpdateServer { .. }) => self.updater.update_server(false),
            Some(Action::RestartAndUpdateServer) => self.updater.update_server(true),
            None => {}
        }
    }

    /// The palette's one verb: do what's due, or go and look.
    pub fn install_update(&mut self, tx: &mut Tx) {
        if self.updater.action().is_some() {
            self.updater.reopen();
            self.update_act(tx);
        } else {
            self.updater.check_now();
        }
    }

    /// The facts a bug report needs, and the logs, for the person to paste.
    pub fn copy_diagnostics(&mut self, tx: &mut Tx) {
        let updater = &self.updater;
        let install = match &updater.install {
            Ok(path) => path.display().to_string(),
            Err(reason) => (*reason).to_owned(),
        };
        let phux = updater.cli.as_ref().map(|cli| cli.current.clone());
        let facts = [
            ("connection", format!("{:?}", self.status)),
            ("connection error", self.error.clone().unwrap_or_default()),
            ("install", install),
            ("protocol", updates::own_protocol()),
            ("phux", phux.unwrap_or_default()),
            ("update work", format!("{:?}", updater.work)),
            (
                "update message",
                updater.message.clone().unwrap_or_default(),
            ),
        ];
        tx.copy(diag::report(&facts));
        self.updater.show_copied(COPIED_FOR);
    }

    pub fn toggle_update_automatic(&mut self) {
        self.updater.automatic = !self.updater.automatic;
        let _ = self.prefs.set("update_automatic", &self.updater.automatic);
        if self.updater.automatic {
            self.updater.check();
        }
    }

    pub fn toggle_update_phux(&mut self) {
        self.updater.manage_phux = !self.updater.manage_phux;
        let _ = self.prefs.set("update_phux", &self.updater.manage_phux);
        if self.updater.manage_phux {
            self.updater.check();
        } else {
            self.updater.cli = None;
            self.updater.pending_handoff = false;
            self.updater.sync_hold();
        }
    }
}

#[cfg(test)]
mod tests;
