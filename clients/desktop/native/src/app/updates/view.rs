//! The Updates page: one overview with the single update button, then the
//! preferences that shape what that button does.

use super::{Tone, Updater};
use crate::app::color::from_hex;
use crate::app::overlays::corner;
use crate::app::settings_panel::catalog::{
    Content as SettingPage, Group as SettingGroup, Item as SettingItem,
};
use crate::app::skin::Look;
use crate::app::ui::segmented::{Segment, segmented};
use crate::updates::{self, Channel, Preferences};
use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt as _, AnyElement, Entity, Hsla, SharedString, Transformation, div,
    percentage, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::setting::SettingField;
use gpui_kit::component::{Disableable, Icon, Sizable};
use std::rc::Rc;
use std::time::Duration;

/// Fixed labels align on wide rows; versions and status wrap below on narrow ones.
const NAME_COLUMN: f32 = 148.;
const CHANNEL_COLUMN: f32 = 64.;
const STATE_COLUMN: f32 = 112.;

/// One installed product: its versions, channel and state.
struct Product {
    name: &'static str,
    installed: String,
    incoming: Option<String>,
    channel: Option<Channel>,
    state: (String, Tone),
}

/// Everything the overview paints, read from the updater once per frame.
struct Overview {
    headline: (String, Tone),
    detail: Option<String>,
    busy: bool,
    /// The busy mark spins: Animations is on and the system does not reduce motion.
    moves: bool,
    development: bool,
    action: Option<&'static str>,
    products: [Product; 2],
    notices: Vec<(String, bool)>,
}

impl Overview {
    fn of(updater: &Updater, moves: bool) -> Self {
        let (installed, incoming) = updater.desktop_versions();
        let desktop = Product {
            name: "Phux Desktop",
            installed,
            incoming,
            channel: updater.current.as_ref().map(|_| updater.prefs.desktop),
            state: updater.desktop_status(),
        };
        let (installed, incoming) = updater.runtime_versions();
        let runtime = Product {
            name: "phux CLI & server",
            installed,
            incoming,
            channel: updater
                .prefs
                .manage_runtime
                .then_some(updater.prefs.runtime),
            state: updater.runtime_status(),
        };
        Self {
            headline: (updater.card_headline(), updater.summary().1),
            detail: updater.summary_detail(),
            busy: updater.busy,
            moves,
            development: updater.development,
            action: updater.action_label(),
            products: [desktop, runtime],
            notices: updater.notices(),
        }
    }
}

pub(super) fn tone_color(look: &Look, tone: Tone) -> Hsla {
    let theme = &look.chrome.theme;
    match tone {
        Tone::Quiet => from_hex(&look.chrome.muted),
        Tone::Good => from_hex(&theme.success),
        Tone::Ready => from_hex(&theme.accent),
        Tone::Bad => from_hex(&theme.danger),
    }
}

/// The headline's mark: a tinted tile with the state's icon.
fn glyph(look: &Look, overview: &Overview) -> impl IntoElement {
    let (_, tone) = overview.headline;
    let icon = match tone {
        _ if overview.busy => IconName::Loader,
        _ if overview.development => IconName::Wrench,
        Tone::Ready => IconName::ArrowDown,
        Tone::Good => IconName::Check,
        Tone::Bad => IconName::TriangleAlert,
        Tone::Quiet => IconName::RefreshCw,
    };
    let color = tone_color(look, tone);
    let icon = Icon::new(icon).size(px(18.)).text_color(color);
    let icon = if overview.busy && overview.moves {
        icon.with_animation(
            "updates-working",
            Animation::new(Duration::from_millis(900)).repeat(),
            |icon, turn| icon.transform(Transformation::rotate(percentage(turn))),
        )
        .into_any_element()
    } else {
        icon.into_any_element()
    };
    div()
        .flex_shrink_0()
        .size(px(36.))
        .rounded(corner(look, 10.))
        .bg(color.opacity(0.14))
        .flex()
        .items_center()
        .justify_center()
        .child(icon)
}

