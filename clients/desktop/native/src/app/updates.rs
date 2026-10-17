//! One process-wide updater; window subscriptions repaint on its activity.
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, SharedString, Subscription, Task, div, px};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::{Disableable, Sizable};

use super::color::from_hex;
use super::overlays::corner;
use super::skin::Look;
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

/// A header button; primary actions are filled, the rest ghost.
#[derive(Clone, Copy)]
struct Action {
    id: &'static str,
    label: &'static str,
    primary: bool,
    disabled: bool,
    run: fn(&mut Updater, &mut Context<Updater>),
}

/// A product's headline: name, the installed build in mono, a status pill, and
/// its actions on the right. A failure or detail sits under it as one callout,
/// bordered in danger only for a failure; a muted note may follow.
struct Header {
    title: &'static str,
    build: String,
    status: (String, Tone),
    detail: Option<(String, bool)>,
    note: Option<String>,
    actions: Vec<Action>,
}

impl Header {
    fn item(self, entity: &Entity<Updater>, look: &Rc<Look>, mono: &SharedString) -> SettingItem {
        let (entity, look, mono) = (entity.clone(), look.clone(), mono.clone());
        SettingItem::render(move |_, _, _| self.render(&entity, &look, &mono))
    }

    fn render(&self, entity: &Entity<Updater>, look: &Look, mono: &SharedString) -> AnyElement {
        let colors = &look.chrome;
        let (status, tone) = self.status.clone();
        let (fg, bg) = match tone {
            Tone::Quiet => (&colors.muted, &colors.hover),
            Tone::Good => (&colors.theme.success, &colors.hover),
            Tone::Ready => (&colors.accent_text, &colors.theme.accent),
            Tone::Bad => (&colors.theme.danger, &colors.hover),
        };
        let pill = div()
            .flex_shrink_0()
            .px(px(8.))
            .py(px(2.))
            .rounded(corner(look, 10.))
            .bg(from_hex(bg))
            .text_xs()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(from_hex(fg))
            .child(status);
        let actions = self.actions.iter().map(|action| {
            let (entity, run) = (entity.clone(), action.run);
            Button::new(action.id)
                .label(action.label)
                .small()
                .disabled(action.disabled)
                .map(|button| {
                    if action.primary {
                        button.primary()
                    } else {
                        button.ghost()
                    }
                })
                .on_click(move |_, _, cx| entity.update(cx, run))
        });
        let header = div()
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
                                            .child(self.title),
                                    )
                                    .child(pill),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font_family(mono.clone())
                                    .text_color(from_hex(&colors.muted))
                                    .child(self.build.clone()),
                            ),
                    )
                    .child(div().flex().items_center().gap(px(6.)).children(actions)),
            )
            .when_some(self.detail.clone(), |header, (detail, failed)| {
                let edge = if failed {
                    &colors.theme.danger
                } else {
                    &colors.border
                };
                header.child(
                    div()
                        .px(px(10.))
                        .py(px(8.))
                        .rounded(corner(look, 6.))
                        .border_1()
                        .border_color(from_hex(edge))
                        .bg(from_hex(&colors.hover))
                        .text_xs()
                        .text_color(from_hex(&colors.subtext))
                        .child(detail),
                )
            });
        match self.note.clone() {
            Some(note) => div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(header)
                .child(
                    div()
                        .text_xs()
                        .text_color(from_hex(&colors.muted))
                        .child(note),
                )
                .into_any_element(),
            None => header.into_any_element(),
        }
    }
}

/// The failure line a check left for `prefix`, without its prefix.
fn failure(message: &str, prefix: &str) -> Option<String> {
    message
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .map(|line| line.trim().to_owned())
}

/// One updater preference as a switch.
fn switch_item(
    entity: &Entity<Updater>,
    title: &'static str,
    pref: fn(&mut Preferences) -> &mut bool,
    description: &'static str,
) -> SettingItem {
    let (reader, writer) = (entity.clone(), entity.clone());
    let field = SettingField::switch(
        move |cx| *pref(&mut reader.read(cx).prefs.clone()),
        move |value, cx| {
            writer.update(cx, |updater, cx| {
                updater.preferences(|prefs| *pref(prefs) = value, cx)
            });
        },
    );
    SettingItem::new(title, field).description(description)
}

