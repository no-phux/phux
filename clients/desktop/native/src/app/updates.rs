//! One process-wide updater; window subscriptions repaint on its activity.
use std::path::PathBuf;
use std::time::Duration;

use gpui::{Context, Task};

use crate::updates::{self, Preferences, Release, RuntimeRelease};

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
    _automatic: Option<Task<()>>,
}

pub(crate) fn page(
    entity: &gpui::Entity<Updater>,
    cx: &gpui::App,
) -> gpui_kit::component::setting::SettingPage {
    use gpui::prelude::*;
    use gpui::{div, px};
    use gpui_kit::component::button::{Button, ButtonVariants};
    use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
    use gpui_kit::component::{Disableable, Sizable};
    let updater = entity.read(cx);
    let automatic_read = entity.clone();
    let automatic_write = entity.clone();
    let manage_read = entity.clone();
    let manage_write = entity.clone();
    let busy = updater.busy;
    let desktop_enabled = !busy && !updater.development && updater.desktop_available();
    let runtime_enabled = !busy
        && !updater.development
        && updater.prefs.manage_runtime
        && updater.runtime_available();
    let check = entity.clone();
    let install = entity.clone();
    let runtime_install = entity.clone();
    let runtime_label = if updater.pending_handoff {
        "Retry server handoff"
    } else {
        "Update phux & hand off server"
    };
    let message = updater.message.clone();
    let current = updater
        .current
        .as_ref()
        .map_or("Development build".into(), |release| {
            format!(
                "{} · {} · {}",
                release.version,
                release.channel.name(),
                release.revision.chars().take(8).collect::<String>()
            )
        });
    let latest = updater
        .target
        .as_ref()
        .map_or("Not checked".into(), |release| {
            format!(
                "{} {}",
                release.version,
                release.revision.chars().take(8).collect::<String>()
            )
        });
    let runtime =
        updater
            .runtime
            .as_ref()
            .map_or("Updates remain under your control.".into(), |runtime| {
                format!(
                    "{} → {}. {}",
                    runtime.current,
                    runtime.target,
                    runtime
                        .native_command
                        .as_deref()
                        .unwrap_or("Verified updates use phux’s live-server handoff.")
                )
            });
    let development = updater.development;
    SettingPage::new("Updates")
        .group(SettingGroup::new().title("Desktop")
            .description(format!("Installed: {current}. Selected release: {latest}."))
            .item(SettingItem::new("Automatic checks", SettingField::switch(
                move |cx| automatic_read.read(cx).prefs.automatic,
                move |value, cx| { automatic_write.update(cx, |updater, cx| updater.preferences(|prefs| prefs.automatic = value, cx)); },
            )).description("Check at launch and every six hours. Downloads and restarts only happen when you click."))
            .item(channel_item(entity, false, busy, updater.prefs.desktop))
            .item(SettingItem::render(move |_, _, _| {
                let check = check.clone();
                let install = install.clone();
                div().flex().flex_col().gap(px(8.)).text_sm()
                    .child(message.clone())
                    .when(development, |div| div.child("Development/explicit-target launch: installation is disabled."))
                    .child(div().flex().gap(px(8.))
                        .child(Button::new("updates-check").label("Check now").small().disabled(busy)
                            .on_click(move |_, _, cx| check.update(cx, |updater, cx| updater.check(cx))))
                        .child(Button::new("updates-install").label("Update & restart").primary().small().disabled(!desktop_enabled)
                            .on_click(move |_, _, cx| install.update(cx, |updater, cx| updater.install_desktop(cx)))))
                    .into_any_element()
            })))
        .group(SettingGroup::new().title("Local phux runtime")
            .description("Optional. This app can run phux’s existing updater for this Mac, never remote hosts. Turning management off leaves the installed release and live sessions alone.")
            .item(SettingItem::new("Manage phux updates", SettingField::switch(
                move |cx| manage_read.read(cx).prefs.manage_runtime,
                move |value, cx| { manage_write.update(cx, |updater, cx| updater.preferences(|prefs| prefs.manage_runtime = value, cx)); },
            )))
            .item(channel_item(entity, true, busy, updater.prefs.runtime))
            .item(SettingItem::render(move |_, _, _| {
                let install = runtime_install.clone();
                div().flex().flex_col().gap(px(8.)).text_sm().child(runtime.clone())
                    .child(Button::new("runtime-update").label(runtime_label).small().disabled(!runtime_enabled)
                        .on_click(move |_, _, cx| install.update(cx, |updater, cx| updater.install_runtime(cx))))
                    .into_any_element()
            })))
}

