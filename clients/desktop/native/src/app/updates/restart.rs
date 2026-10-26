//! Quit, server handoff, and the explicit desktop restart.

use super::{RuntimeRelease, Updater, updates};
use gpui::Context;

impl Updater {
    /// Quitting with a staged update installs it after exit, unless its managed
    /// server still needs updating. Combined updates use the explicit restart.
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

    pub(super) fn apply_staged(&mut self) {
        if self.handed_off || self.development || self.phux_due() {
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

    /// A second click either finishes the server update or explicitly restarts.
    pub(super) fn finish_update(&mut self, cx: &mut Context<Self>) {
        if self.phux_due() {
            self.run_runtime(cx, self.staged.is_some());
        } else {
            self.restart_to_update(cx);
        }
    }

    pub(super) fn restart_to_update(&mut self, cx: &mut Context<Self>) {
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

    fn run_runtime(&mut self, cx: &mut Context<Self>, restart_after: bool) {
        if self.busy || self.development || !self.phux_due() {
            return;
        }
        let phux = self.phux.clone();
        let socket = self.socket.clone();
        let channel = self.prefs.runtime;
        let retry = self.pending_handoff;
        self.busy = true;
        self.work = super::Work::Installing;
        self.message = "Updating phux and handing the live server off…".into();
        cx.notify();
        let installed = cx.background_executor().spawn(async move {
            if retry {
                updates::runtime_retry(&phux, &socket)
            } else {
                updates::runtime_install(&phux, &socket, channel)
            }
        });
        cx.spawn(async move |this, cx| {
            let result = installed.await;
            let _ = this.update(cx, |updater, cx| {
                // The staged desktop needs the server phux just handed off to.
                if updater.record_runtime(result) && restart_after {
                    updater.restart_to_update(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Record a finished phux update; true once the live server was handed off.
    pub(super) fn record_runtime(
        &mut self,
        result: anyhow::Result<updates::RuntimeInstalled>,
    ) -> bool {
        self.busy = false;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                self.message = format!("phux: Update failed: {error:#}");
                return false;
            }
        };
        if !result.socket.as_os_str().is_empty() {
            self.socket = result.socket.clone();
        }
        self.pending_handoff = result.retry_handoff;
        // What was the target is installed now, unless only the handoff ran.
        self.runtime = self.runtime.take().map(|runtime| RuntimeRelease {
            current: runtime.target.clone(),
            available: false,
            protocol_current: runtime.protocol_target.clone().or(runtime.protocol_current),
            ..runtime
        });
        if result.reconnect {
            self.completed = true;
            self.runtime_generation += 1;
            self.message = "phux is updated and the server was handed off".into();
        } else {
            self.message = format!("{} {}", super::HANDOFF_NOTE, result.message);
        }
        result.reconnect
    }
}