/// The Updates page in a skin, with build strings in the terminal `font`.
pub(crate) fn page(
    entity: &Entity<Updater>,
    look: &Rc<Look>,
    font: &str,
    cx: &gpui::App,
) -> SettingPage {
    let updater = entity.read(cx);
    let busy = updater.busy;
    let mono = SharedString::from(font.to_owned());
    let check = Action {
        id: "updates-check",
        label: "Check now",
        primary: false,
        disabled: busy,
        run: Updater::check,
    };
    let install = updater.desktop_action().map(|action| Action {
        id: "updates-install",
        label: action.label(),
        primary: true,
        disabled: false,
        run: Updater::desktop_act,
    });
    let runtime_enabled = !busy
        && !updater.development
        && updater.prefs.manage_runtime
        && updater.runtime_available();
    let runtime_install = runtime_enabled.then_some(Action {
        id: "runtime-update",
        label: if updater.pending_handoff {
            "Retry handoff"
        } else {
            "Update & hand off"
        },
        primary: true,
        disabled: false,
        run: Updater::install_runtime,
    });
    let desktop = Header {
        title: "Phux Desktop",
        build: updater
            .current
            .as_ref()
            .map_or("development build".into(), |release| {
                format!(
                    "{} · {} · {}",
                    release.version,
                    release.channel.as_str(),
                    release.revision.chars().take(8).collect::<String>()
                )
            }),
        status: updater.desktop_status(),
        detail: updater.desktop_detail(),
        note: None,
        actions: std::iter::once(check).chain(install).collect(),
    };
    let runtime = Header {
        title: "phux CLI & server",
        build: updater.runtime.as_ref().map_or_else(
            || "managed by the phux CLI".to_owned(),
            |runtime| format!("{} → {}", runtime.current, runtime.target),
        ),
        status: updater.runtime_status(),
        detail: failure(&updater.message, "phux:").map(|failure| (failure, true)),
        note: updater
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.native_command.clone()),
        actions: runtime_install.into_iter().collect(),
    };
    SettingPage::new("Updates")
        .group(
            SettingGroup::new()
                .title("Desktop")
                .item(desktop.item(entity, look, &mono))
                .item(switch_item(
                    entity,
                    "Automatic checks",
                    |prefs| &mut prefs.automatic,
                    "At launch and every six hours. Nothing downloads or restarts until you click.",
                ))
                .item(channel_item(entity, false, busy, updater.prefs.desktop, look)),
        )
        .group(
            SettingGroup::new()
                .title("phux on this Mac")
                .item(runtime.item(entity, look, &mono))
                .item(switch_item(
                    entity,
                    "Manage phux updates",
                    |prefs| &mut prefs.manage_runtime,
                    "Let this app run phux’s own updater for this Mac and hand the live server off. Never touches remote hosts. Off leaves the install and sessions alone.",
                ))
                .item(channel_item(entity, true, busy, updater.prefs.runtime, look)),
        )
}

