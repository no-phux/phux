//! One process-wide updater; window subscriptions repaint on its activity.
use std::path::PathBuf;
use std::time::Duration;

use gpui::{Context, Task};

use super::color::from_hex;
use crate::settings::theme::Palette;
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

/// A status pill's tone on the Updates page.
#[derive(Clone, Copy)]
enum Tone {
    Quiet,
    Good,
    Ready,
    Bad,
}

fn pill(text: String, tone: Tone, colors: &Palette) -> gpui::Div {
    use gpui::prelude::*;
    use gpui::{div, px};
    let (fg, bg) = match tone {
        Tone::Quiet => (colors.muted.clone(), colors.hover.clone()),
        Tone::Good => (colors.theme.success.clone(), colors.hover.clone()),
        Tone::Ready => (colors.accent_text.clone(), colors.theme.accent.clone()),
        Tone::Bad => (colors.theme.danger.clone(), colors.hover.clone()),
    };
    div()
        .flex_shrink_0()
        .px(px(8.))
        .py(px(2.))
        .rounded(px(10.))
        .bg(from_hex(&bg))
        .text_xs()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(from_hex(&fg))
        .child(text)
}

/// A product's headline: name, the installed build in mono, a status pill, and
/// its actions on the right. A note or failure sits under it as one callout,
/// bordered in danger only for a failure.
fn status_header(
    title: &'static str,
    build: String,
    status: (String, Tone),
    detail: Option<(String, bool)>,
    actions: Vec<gpui::AnyElement>,
    colors: &Palette,
) -> gpui::AnyElement {
    use gpui::prelude::*;
    use gpui::{div, px};
    let (status, tone) = status;

    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .text_base()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(from_hex(&colors.theme.foreground))
                                        .child(title),
                                )
                                .child(pill(status, tone, colors)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_family("monospace")
                                .text_color(from_hex(&colors.muted))
                                .child(build),
                        ),
                )
                .child(div().flex().items_center().gap(px(6.)).children(actions)),
        )
        .when_some(detail, |header, (detail, failed)| {
            let edge = if failed {
                colors.theme.danger.clone()
            } else {
                colors.border.clone()
            };
            header.child(
                div()
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(from_hex(&edge))
                    .bg(from_hex(&colors.hover))
                    .text_xs()
                    .text_color(from_hex(&colors.subtext))
                    .child(detail),
            )
        })
        .into_any_element()
}

/// The failure line a check left for `prefix`, without its prefix.
fn failure(message: &str, prefix: &str) -> Option<String> {
    message
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .map(|line| line.trim().to_owned())
}