/// A product's row in the grid: name, installed → incoming in mono, channel, state.
fn product_row(look: &Look, mono: &SharedString, product: &Product) -> impl IntoElement {
    let muted = from_hex(&look.chrome.muted);
    let (state, tone) = &product.state;
    let color = tone_color(look, *tone);
    let name = product.name;
    let versions = div()
        .flex_1()
        .min_w(px(120.))
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.))
        .font_family(mono.clone())
        // A monospace face reads larger than the UI face at the same size.
        .text_size(px(12.5))
        .child(
            div()
                .debug_selector(move || format!("updates-installed-{name}"))
                .text_color(if product.incoming.is_some() {
                    muted
                } else {
                    look.ink
                })
                .child(product.installed.clone()),
        )
        .when_some(product.incoming.clone(), |versions, incoming| {
            versions
                .child(
                    Icon::new(IconName::ArrowRight)
                        .size(px(12.))
                        .text_color(muted),
                )
                .child(
                    div()
                        .debug_selector(move || format!("updates-incoming-{name}"))
                        .text_color(look.ink)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(incoming),
                )
        });
    div()
        .min_h(px(40.))
        .py(px(10.))
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(12.))
        .child(
            div()
                .debug_selector(move || format!("updates-name-{name}"))
                .w(px(NAME_COLUMN))
                .flex_shrink_0()
                .text_sm()
                .text_color(look.ink)
                .child(product.name),
        )
        .child(versions)
        .child(
            div()
                .debug_selector(move || format!("updates-status-{name}"))
                .ml_auto()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(
                    div()
                        .w(px(CHANNEL_COLUMN))
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(muted)
                        .children(product.channel.map(updates::channel_label)),
                )
                .child(
                    div()
                        .w(px(STATE_COLUMN))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap(px(6.))
                        .child(div().size(px(6.)).rounded_full().bg(color))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(if matches!(tone, Tone::Quiet) {
                                    muted
                                } else {
                                    color
                                })
                                .child(state.clone()),
                        ),
                ),
        )
}

/// A notice under the grid: danger-edged for a failure, quiet otherwise.
pub(super) fn notice(look: &Look, text: String, failed: bool) -> impl IntoElement {
    let edge = if failed {
        from_hex(&look.chrome.theme.danger)
    } else {
        look.border
    };
    let icon = if failed {
        IconName::CircleAlert
    } else {
        IconName::Info
    };
    div()
        .flex()
        .items_start()
        .gap(px(10.))
        .px(px(12.))
        .py(px(10.))
        .rounded(corner(look, 8.))
        .border_1()
        .border_color(edge.opacity(if failed { 0.6 } else { 1. }))
        .bg(edge.opacity(0.06))
        .child(
            div()
                .pt(px(1.))
                .child(Icon::new(icon).size(px(14.)).text_color(if failed {
                    edge
                } else {
                    from_hex(&look.chrome.muted)
                })),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .line_height(px(18.))
                .text_color(from_hex(&look.chrome.subtext))
                .child(text),
        )
}

fn render_overview(
    entity: &Entity<Updater>,
    look: &Look,
    mono: &SharedString,
    overview: &Overview,
) -> AnyElement {
    let (headline, _) = &overview.headline;
    let check = (!overview.development).then(|| {
        let entity = entity.clone();
        Button::new("updates-check")
            .label("Check now")
            .small()
            .disabled(overview.busy)
            .on_click(move |_, _, cx| entity.update(cx, Updater::check))
    });
    let install = overview.action.map(|label| {
        let entity = entity.clone();
        Button::new("updates-install")
            .label(label)
            .small()
            .primary()
            .on_click(move |_, _, cx| entity.update(cx, Updater::act))
    });
    let summary = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(14.))
        .child(glyph(look, overview))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .text_base()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(look.ink)
                        .child(headline.clone()),
                )
                .children(overview.detail.clone().map(|detail| {
                    div()
                        .text_xs()
                        .line_height(px(17.))
                        .text_color(from_hex(&look.chrome.subtext))
                        .child(detail)
                })),
        )
        .child(
            div()
                .debug_selector(|| "updates-actions".into())
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(8.))
                .children(check)
                .children(install),
        );
    let grid = div()
        .flex()
        .flex_col()
        .border_t_1()
        .border_color(look.divider)
        .children(overview.products.iter().enumerate().map(|(row, product)| {
            div()
                .when(row > 0, |line| line.border_t_1().border_color(look.divider))
                .child(product_row(look, mono, product))
        }));
    div()
        .id("updates-overview")
        .debug_selector(|| "updates-overview".into())
        .w_full()
        .flex()
        .flex_col()
        .gap(px(16.))
        .p(px(18.))
        .rounded(corner(look, 12.))
        .border_1()
        .border_color(look.border)
        .bg(look.hover)
        .child(summary)
        .child(grid)
        .children(
            overview
                .notices
                .iter()
                .map(|(text, failed)| notice(look, text.clone(), *failed)),
        )
        .into_any_element()
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
        Some(*pref(&mut Preferences::default())),
    )
    .description(description)
}

