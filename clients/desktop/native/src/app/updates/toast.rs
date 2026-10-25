//! One quiet corner card, backed by the same updater as Settings. It never
//! takes focus or blocks the workspace, and dismissal never cancels an update.

use super::{Tone, Updater, Work};
use crate::app::color::from_hex;
use crate::app::desk::{Desk, Modal};
use crate::app::overlays::{SHEET_GUTTER, corner};
use crate::app::skin::Look;
use crate::app::ui::ghost_icon;
use crate::app::view::chrome_moves;
use crate::updates::StageProgress;
use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, Hsla, MouseButton, Window, div, px};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::progress::ProgressCircle;
use gpui_kit::component::{Icon, Sizable};

impl Updater {
    pub(super) fn card_headline(&self) -> String {
        if !self.busy {
            return self.summary().0;
        }
        match (self.work, self.progress) {
            (Work::Checking, _) => "Checking for updates…".into(),
            (Work::Installing, _) => "Updating your local server…".into(),
            (Work::Downloading, Some(StageProgress::Verifying)) => "Verifying Desktop…".into(),
            (Work::Downloading, Some(StageProgress::Downloading(percent))) => {
                format!("Downloading Desktop · {percent}%")
            }
            (Work::Downloading, None) => "Downloading Desktop…".into(),
        }
    }

    fn card_caption(&self) -> String {
        if self.busy && self.work == Work::Installing && self.staged.is_some() {
            return "Handing off your server, then restarting Desktop. Sessions stay running."
                .into();
        }
        if self.busy {
            return "Keep working. Your sessions stay running.".into();
        }
        self.summary_detail().unwrap_or_default()
    }

    fn indicator_icon(&self) -> IconName {
        if (self.failed() || self.pending_handoff) && !self.busy {
            return IconName::TriangleAlert;
        }
        if self.work == Work::Installing && self.busy {
            return IconName::Server;
        }
        if self.staged.is_some() || self.completed {
            return IconName::Check;
        }
        IconName::ArrowDown
    }

    fn indicator_color(&self, look: &Look) -> Hsla {
        let tone = self.summary().1;
        super::view::tone_color(look, tone)
    }
}

/// A measured arc grows around the download mark. Unknown lengths and
/// verification rotate a short arc instead of inventing a percentage.
fn indicator(updater: &Updater, look: &Look, moves: bool, diameter: f32) -> AnyElement {
    let color = updater.indicator_color(look);
    let measured = match updater.progress {
        Some(StageProgress::Downloading(percent)) => Some(f32::from(percent)),
        _ => None,
    };
    let icon = Icon::new(updater.indicator_icon())
        .size(px(diameter * 0.43))
        .text_color(color);
    if updater.busy && moves {
        return ProgressCircle::new("update-transfer-ring")
            .size(px(diameter))
            .color(color)
            .value(measured.unwrap_or(24.))
            .loading(measured.is_none())
            .accessibility_label(updater.card_headline())
            .child(icon)
            .into_any_element();
    }
    div()
        .relative()
        .flex_shrink_0()
        .size(px(diameter))
        .rounded_full()
        .border_2()
        .border_color(color.opacity(0.15))
        .bg(color.opacity(0.07))
        .flex()
        .items_center()
        .justify_center()
        .child(icon)
        .into_any_element()
}

/// The titlebar stays useful while work runs, and reopens a dismissed card.
pub(crate) fn trigger(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> Option<AnyElement> {
    let updater = desk.updates.read(cx);
    if !updater.has_card() {
        return None;
    }
    let mark = indicator(updater, look, chrome_moves(desk, cx), 22.);
    let label = if updater.busy { "Updating" } else { "Update" };
    Some(
        div()
            .debug_selector(|| "desktop-update-ready".into())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Button::new("desktop-update-ready")
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(mark)
                            .child(label),
                    )
                    .accessibility_label("Desktop & local server updates")
                    .small()
                    .tooltip("Desktop & local server updates")
                    .on_click(cx.listener(|desk, _, _, cx| {
                        desk.updates.update(cx, |updater, cx| {
                            updater.card_expanded = !updater.card_expanded;
                            cx.notify();
                        });
                    })),
            )
            .into_any_element(),
    )
}