pub(crate) fn page(
    entity: &gpui::Entity<Updater>,
    colors: &Palette,
    cx: &gpui::App,
) -> gpui_kit::component::setting::SettingPage {
    use gpui::prelude::*;
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
        "Retry handoff"
    } else {
        "Update & hand off"
    };
    let build = updater
        .current
        .as_ref()
        .map_or("development build".into(), |release| {
            format!(
                "{} · {} · {}",
                release.version,
                release.channel.name(),
                release.revision.chars().take(8).collect::<String>()
            )
        });
    let desktop_failure = failure(&updater.message, "Desktop:");
    let desktop_status = if busy {
        ("Checking…".to_owned(), Tone::Quiet)
    } else if updater.development {
        ("Development".to_owned(), Tone::Quiet)
    } else if desktop_failure.is_some() {
        ("Check failed".to_owned(), Tone::Bad)
    } else if updater.desktop_available() {
        let version = updater
            .target
            .as_ref()
            .map_or(String::new(), |release| format!("{} ", release.version));
        (format!("{version}available"), Tone::Ready)
    } else if updater.target.is_some() {
        ("Up to date".to_owned(), Tone::Good)
    } else {
        ("Not checked".to_owned(), Tone::Quiet)
    };
    let runtime_failure = failure(&updater.message, "phux:");
    let runtime_build = updater.runtime.as_ref().map_or_else(
        || "managed by the phux CLI".to_owned(),
        |runtime| format!("{} → {}", runtime.current, runtime.target),
    );
    let runtime_status = if !updater.prefs.manage_runtime {
        ("Not managed".to_owned(), Tone::Quiet)
    } else if runtime_failure.is_some() {
        ("Check failed".to_owned(), Tone::Bad)
    } else if updater.runtime_available() {
        ("Update available".to_owned(), Tone::Ready)
    } else if updater.runtime.is_some() {
        ("Up to date".to_owned(), Tone::Good)
    } else {
        ("Not checked".to_owned(), Tone::Quiet)
    };
    let runtime_note = updater
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.native_command.clone());
    let development = updater.development;
    let (desktop_colors, runtime_colors) = (colors.clone(), colors.clone());
    SettingPage::new("Updates")
        .group(SettingGroup::new().title("Desktop")
            .item(SettingItem::render(move |_, _, _| {
                let check = check.clone();
                let install = install.clone();
                let mut actions = vec![Button::new("updates-check").label("Check now").ghost().small().disabled(busy)
                    .on_click(move |_, _, cx| check.update(cx, |updater, cx| updater.check(cx)))
                    .into_any_element()];
                if desktop_enabled {
                    actions.push(Button::new("updates-install").label("Update & restart").primary().small()
                        .on_click(move |_, _, cx| install.update(cx, |updater, cx| updater.install_desktop(cx)))
                        .into_any_element());
                }
                let detail = desktop_failure.clone().map(|failure| (failure, true)).or_else(|| development.then(|| ("Launched from a checkout or an explicit target, so installing is off.".to_owned(), false)));
                status_header("Phux Desktop", build.clone(), desktop_status.clone(), detail, actions, &desktop_colors)
            }))
            .item(SettingItem::new("Automatic checks", SettingField::switch(
                move |cx| automatic_read.read(cx).prefs.automatic,
                move |value, cx| { automatic_write.update(cx, |updater, cx| updater.preferences(|prefs| prefs.automatic = value, cx)); },
            )).description("At launch and every six hours. Nothing downloads or restarts until you click."))
            .item(channel_item(entity, false, busy, updater.prefs.desktop, colors)))
        .group(SettingGroup::new().title("phux on this Mac")
            .item(SettingItem::render(move |_, _, _| {
                let install = runtime_install.clone();
                let mut actions = Vec::new();
                if runtime_enabled {
                    actions.push(Button::new("runtime-update").label(runtime_label).primary().small()
                        .on_click(move |_, _, cx| install.update(cx, |updater, cx| updater.install_runtime(cx)))
                        .into_any_element());
                }
                let detail = runtime_failure.clone().map(|failure| (failure, true));
                let header = status_header("phux CLI & server", runtime_build.clone(), runtime_status.clone(), detail, actions, &runtime_colors);
                match runtime_note.clone() {
                    Some(note) => gpui::div().flex().flex_col().gap(gpui::px(6.)).child(header)
                        .child(gpui::div().text_xs().text_color(from_hex(&runtime_colors.muted)).child(note)).into_any_element(),
                    None => header,
                }
            }))
            .item(SettingItem::new("Manage phux updates", SettingField::switch(
                move |cx| manage_read.read(cx).prefs.manage_runtime,
                move |value, cx| { manage_write.update(cx, |updater, cx| updater.preferences(|prefs| prefs.manage_runtime = value, cx)); },
            )).description("Let this app run phux’s own updater for this Mac and hand the live server off. Never touches remote hosts. Off leaves the install and sessions alone."))
            .item(channel_item(entity, true, busy, updater.prefs.runtime, colors)))
}

/// Stable and Next as one segmented control, beside its title like every other row.
fn channel_item(
    entity: &gpui::Entity<Updater>,
    runtime: bool,
    busy: bool,
    selected: updates::Channel,
    colors: &Palette,
) -> gpui_kit::component::setting::SettingItem {
    use gpui::prelude::*;
    use gpui::{div, px};
    use gpui_kit::component::setting::{SettingField, SettingItem};
    let entity = entity.clone();
    let colors = colors.clone();
    SettingItem::new(
        "Channel",
        SettingField::render(move |_, _, _| {
            div()
                .flex()
                .p(px(2.))
                .gap(px(2.))
                .rounded(px(7.))
                .bg(from_hex(&colors.hover))
                .children(
                    [updates::Channel::Stable, updates::Channel::Next]
                        .into_iter()
                        .map(|channel| {
                            let entity = entity.clone();
                            let on = channel == selected;
                            let id = format!(
                                "{}-{}",
                                if runtime { "runtime" } else { "desktop" },
                                channel.name()
                            );
                            div()
                                .id(gpui::SharedString::from(id))
                                .px(px(12.))
                                .py(px(3.))
                                .rounded(px(5.))
                                .text_sm()
                                .when(on, |segment| {
                                    segment
                                        .bg(from_hex(&colors.raised))
                                        .shadow_sm()
                                        .text_color(from_hex(&colors.theme.foreground))
                                })
                                .when(!on, |segment| segment.text_color(from_hex(&colors.muted)))
                                .when(!busy && !on, |segment| {
                                    segment
                                        .cursor_pointer()
                                        .hover(|style| {
                                            style.text_color(from_hex(&colors.theme.foreground))
                                        })
                                        .on_click(move |_, _, cx| {
                                            entity.update(cx, |updater, cx| {
                                                updater.preferences(
                                                    |prefs| {
                                                        if runtime {
                                                            prefs.runtime = channel;
                                                        } else {
                                                            prefs.desktop = channel;
                                                        }
                                                    },
                                                    cx,
                                                )
                                            })
                                        })
                                })
                                .child(if channel == updates::Channel::Stable {
                                    "Stable"
                                } else {
                                    "Next"
                                })
                        }),
                )
        }),
    )
    .description(
        "Stable is the latest release. Next follows verified main builds; switch back any time.",
    )
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
