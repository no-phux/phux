//! Opt-in mission control: status tiles, not terminal thumbnails or synthetic telemetry.
//! Entry motion is finite; settled fleets cost no animation loop.

use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, KeyDownEvent, ScrollHandle, Window, div, px};
use gpui_kit::assets::IconName;

use super::{Agent, Reach};
use crate::app::color::from_hex;
use crate::app::desk::Desk;
use crate::app::overlays::{SHEET_GUTTER, corner, sheet};
use crate::app::skin::Look;
use crate::app::ui::{agent_swatch, dot, icon_button, muted};
use crate::connection::AgentState;

const ENTER: Duration = Duration::from_millis(360);
const STAGGER: Duration = Duration::from_millis(18);
const TILE_HEIGHT: f32 = 158.;
const GAP: f32 = 10.;

#[derive(Default)]
pub(crate) struct State {
    opened: Option<Instant>,
    selected: Option<Agent>,
    scroll: ScrollHandle,
    pub pending: Option<Agent>,
}

impl State {
    pub fn open(&mut self) {
        self.opened = Some(Instant::now());
        self.selected = None;
        self.scroll = ScrollHandle::new();
    }

    fn align(&mut self, agents: &[Agent]) -> usize {
        let index = self
            .selected
            .as_ref()
            .and_then(|selected| agents.iter().position(|agent| agent.same_target(selected)))
            .unwrap_or(0);
        self.selected = agents.get(index).cloned();
        index
    }
}

fn columns(width: f32) -> usize {
    ((width + GAP) / (224. + GAP)).floor().clamp(1., 5.) as usize
}

fn width(window: &Window) -> f32 {
    (window.viewport_size().width.as_f32() - SHEET_GUTTER * 2.).clamp(0., 1240.)
}

/// An eased, capped cascade: even a huge fleet finishes in half a second.
fn presence(opened: Option<Instant>, index: usize, now: Instant, animate: bool) -> f32 {
    if !animate {
        return 1.;
    }
    let Some(opened) = opened else { return 1. };
    let delay = STAGGER * index.min(8) as u32;
    let elapsed = now.saturating_duration_since(opened).saturating_sub(delay);
    let t = (elapsed.as_secs_f32() / ENTER.as_secs_f32()).clamp(0., 1.);
    1. - (1. - t).powi(4)
}

impl Desk {
    pub(crate) fn fleet_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = event.keystroke.key.as_str();
        if event.keystroke.modifiers.modified() {
            return false;
        }
        let agents = self.agent_catalog();
        let current = self.fleet_view.align(&agents);
        let step = columns((width(window) - 40.).max(0.)) as isize;
        let delta = match key {
            "left" => -1,
            "right" => 1,
            "up" => -step,
            "down" => step,
            "enter" => {
                if let Some(agent) = agents.get(current) {
                    self.open_fleet_agent(agent.clone(), window, cx);
                }
                return true;
            }
            _ => return false,
        };
        let next =
            (current as isize + delta).clamp(0, agents.len().saturating_sub(1) as isize) as usize;
        self.fleet_view.selected = agents.get(next).cloned();
        self.fleet_view.scroll.scroll_to_item(next / step as usize);
        cx.notify();
        true
    }
}

pub(crate) fn panel(
    desk: &mut Desk,
    look: &Look,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let agents = desk.agent_catalog();
    let selected = desk.fleet_view.align(&agents);
    let animate = desk.prefs.animations && !cx.reduce_motion();
    let now = Instant::now();
    let opening = presence(desk.fleet_view.opened, 8, now, animate) < 1.;
    if opening {
        let handle = cx.weak_entity();
        window.on_next_frame(move |_, cx| {
            let _ = handle.update(cx, |_, cx| cx.notify());
        });
    }
    let width = width(window);
    let inner = (width - 40.).max(0.);
    let cols = columns(inner);
    let tile_width = ((inner - GAP * (cols - 1) as f32) / cols as f32).max(0.);
    let height = (window.viewport_size().height.as_f32() - 2. * SHEET_GUTTER).max(0.);
    let rows = agents
        .chunks(cols)
        .enumerate()
        .map(|(row, chunk)| {
            div()
                .flex()
                .gap(px(GAP))
                .pb(px(GAP))
                .children(chunk.iter().enumerate().map(|(col, agent)| {
                    let index = row * cols + col;
                    tile(
                        agent,
                        tile_width,
                        selected == index,
                        presence(desk.fleet_view.opened, index, now, animate),
                        look,
                        cx,
                    )
                }))
        })
        .collect::<Vec<_>>();
    sheet(look, width)
        .id("agent-fleet-panel")
        .debug_selector(|| "agent-fleet-panel".into())
        .max_h(px(height))
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(header(&agents, look, cx))
        .children(machine_notices(desk, look))
        .child(
            div()
                .id("fleet-grid")
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&desk.fleet_view.scroll)
                .px(px(20.))
                .children(rows)
                .when(agents.is_empty(), |grid| grid.child(empty(look))),
        )
        .child(
            muted(look, "↑ ↓ ← → to explore · Enter to open · Esc to return")
                .px(px(20.))
                .py(px(12.))
                .flex_shrink_0(),
        )
        .into_any_element()
}