fn channel_item(
    entity: &gpui::Entity<Updater>,
    runtime: bool,
    busy: bool,
    selected: updates::Channel,
) -> gpui_kit::component::setting::SettingItem {
    use gpui::prelude::*;
    use gpui::{div, px};
    use gpui_kit::component::button::{Button, ButtonVariants};
    use gpui_kit::component::setting::SettingItem;
    use gpui_kit::component::{Disableable, Sizable};
    let entity = entity.clone();
    SettingItem::render(move |_, _, _| {
        div().flex().items_center().gap(px(8.)).text_sm().child("Channel")
            .children([updates::Channel::Stable, updates::Channel::Next].into_iter().map(|channel| {
                let entity = entity.clone();
                let id = format!("{}-{}", if runtime { "runtime" } else { "desktop" }, channel.name());
                Button::new(id).label(if channel == updates::Channel::Stable { "Stable" } else { "Next" })
                    .small().map(|button| if channel == selected { button.primary() } else { button })
                    .disabled(busy)
                    .on_click(move |_, _, cx| entity.update(cx, |updater, cx| updater.preferences(|prefs| {
                        if runtime { prefs.runtime = channel; } else { prefs.desktop = channel; }
                    }, cx)))
            })).into_any_element()
    }).description("Stable is the latest published release. Next follows verified main builds. Switch back to Stable here at any time.")
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
            _automatic: None,
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

    pub fn install_desktop(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.development || !self.desktop_available() {
            return;
        }
        let Some((bundle, target)) = self.bundle.clone().zip(self.target.clone()) else {
            return;
        };
        self.busy = true;
        self.message = "Downloading and verifying desktop update…".into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || {
                // Next retains only recent revisions. Resolve again at the
                // click, then pin that exact artifact for the whole install.
                let target = if target.channel == updates::Channel::Next {
                    updates::latest(updates::Channel::Next)?
                } else {
                    target
                };
                updates::desktop_install(&bundle, &target)?;
                updates::restart(&bundle)
            })
            .await;
            let _ = this.update(cx, |updater, cx| {
                updater.busy = false;
                match result {
                    Ok(()) => cx.quit(),
                    Err(error) => {
                        updater.message = format!("Update failed: {error:#}");
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub fn install_runtime(&mut self, cx: &mut Context<Self>) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_check_clears_stale_targets_but_preserves_independent_success() {
        let mut updater = Updater::new();
        updater.current = Some(Release {
            version: "0.2.0".into(),
            revision: String::new(),
            channel: updates::Channel::Stable,
        });
        let target = Release {
            version: "0.3.0".into(),
            revision: String::new(),
            channel: updates::Channel::Stable,
        };
        updater.busy = true;
        updater.record_check(Ok(target), Some(Err(anyhow::anyhow!("offline"))));
        assert!(!updater.busy);
        assert!(updater.desktop_available());
        assert!(updater.message.contains("phux: offline"));
        updater.record_check(Err(anyhow::anyhow!("rate limited")), None);
        assert!(updater.target.is_none());
        assert!(updater.runtime.is_none());
        assert!(!updater.desktop_available());
    }

    #[test]
    fn opting_out_does_not_start_another_check() {
        let mut before = Preferences::default();
        let mut after = before.clone();
        after.automatic = false;
        assert!(!needs_recheck(&before, &after));
        before.manage_runtime = true;
        after = before.clone();
        after.manage_runtime = false;
        assert!(!needs_recheck(&before, &after));
        after.desktop = updates::Channel::Next;
        assert!(needs_recheck(&before, &after));
    }

    #[gpui::test]
    fn development_and_unmanaged_runtime_cannot_install(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext;
        let updater = cx.new(|_| Updater::new());
        updater.update(cx, |updater, cx| {
            updater.prefs.manage_runtime = true;
            updater.runtime = Some(RuntimeRelease {
                current: "1.0.0".into(),
                target: "2.0.0".into(),
                available: true,
                mutable: true,
                native_command: None,
            });
            updater.install_runtime(cx);
            assert!(!updater.busy);
            updater.development = false;
            updater.prefs.manage_runtime = false;
            updater.install_runtime(cx);
            assert!(!updater.busy);
            updater.install_desktop(cx);
            assert!(!updater.busy);
        });
    }
}
