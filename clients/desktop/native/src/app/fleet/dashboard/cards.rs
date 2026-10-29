//! An agent's identity, real task title, state and exact location. No fake metrics.

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, Hsla, div, linear_color_stop, linear_gradient, px};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;

use super::super::Agent;
use crate::app::color::from_hex;
use crate::app::desk::Desk;
use crate::app::overlays::corner;
use crate::app::pane_identity::Harness;
use crate::app::skin::Look;
use crate::app::ui::status::harness_mark;
use crate::app::ui::{agent_swatch, dot, muted, shadow};
use crate::connection::{AgentInfo, AgentState};

pub(super) fn tile_id(host: Option<&str>, terminal: &phux_protocol::ResourceId) -> String {
    // Local is a display label, not a namespace: a remote can be named Local.
    match host {
        Some(host) => format!("fleet-agent-remote-{host}-{terminal}"),
        None => format!("fleet-agent-local-{terminal}"),
    }
}

pub(super) fn tile(
    agent: &Agent,
    index: usize,
    width: f32,
    selected: bool,
    presence: f32,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let color = state_color(&agent.info, look);
    let target = agent.clone();
    let selector = tile_id(agent.destination.host.as_deref(), &agent.terminal);
    let harness = Harness::from_slug(agent.info.kind.as_deref().unwrap_or_default());
    div()
        .w(px(width))
        .h(px(244.))
        .flex_shrink_0()
        .relative()
        .child(
            div()
                .id(selector.clone())
                .debug_selector(move || selector.clone())
                .relative()
                .top(px((1. - presence) * 12.))
                .opacity(presence)
                .size_full()
                .flex()
                .flex_col()
                .rounded(corner(look, 14.))
                .overflow_hidden()
                .cursor_pointer()
                .bg(linear_gradient(
                    135.,
                    linear_color_stop(color.opacity(if look.light { 0.09 } else { 0.12 }), 0.),
                    linear_color_stop(look.raised, 0.8),
                ))
                .border_1()
                .border_color(if selected {
                    color.opacity(0.85)
                } else {
                    look.border
                })
                .when(selected, |card| {
                    card.shadow(vec![shadow(color.opacity(0.10), 4., 18., 0., false)])
                })
                .hover(|style| style.bg(look.hover).border_color(color.opacity(0.8)))
                .on_click(cx.listener(move |desk, _, window, cx| {
                    desk.open_fleet_agent(target.clone(), window, cx)
                }))
                .child(
                    div()
                        .px(px(18.))
                        .pt(px(16.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(muted(look, format!("{:02}", index + 1)).font_family("monospace"))
                        .child(status(&agent.info, color, look)),
                )
                .child(
                    div()
                        .px(px(18.))
                        .pt(px(18.))
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(emblem(harness, color, look))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_size(px(18.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .truncate()
                                        .child(agent.info.name.clone()),
                                )
                                .child(muted(look, harness.label()).mt(px(3.))),
                        ),
                )
                .child(
                    div().px(px(18.)).pt(px(14.)).flex_1().min_w_0().child(
                        div()
                            .text_size(px(13.))
                            .text_color(look.ink.opacity(0.78))
                            .truncate()
                            .child(task_title(agent)),
                    ),
                )
                .child(location(agent, color, selected, look)),
        )
        .into_any_element()
}

fn task_title(agent: &Agent) -> String {
    if agent.title.is_empty() {
        "No terminal title".into()
    } else {
        agent.title.clone()
    }
}

fn emblem(harness: Harness, color: Hsla, look: &Look) -> impl IntoElement {
    div()
        .size(px(48.))
        .flex_shrink_0()
        .relative()
        .rounded(corner(look, 14.))
        .border_1()
        .border_color(color.opacity(0.22))
        .bg(color.opacity(0.07))
        .flex()
        .items_center()
        .justify_center()
        .child(harness_mark(harness, None, 26., look))
        .child(dot(5., color).absolute().right(px(4.)).bottom(px(4.)))
}

fn status(info: &AgentInfo, color: Hsla, look: &Look) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(8.))
        .py(px(4.))
        .rounded_full()
        .bg(color.opacity(0.10))
        .text_color(status_ink(color, look))
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .child(dot(5., color))
        .child(state_label(info))
}