fn header(agents: &[Agent], look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let working = agents.iter().filter(|agent| agent.info.working()).count();
    let attention = agents.iter().filter(|agent| agent.info.needs_you()).count();
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .p(px(20.))
        .flex_shrink_0()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(muted(look, "MISSION CONTROL"))
                .child(
                    div()
                        .text_size(px(25.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Agent fleet"),
                )
                .child(muted(
                    look,
                    format!(
                        "{} agents · {working} working · {attention} need you",
                        agents.len()
                    ),
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
        ))
}

fn machine_notices(desk: &Desk, look: &Look) -> Vec<gpui::Div> {
    std::iter::once(None)
        .chain(desk.hosts.iter().map(|host| Some(host.name.as_str())))
        .filter_map(|host| {
            let message = match desk.machine_reach(host) {
                Reach::Connected => return None,
                Reach::Connecting => "Connecting…".to_owned(),
                Reach::Unavailable(error) => format!("Unavailable · {error}"),
            };
            Some(
                muted(look, format!("{} · {message}", host.unwrap_or("Local")))
                    .px(px(20.))
                    .pb(px(8.)),
            )
        })
        .collect()
}

fn empty(look: &Look) -> impl IntoElement {
    div().debug_selector(|| "fleet-empty".into()).py(px(40.)).flex().flex_col().gap(px(8.))
        .child(div().text_lg().child("Your fleet starts here."))
        .child(muted(look, "Agents running in phux appear automatically. Start one in a terminal, or use New Agent… from the command palette."))
}

fn tile(
    agent: &Agent,
    width: f32,
    selected: bool,
    presence: f32,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let color = from_hex(agent_swatch(agent.info.state));
    let target = agent.clone();
    let selector = format!(
        "fleet-agent-{}-{}",
        agent.destination.host_label(),
        agent.terminal
    );
    let label = state_label(&agent.info);
    div()
        .w(px(width))
        .h(px(TILE_HEIGHT))
        .flex_shrink_0()
        .relative()
        .child(
            div()
                .id(selector.clone())
                .debug_selector(move || selector.clone())
                .relative()
                .top(px((1. - presence) * 18.))
                .opacity(presence)
                .size_full()
                .rounded(corner(look, 12.))
                .overflow_hidden()
                .cursor_pointer()
                .bg(from_hex(&look.chrome.raised))
                .border_1()
                .border_color(if selected { color } else { color.opacity(0.22) })
                .hover(|style| style.bg(color.opacity(0.12)).border_color(color))
                .on_click(cx.listener(move |desk, _, window, cx| {
                    desk.open_fleet_agent(target.clone(), window, cx)
                }))
                .child(div().h(px(3.)).w_full().bg(color.opacity(0.8)))
                .child(
                    div()
                        .p(px(14.))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(dot(7., color))
                                .child(
                                    div()
                                        .flex_1()
                                        .truncate()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(agent.info.name.clone()),
                                )
                                .child(muted(look, agent.terminal.to_string())),
                        )
                        .child(
                            div()
                                .text_size(px(21.))
                                .font_weight(FontWeight::MEDIUM)
                                .child(label),
                        )
                        .child(
                            muted(
                                look,
                                format!(
                                    "{} / {}",
                                    agent.destination.host_label(),
                                    agent.destination.name
                                ),
                            )
                            .truncate(),
                        )
                        .child(
                            muted(
                                look,
                                if agent.title.is_empty() {
                                    agent.info.kind.clone().unwrap_or_else(|| "Agent".into())
                                } else {
                                    agent.title.clone()
                                },
                            )
                            .truncate(),
                        ),
                )
                .child(signal(agent.info.state, presence, color)),
        )
        .into_any_element()
}

fn state_label(info: &crate::connection::AgentInfo) -> String {
    let word = if info.word().is_empty() {
        "unknown"
    } else {
        info.word()
    };
    if info.urgent() && info.state != AgentState::Blocked {
        format!("needs you · {word}")
    } else {
        word.to_owned()
    }
}

fn signal(state: AgentState, presence: f32, color: gpui::Hsla) -> gpui::Div {
    div()
        .absolute()
        .bottom_0()
        .left_0()
        .right_0()
        .h(px(30.))
        .opacity(0.13)
        .flex()
        .items_end()
        .gap(px(3.))
        .children((0..24).map(|i| {
            // A state emblem, not a fabricated CPU or throughput chart.
            let height = match state {
                AgentState::Working => 5. + ((i * 7 + 3) % 23) as f32,
                AgentState::Blocked => {
                    if i % 5 == 0 {
                        26.
                    } else {
                        5.
                    }
                }
                AgentState::Done => 5. + i as f32,
                AgentState::Idle | AgentState::Unknown => 4.,
            };
            div().flex_1().h(px(height * presence)).bg(color)
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_is_bounded_and_reduced_motion_is_instant() {
        let start = Instant::now();
        assert_eq!(presence(Some(start), 0, start, true), 0.);
        assert_eq!(presence(Some(start), 0, start, false), 1.);
        let end = start + ENTER + STAGGER * 8;
        assert_eq!(presence(Some(start), 1000, end, true), 1.);
        assert!(presence(Some(start), 0, start + ENTER / 2, true) > 0.9);
    }

    #[test]
    fn grid_fits_narrow_windows_and_caps_wide_ones() {
        assert_eq!(columns(0.), 1);
        assert_eq!(columns(220.), 1);
        assert_eq!(columns(460.), 2);
        assert_eq!(columns(1200.), 5);
    }
}
