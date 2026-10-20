//! The Updates page: live controls and desktop/runtime release actions.

use super::{Tone, Updater, failure};
use crate::app::color::from_hex;
use crate::app::overlays::corner;
use crate::app::settings_panel::catalog::{
    Content as SettingPage, Group as SettingGroup, Item as SettingItem,
};
use crate::app::skin::Look;
use crate::app::ui::muted;
use crate::updates::{self, Preferences};
use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, SharedString, div, px};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::setting::SettingField;
use gpui_kit::component::{Disableable, Sizable};
use std::rc::Rc;

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
        SettingItem::render(self.title, move |_, _, _| {
            self.render(&entity, &look, &mono)
        })
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
                                            .text_color(look.ink)
                                            .child(self.title),
                                    )
                                    .child(pill),
                            )
                            .child(muted(look, self.build.clone()).font_family(mono.clone())),
                    )
                    .child(div().flex().items_center().gap(px(6.)).children(actions)),
            )
            .when_some(self.detail.clone(), |header, (detail, failed)| {
                let edge = if failed {
                    from_hex(&colors.theme.danger)
                } else {
                    look.border
                };
                header.child(
                    div()
                        .px(px(10.))
                        .py(px(8.))
                        .rounded(corner(look, 6.))
                        .border_1()
                        .border_color(edge)
                        .bg(look.hover)
                        .text_xs()
                        .text_color(from_hex(&colors.subtext))
                        .child(detail),
                )
            });
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(header)
            .children(self.note.clone().map(|note| muted(look, note)))
            .into_any_element()
    }
}

/// One updater preference as a switch.
fn switch_item(
    entity: &Entity<Updater>,
    title: &'static str,
    pref: fn(&mut Preferences) -> &mut bool,
    description: &'static str,
) -> SettingItem {
    let (reader, writer) = (entity.clone(), entity.clone());
    SettingItem::switch(
        title,
        move |cx| *pref(&mut reader.read(cx).prefs.clone()),
        move |value, cx| {
            writer.update(cx, |updater, cx| {
                updater.preferences(|prefs| *pref(prefs) = value, cx)
            });
        },
        None,
    )
    .description(description)
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
    let install = updater
        .desktop_action()
        .filter(|_| !cfg!(target_os = "linux"))
        .map(|action| Action {
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
        note: cfg!(target_os = "linux").then(|| {
            updates::linux_install_remedy(updater.prefs.desktop, updater.bundle.as_deref())
        }),
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
                .title("phux on this computer")
                .item(runtime.item(entity, look, &mono))
                .item(switch_item(
                    entity,
                    "Manage phux updates",
                    |prefs| &mut prefs.manage_runtime,
                    "Let this app run phux’s own updater for this computer and hand the live server off. Never touches remote hosts. Off leaves the install and sessions alone.",
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
    look: &Rc<Look>,
) -> SettingItem {
    let (entity, look) = (entity.clone(), look.clone());
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
                .rounded(corner(&look, 7.))
                .bg(look.hover)
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
                                .rounded(corner(&look, 5.))
                                .text_sm()
                                .when(on, |segment| {
                                    segment
                                        .bg(from_hex(&look.chrome.raised))
                                        .when(!look.popover_shadow.is_empty(), |segment| {
                                            segment.shadow_sm()
                                        })
                                        .text_color(look.ink)
                                })
                                .when(!on, |segment| {
                                    segment.text_color(from_hex(&look.chrome.muted))
                                })
                                .when(!busy && !on, |segment| {
                                    segment
                                        .cursor_pointer()
                                        .hover(|style| style.text_color(look.ink))
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