/// Stable and Next as one segmented control, beside its title like every other row.
fn channel_item(
    entity: &Entity<Updater>,
    runtime: bool,
    busy: bool,
    selected: updates::Channel,
    look: &Look,
) -> SettingItem {
    let entity = entity.clone();
    let colors = look.chrome.clone();
    let outer = corner(look, 7.);
    let inner = corner(look, 5.);
    let lifts = !look.popover_shadow.is_empty();
    let pref: fn(&mut Preferences) -> &mut updates::Channel = if runtime {
        |prefs| &mut prefs.runtime
    } else {
        |prefs| &mut prefs.desktop
    };
    SettingItem::new(
        "Channel",
        SettingField::render(move |_, _, _| {
            div()
                .flex()
                .p(px(2.))
                .gap(px(2.))
                .rounded(outer)
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
                                channel.as_str()
                            );
                            div()
                                .id(SharedString::from(id))
                                .px(px(12.))
                                .py(px(3.))
                                .rounded(inner)
                                .text_sm()
                                .when(on, |segment| {
                                    segment
                                        .bg(from_hex(&colors.raised))
                                        .when(lifts, |segment| segment.shadow_sm())
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
                                                updater
                                                    .preferences(|prefs| *pref(prefs) = channel, cx)
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
                "This update changes the wire protocol from {now} to {next}; phux on this Mac must be updated too."
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

#[cfg(test)]
mod tests {
    use super::*;

    fn release(version: &str, protocol: &str) -> Release {
        Release {
            version: version.into(),
            protocol: protocol.into(),
            ..Release::default()
        }
    }

    fn installed(version: &str) -> Updater {
        let mut updater = Updater::new();
        updater.current = Some(release(version, &updates::own_protocol()));
        updater.development = false;
        updater
    }

    fn runtime(protocol_current: Option<&str>) -> RuntimeRelease {
        RuntimeRelease {
            current: "1.0.0".into(),
            target: "2.0.0".into(),
            available: true,
            mutable: true,
            native_command: None,
            protocol_current: protocol_current.map(str::to_owned),
            protocol_target: None,
        }
    }

    #[test]
    fn a_failed_check_clears_stale_targets_but_preserves_independent_success() {
        let mut updater = installed("0.2.0");
        updater.busy = true;
        updater.record_check(
            Ok(release("0.3.0", "0.9")),
            Some(Err(anyhow::anyhow!("offline"))),
        );
        assert!(!updater.busy);
        assert!(updater.desktop_available());
        assert!(updater.message.contains("phux: offline"));
        updater.record_check(Err(anyhow::anyhow!("rate limited")), None);
        assert!(updater.target.is_none());
        assert!(updater.runtime.is_none());
        assert!(!updater.desktop_available());
    }

    #[test]
    fn an_available_update_downloads_then_offers_restart() {
        let mut updater = installed("0.2.0");
        let protocol = updates::own_protocol();
        updater.record_check(Ok(release("0.3.0", &protocol)), None);
        assert_eq!(updater.desktop_action(), Some(DesktopAction::Download));
        updater.busy = true;
        assert_eq!(updater.desktop_action(), None);
        updater.record_stage(Ok(Staged {
            app: "/Applications/.phux-desktop-update/Phux.app".into(),
            release: release("0.3.0", &protocol),
        }));
        assert!(!updater.busy);
        assert_eq!(updater.desktop_action(), Some(DesktopAction::Restart));
        assert_eq!(DesktopAction::Restart.label(), "Restart to update");
        let (detail, failed) = updater.desktop_detail().unwrap();
        assert!(!failed);
        assert!(detail.contains("installs when you quit"), "{detail}");
        // A check after staging keeps the staged bundle.
        updater.record_check(Ok(release("0.3.0", &protocol)), None);
        assert_eq!(updater.desktop_action(), Some(DesktopAction::Restart));
    }

    #[test]
    fn a_failed_stage_is_shown_as_a_desktop_failure() {
        let mut updater = installed("0.2.0");
        updater.record_check(Ok(release("0.3.0", "0.9")), None);
        updater.record_stage(Err(anyhow::anyhow!("checksum mismatch")));
        assert!(updater.staged.is_none());
        let (detail, failed) = updater.desktop_detail().unwrap();
        assert!(failed);
        assert_eq!(detail, "Update failed: checksum mismatch");
    }

    #[test]
    fn switching_the_desktop_channel_drops_a_staged_update() {
        let mut updater = installed("0.2.0");
        updater.staged = Some(Staged {
            app: PathBuf::from("/x/Phux.app"),
            release: release("0.3.0", "0.9"),
        });
        let mut prefs = updater.prefs.clone();
        prefs.desktop = updates::Channel::Next;
        updater.adopt_preferences(prefs);
        assert!(updater.staged.is_none());
    }

    #[test]
    fn a_protocol_change_is_named_and_couples_a_managed_runtime() {
        let mut updater = installed("0.2.0");
        updater.record_check(Ok(release("0.3.0", "99.0")), None);
        let (now, next) = updater.protocol_change().unwrap();
        assert_eq!(
            (now.as_str(), next.as_str()),
            (updates::own_protocol().as_str(), "99.0")
        );
        let (detail, _) = updater.desktop_detail().unwrap();
        assert!(detail.contains("must be updated too"), "{detail}");
        updater.staged = Some(Staged {
            app: PathBuf::from("/x/Phux.app"),
            release: release("0.3.0", "99.0"),
        });
        assert_eq!(updater.desktop_action(), Some(DesktopAction::Restart));
        updater.prefs.manage_runtime = true;
        updater.runtime = Some(runtime(None));
        assert_eq!(
            updater.desktop_action(),
            Some(DesktopAction::RuntimeThenRestart)
        );
        // A runtime update that lands elsewhere cannot fix the mismatch.
        let mut elsewhere = runtime(None);
        elsewhere.protocol_target = Some("98.0".into());
        updater.runtime = Some(elsewhere);
        assert_eq!(updater.desktop_action(), Some(DesktopAction::Restart));
        // A server already on the incoming protocol needs no coupling.
        updater.runtime = Some(runtime(Some("99.0")));
        assert_eq!(updater.protocol_change(), None);
        assert_eq!(updater.desktop_action(), Some(DesktopAction::Restart));
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
            updater.runtime = Some(runtime(None));
            updater.install_runtime(cx);
            assert!(!updater.busy);
            updater.development = false;
            updater.prefs.manage_runtime = false;
            updater.install_runtime(cx);
            assert!(!updater.busy);
            updater.desktop_act(cx);
            assert!(!updater.busy);
        });
    }

    #[gpui::test]
    fn development_builds_never_stage_or_swap(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext;
        let updater = cx.new(|_| Updater::new());
        updater.update(cx, |updater, cx| {
            updater.current = Some(release("0.2.0", "0.9"));
            updater.target = Some(release("0.3.0", "0.9"));
            assert!(updater.development);
            assert_eq!(updater.desktop_action(), None);
            updater.desktop_act(cx);
            assert!(!updater.busy);
            updater.staged = Some(Staged {
                app: PathBuf::from("/x/Phux.app"),
                release: release("0.3.0", "0.9"),
            });
            updater.apply_staged();
            assert!(!updater.handed_off);
        });
    }
}