fn product(
    look: &Look,
    name: &'static str,
    versions: (String, Option<String>),
    state: (String, Tone),
) -> impl IntoElement {
    let (current, incoming) = versions;
    let versions = incoming.map_or_else(|| current.clone(), |next| format!("{current} → {next}"));
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .text_xs()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_color(look.ink).child(name))
                .child(
                    div()
                        .text_color(from_hex(&look.chrome.muted))
                        .child(versions),
                ),
        )
        .child(
            div()
                .text_color(super::view::tone_color(look, state.1))
                .child(state.0),
        )
}

fn controls(desk: &Desk, cx: &mut Context<Desk>) -> impl IntoElement {
    let updater = desk.updates.read(cx);
    let action = updater.action_label();
    let check = !updater.busy && updater.failed() && action.is_none();
    let manual = crate::updates::manual_desktop_install()
        && updater.action().is_some_and(super::UpdateAction::desktop);
    div()
        .flex()
        .items_center()
        .justify_end()
        .gap(px(8.))
        .child(
            div().debug_selector(|| "update-details".into()).child(
                Button::new("update-details")
                    .label("Details")
                    .ghost()
                    .small()
                    .on_click(cx.listener(|desk, _, window, cx| {
                        desk.show_settings_page(
                            crate::app::settings_panel::Page::Updates,
                            window,
                            cx,
                        )
                    })),
            ),
        )
        .when(check, |row| {
            let entity = desk.updates.clone();
            row.child(
                Button::new("update-retry-check")
                    .label("Try again")
                    .primary()
                    .small()
                    .on_click(move |_, _, cx| entity.update(cx, Updater::check)),
            )
        })
        .when_some(action, |row, label| {
            row.child(
                Button::new("update-card-action")
                    .label(label)
                    .primary()
                    .small()
                    .on_click(cx.listener(move |desk, _, window, cx| {
                        if manual {
                            desk.show_settings_page(
                                crate::app::settings_panel::Page::Updates,
                                window,
                                cx,
                            );
                        } else {
                            desk.updates.update(cx, Updater::act);
                        }
                    })),
            )
        })
}

pub(crate) fn card(
    desk: &Desk,
    look: &Look,
    window: &Window,
    cx: &mut Context<Desk>,
) -> Option<AnyElement> {
    let updater = desk.updates.read(cx);
    if !matches!(desk.modal, Modal::None) || !updater.card_expanded || !updater.has_card() {
        return None;
    }
    let top = crate::app::view::content_top(desk, &look.skin) + SHEET_GUTTER / 2.;
    let width = (window.viewport_size().width.as_f32() - SHEET_GUTTER * 2.).min(344.);
    let entity = desk.updates.clone();
    let content = crate::app::overlays::card(look, width)
        .id("update-card")
        .debug_selector(|| "update-card".into())
        .max_h(window.viewport_size().height - px(top + SHEET_GUTTER))
        .overflow_y_scroll()
        .p(px(16.))
        .rounded(corner(look, 14.))
        .flex()
        .flex_col()
        .gap(px(14.))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(indicator(updater, look, chrome_moves(desk, cx), 38.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(updater.card_headline()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .line_height(px(17.))
                                .text_color(from_hex(&look.chrome.subtext))
                                .child(updater.card_caption()),
                        ),
                )
                .child(div().debug_selector(|| "update-dismiss".into()).child(
                    ghost_icon("update-dismiss", IconName::X, "Dismiss update card").on_click(
                        move |_, _, cx| {
                            entity.update(cx, |updater, cx| {
                                updater.card_expanded = false;
                                cx.notify();
                            })
                        },
                    ),
                )),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .border_t_1()
                .border_color(look.divider)
                .pt(px(12.))
                .child(product(
                    look,
                    "Desktop",
                    updater.desktop_versions(),
                    updater.desktop_status(),
                ))
                .child(product(
                    look,
                    "Local CLI & server",
                    updater.runtime_versions(),
                    updater.runtime_status(),
                )),
        )
        .children(
            updater
                .notices()
                .into_iter()
                .map(|(text, failed)| super::view::notice(look, text, failed)),
        )
        .child(controls(desk, cx));
    Some(
        div()
            .absolute()
            .top(px(top))
            .right(px(SHEET_GUTTER))
            .child(content)
            .into_any_element(),
    )
}