fn location(agent: &Agent, color: Hsla, selected: bool, look: &Look) -> impl IntoElement {
    div()
        .px(px(18.))
        .py(px(12.))
        .flex()
        .items_center()
        .gap(px(10.))
        .border_t_1()
        .border_color(look.divider)
        .bg(look.ink.opacity(0.02))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(12.))
                        .truncate()
                        .child(agent.destination.name.clone()),
                )
                .child(
                    muted(
                        look,
                        format!("{}  ·  {}", agent.destination.host_label(), agent.terminal),
                    )
                    .truncate()
                    .mt(px(3.)),
                ),
        )
        .child(
            div()
                .size(px(25.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .when(selected, |mark| mark.bg(color.opacity(0.12)))
                .child(
                    Icon::new(IconName::ArrowUpRight)
                        .size(px(14.))
                        .text_color(if selected {
                            color
                        } else {
                            look.ink.opacity(0.45)
                        }),
                ),
        )
}

/// Fixed lifecycle hues are darkened on light themes for readable small labels.
pub(super) fn state_color(info: &AgentInfo, look: &Look) -> Hsla {
    let state = if info.urgent() {
        AgentState::Blocked
    } else {
        info.state
    };
    let mut color = from_hex(agent_swatch(state));
    if look.light {
        color.l = color.l.min(0.32);
    }
    color
}

fn status_ink(color: Hsla, look: &Look) -> Hsla {
    if look.light { look.ink } else { color }
}

pub(super) fn state_label(info: &AgentInfo) -> String {
    if info.urgent() {
        return "Needs you".into();
    }
    match info.state {
        AgentState::Working => "Working",
        AgentState::Blocked => "Needs you",
        AgentState::Done => "Review ready",
        AgentState::Idle => "Ready",
        AgentState::Unknown => "Unknown",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(color: Hsla) -> f32 {
        let rgb = color.to_rgb();
        let channel = |value: f32| {
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(rgb.r) + 0.7152 * channel(rgb.g) + 0.0722 * channel(rgb.b)
    }

    #[test]
    fn light_theme_status_text_has_accessible_contrast() {
        let prefs = crate::settings::default_display();
        for theme in crate::settings::theme::themes()
            .iter()
            .filter(|theme| theme.appearance == "light")
        {
            let look = Look::of(
                crate::settings::skin::skin_by_id("glass"),
                &crate::settings::theme::palette(theme),
                &prefs,
            );
            for state in [
                AgentState::Working,
                AgentState::Blocked,
                AgentState::Done,
                AgentState::Idle,
                AgentState::Unknown,
            ] {
                let info = AgentInfo {
                    name: "Test".into(),
                    kind: None,
                    state,
                    attention: crate::connection::AgentAttention::None,
                    changed_at_ms: 0,
                };
                let accent = state_color(&info, &look);
                let ink = luminance(status_ink(accent, &look));
                // The pill adds its ten-percent state wash to the card backing.
                let background = look.raised.to_rgb();
                let wash = accent.to_rgb();
                let fill = gpui::Rgba {
                    r: background.r * 0.9 + wash.r * 0.1,
                    g: background.g * 0.9 + wash.g * 0.1,
                    b: background.b * 0.9 + wash.b * 0.1,
                    a: 1.,
                };
                let fill = luminance(fill.into());
                let ratio = (ink.max(fill) + 0.05) / (ink.min(fill) + 0.05);
                assert!(ratio >= 4.5, "{} {state:?}: {ratio}", theme.id);
            }
        }
    }

    #[test]
    fn remote_named_local_has_its_own_identity() {
        let terminal = phux_protocol::ResourceId::local(9);
        assert_ne!(tile_id(None, &terminal), tile_id(Some("Local"), &terminal));
        assert_ne!(tile_id(None, &terminal), tile_id(Some("local"), &terminal));
    }
}