/// The Updates page in a skin, with versions in the terminal `font`.
pub(crate) fn page(
    entity: &Entity<Updater>,
    look: &Rc<Look>,
    font: &str,
    moves: bool,
    cx: &gpui::App,
) -> SettingPage {
    let updater = entity.read(cx);
    let busy = updater.busy;
    let (desktop, runtime) = (updater.prefs.desktop, updater.prefs.runtime);
    let mono = SharedString::from(font.to_owned());
    let overview = {
        let (entity, look) = (entity.clone(), look.clone());
        SettingItem::render("Update status", move |_, _, cx| {
            let overview = Overview::of(entity.read(cx), moves);
            render_overview(&entity, &look, &mono, &overview)
        })
        .keywords(["update", "version", "install", "restart", "download"])
    };
    SettingPage::new("Updates")
        .group(SettingGroup::new().item(overview))
        .group(
            SettingGroup::new()
                .title("Automatic updates")
                .item(switch_item(
                    entity,
                    "Check automatically",
                    |prefs| &mut prefs.automatic,
                    "At launch and every six hours. Nothing downloads or restarts until you click Update.",
                ))
                .item(switch_item(
                    entity,
                    if cfg!(windows) { "Check WSL runtime releases" } else { "Update phux with Desktop" },
                    |prefs| &mut prefs.manage_runtime,
                    if cfg!(windows) {
                        "Checks releases in the selected WSL distribution. Desktop never installs WSL updates or retries server handoff. Use the manual PowerShell command in update details."
                    } else {
                        "The same click installs phux on this computer and hands its live server off, so both stay on one version. Never touches remote hosts. Off leaves phux and its server alone."
                    },
                )),
        )
        .group(
            SettingGroup::new()
                .title("Release channels")
                .description("Stable is the latest release. Next follows verified main builds; switch back any time.")
                .item(channel_item(entity, "Desktop", false, busy, desktop, look))
                .item(channel_item(entity, "phux", true, busy, runtime, look)),
        )
}

/// Stable and Next in the shared segmented track.
fn channel_item(
    entity: &Entity<Updater>,
    title: &'static str,
    runtime: bool,
    busy: bool,
    selected: Channel,
    look: &Rc<Look>,
) -> SettingItem {
    let (entity, look) = (entity.clone(), look.clone());
    let product = if runtime { "runtime" } else { "desktop" };
    let pref: fn(&mut Preferences) -> &mut Channel = if runtime {
        |prefs| &mut prefs.runtime
    } else {
        |prefs| &mut prefs.desktop
    };
    SettingItem::new(
        title,
        SettingField::render(move |_, _, _| {
            let entity = entity.clone();
            segmented(
                &look,
                selected,
                [Channel::Stable, Channel::Next].map(|channel| Segment {
                    value: channel,
                    label: updates::channel_label(channel),
                    icon: None,
                    selector: format!("{product}-{}", channel.as_str()),
                }),
                move |channel| {
                    let entity = entity.clone();
                    move |_: &gpui::ClickEvent, _: &mut gpui::Window, cx: &mut gpui::App| {
                        if !busy {
                            entity.update(cx, |updater, cx| {
                                updater.preferences(|prefs| *pref(prefs) = channel, cx)
                            });
                        }
                    }
                },
            )
        }),
    )
    .description(if runtime {
        "The phux CLI and server on this computer."
    } else {
        "This app."
    })
    .keywords(["channel", "stable", "next", "release"])
}
