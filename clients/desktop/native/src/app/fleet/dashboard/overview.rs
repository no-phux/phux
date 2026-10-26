//! Real fleet totals, not decorative telemetry. Calm chrome, decisive hierarchy.

use gpui::prelude::*;
use gpui::{Context, FontWeight, div, px};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;

use super::super::{Agent, Reach};
use crate::app::color::from_hex;
use crate::app::desk::Desk;
use crate::app::skin::Look;
use crate::app::ui::{agent_swatch, dot, icon_button, muted};
use crate::connection::AgentState;

pub(super) fn header(
    agents: &[Agent],
    playground: Option<super::playground::Map>,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .p(px(24.))
        .gap(px(20.))
        .child(
            div()
                .flex()
                .items_start()
                .gap(px(12.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(7.))
                                .child(dot(5., from_hex(&look.chrome.theme.accent)))
                                .child(
                                    muted(look, "LIVE OPERATIONS")
                                        .text_size(px(10.))
                                        .font_weight(FontWeight::SEMIBOLD),
                                ),
                        )
                        .child(
                            div()
                                .mt(px(7.))
                                .text_size(px(30.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Agent fleet"),
                        )
                        .child(muted(look, "Every agent. One vantage point.").mt(px(4.))),
                )
                .when_some(playground, |row, map| {
                    row.child(div().debug_selector(|| "fleet-map-toggle".into()).child(
                        icon_button(
                            "fleet-map-toggle",
                            IconName::Layers,
                            map.next().label(),
                            None,
                            cx.listener(move |desk, _, _, cx| {
                                desk.update_prefs(
                                    |prefs| prefs.playground_map = map.next().id().to_owned(),
                                    cx,
                                );
                            }),
                        ),
                    ))
                })
                .child(
                    div()
                        .debug_selector(|| "fleet-playground-toggle".into())
                        .child(icon_button(
                            "fleet-playground-toggle",
                            if playground.is_some() {
                                IconName::LayoutDashboard
                            } else {
                                IconName::Orbit
                            },
                            if playground.is_some() {
                                "Show cards"
                            } else {
                                "Show playground"
                            },
                            None,
                            cx.listener(|desk, _, _, cx| {
                                desk.fleet_view.playground.on = !desk.fleet_view.playground.on;
                                cx.notify();
                            }),
                        )),
                )
                .child(icon_button(
                    "fleet-refresh",
                    IconName::RefreshCw,
                    "Refresh machines",
                    None,
                    cx.listener(|desk, _, _, cx| {
                        if matches!(
                            desk.machine_reach(desk.remote.as_deref()),
                            Reach::Unavailable(_)
                        ) {
                            desk.reconnect(cx);
                        }
                        desk.retry_fleet(cx);
                    }),
                ))
                .child(icon_button(
                    "fleet-close",
                    IconName::X,
                    "Close fleet",
                    None,
                    cx.listener(|desk, _, window, cx| desk.close_modal(window, cx)),
                )),
        )
        .child(totals(agents, look))
}

fn totals(agents: &[Agent], look: &Look) -> impl IntoElement {
    let working = agents.iter().filter(|a| a.info.working()).count();
    let attention = agents.iter().filter(|a| a.info.needs_you()).count();
    div()
        .flex()
        .gap(px(1.))
        .rounded(px(10.))
        .overflow_hidden()
        .border_1()
        .border_color(look.border)
        .bg(look.divider)
        .child(metric("Agents", agents.len(), None, look))
        .child(metric("Working", working, Some(AgentState::Working), look))
        .child(metric(
            "Need you",
            attention,
            Some(AgentState::Blocked),
            look,
        ))
}

fn metric(label: &str, count: usize, state: Option<AgentState>, look: &Look) -> impl IntoElement {
    let mut color = state.map_or(look.ink, |state| from_hex(agent_swatch(state)));
    if look.light {
        color = look.ink;
    }
    div()
        .flex_1()
        .min_w_0()
        .px(px(12.))
        .py(px(12.))
        .bg(look.raised)
        .child(
            div()
                .text_size(px(26.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(color)
                .child(count.to_string()),
        )
        .child(muted(look, label.to_owned()).mt(px(2.)).truncate())
}

pub(super) fn empty(look: &Look) -> impl IntoElement {
    div()
        .debug_selector(|| "fleet-empty".into())
        .py(px(36.))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(10.))
        .child(
            div()
                .size(px(56.))
                .rounded_full()
                .bg(look.active)
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(IconName::Orbit)
                        .size(px(28.))
                        .text_color(look.ink.opacity(0.65)),
                ),
        )
        .child(
            div()
                .text_size(px(20.))
                .font_weight(FontWeight::MEDIUM)
                .child("A little quiet in orbit."),
        )
        .child(muted(
            look,
            "Start an agent in any phux terminal, or choose New Agent…",
        ))
        .child(muted(
            look,
            "It will appear here automatically. No setup, no extra attachments.",
        ))
}
