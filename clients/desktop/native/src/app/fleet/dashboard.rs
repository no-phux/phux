//! Live mission control. Inventory only until a person opens an agent.
//! Entry motion is bounded; a settled fleet schedules no animation frames.

mod cards;
mod overview;
mod playground;
#[cfg(feature = "ui-fixtures")]
pub(crate) mod preview;

use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{AnyElement, Context, KeyDownEvent, ScrollHandle, Window, div, px};

use super::{Agent, Reach};
use crate::app::desk::Desk;
use crate::app::overlays::{SHEET_GUTTER, sheet};
use crate::app::skin::Look;
use crate::app::ui::muted;

const ENTER: Duration = Duration::from_millis(360);
const STAGGER: Duration = Duration::from_millis(18);
const GAP: f32 = 12.;
const INSET: f32 = 24.;
/// The playground's frame interval.
const TICK: Duration = Duration::from_millis(80);

#[derive(Default)]
pub(crate) struct State {
    opened: Option<Instant>,
    selected: Option<Agent>,
    scroll: ScrollHandle,
    pub pending: Option<Agent>,
    pub(super) pending_open: Option<crate::workspace::PendingOpen>,
    /// The playground: the same agents as ghosts in their hosts' rooms.
    pub(super) playground: playground::View,
    /// A playground frame is already on its way.
    tick_scheduled: bool,
    #[cfg(feature = "ui-fixtures")]
    fixture: Option<Vec<Agent>>,
}

impl State {
    pub fn open(&mut self) {
        self.pending = None;
        self.pending_open = None;
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
    ((width + GAP) / (280. + GAP)).floor().clamp(1., 3.) as usize
}

fn width(window: &Window) -> f32 {
    (window.viewport_size().width.as_f32() - SHEET_GUTTER * 2.).clamp(0., 1200.)
}

/// Even a large fleet finishes entering in half a second. Reduced motion snaps.
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
        if event.keystroke.modifiers.modified() {
            return false;
        }
        let agents = self.agent_catalog();
        let current = self.fleet_view.align(&agents);
        let step = columns((width(window) - INSET * 2.).max(0.)) as isize;
        let delta = match event.keystroke.key.as_str() {
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
    #[cfg(feature = "ui-fixtures")]
    let agents = desk.fleet_view.fixture.clone().unwrap_or(agents);
    let selected = desk.fleet_view.align(&agents);
    let animate = desk.prefs.animations && !cx.reduce_motion();
    let now = Instant::now();
    if presence(desk.fleet_view.opened, 8, now, animate) < 1. {
        let handle = cx.weak_entity();
        window.on_next_frame(move |_, cx| {
            let _ = handle.update(cx, |_, cx| cx.notify());
        });
    }
    let width = width(window);
    let inner = (width - INSET * 2.).max(0.);
    let playground = desk.fleet_view.playground.on;
    // The playground's own clock: a dozen frames a second while it is open
    // and motion is allowed, nothing otherwise. ponytail: this re-renders the
    // desk's chrome each tick; move the scene into its own entity if that
    // ever shows in the HUD.
    if playground && animate && !desk.fleet_view.tick_scheduled {
        desk.fleet_view.tick_scheduled = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TICK).await;
            let _ = this.update(cx, |desk, cx| {
                desk.fleet_view.tick_scheduled = false;
                if desk.fleet_view.playground.on
                    && matches!(desk.modal, crate::app::desk::Modal::AgentFleet)
                {
                    cx.notify();
                }
            });
        })
        .detach();
    }
    let time = if animate {
        desk.fleet_view
            .opened
            .map_or(0., |opened| now.duration_since(opened).as_secs_f32())
    } else {
        0.
    };
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
                    cards::tile(
                        agent,
                        index,
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
        .child(overview::header(
            &agents,
            playground.then(|| playground::Map::from_id(&desk.prefs.playground_map)),
            look,
            cx,
        ))
        .when(playground, |panel| {
            let scene_height = (height - 300.).max(240.);
            panel.child(
                div()
                    .px(px(INSET))
                    .pt(px(4.))
                    .flex_shrink_0()
                    .child(playground::scene(
                        desk,
                        &agents,
                        inner,
                        scene_height,
                        time,
                        look,
                        cx,
                    )),
            )
        })
        .when(!playground, |panel| {
            panel.child(
                div()
                    .id("fleet-grid")
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&desk.fleet_view.scroll)
                    .px(px(INSET))
                    .pt(px(4.))
                    .children(rows)
                    .when(agents.is_empty(), |grid| grid.child(overview::empty(look)))
                    .children(machine_notices(desk, look)),
            )
        })
        .child(
            div()
                .flex()
                .flex_wrap()
                .justify_between()
                .gap(px(6.))
                .px(px(INSET))
                .py(px(12.))
                .flex_shrink_0()
                .border_t_1()
                .border_color(look.divider)
                .child(muted(look, "↑ ↓ ← → explore   ↵ open   esc return"))
                .child(muted(look, "Observe freely. Open deliberately.")),
        )
        .into_any_element()
}

fn machine_notices(desk: &Desk, look: &Look) -> Vec<gpui::Div> {
    #[cfg(feature = "ui-fixtures")]
    if desk.fleet_view.fixture.is_some() {
        return Vec::new();
    }
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
                    .py(px(8.))
                    .w_full(),
            )
        })
        .collect()
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
        assert_eq!(columns(240.), 1);
        assert_eq!(columns(580.), 2);
        assert_eq!(columns(1200.), 3);
    }
}
