//! Read-only tab context, built from current inventory when the tooltip opens.
//! GPUI owns its delay, dismissal and viewport fitting, outside tab layout.

use gpui::prelude::*;
use gpui::{AnyView, App, FontWeight, WeakEntity, Window, div, px};
use gpui_kit::component::tooltip::Tooltip;

use super::super::super::desk::{self, Desk};
use super::super::super::pane_identity::Harness;
use super::super::super::skin::{Look, Surface, Surfaced};
use super::super::super::ui::agent_swatch;
use super::{from_hex, side_lead};
use crate::connection::AgentState;
use crate::workspace::layout::{self, DeskTab, Placement};

const MAX_ROWS: usize = 8;
const ROW_HEIGHT: f32 = 42.;
const CARD_WIDTH: f32 = 320.;

#[derive(Debug, PartialEq)]
pub(in crate::app::view) struct PaneContext {
    pub title: String,
    pub directory: Option<String>,
    pub harness: Option<Harness>,
    pub state: Option<AgentState>,
    pub status: &'static str,
    pub attention: Option<AgentState>,
}

pub(in crate::app::view) fn pane_context(desk: &Desk, placement: &Placement) -> PaneContext {
    let terminal = placement.terminal_id.as_ref();
    let agent = terminal.and_then(|id| desk.agent(id));
    let directory = terminal
        .and_then(|id| desk.link.as_ref()?.connection.pane(id))
        .and_then(|pane| pane.cwd.as_deref())
        .map(desk::display_title)
        .filter(|path| !path.is_empty());
    PaneContext {
        title: desk.pane_title(terminal),
        directory,
        harness: desk.placement_origin(placement).harness(),
        state: agent.map(|agent| agent.state),
        status: agent.map_or("", |agent| agent.word()),
        attention: agent
            .filter(|agent| agent.needs_you())
            .map(|agent| agent.state),
    }
}

pub(in crate::app::view) fn row_limit(height: f32) -> usize {
    // Leave room for the title, overflow count, padding and tooltip margins.
    (((height - 140.).max(ROW_HEIGHT) / ROW_HEIGHT) as usize).clamp(1, MAX_ROWS)
}

pub(super) fn tooltip(
    tab: &str,
    handle: &WeakEntity<Desk>,
    window: &mut Window,
    cx: &mut App,
) -> AnyView {
    let tab = tab.to_owned();
    let handle = handle.clone();
    let look = handle.upgrade().map(|desk| desk.read(cx).look());
    Tooltip::element(move |window, cx| {
        let Some(handle) = handle.upgrade() else {
            return div();
        };
        let desk = handle.read(cx);
        let Some(tab) = desk
            .link
            .as_ref()
            .and_then(|link| link.workspace.tabs.iter().find(|item| item.id == tab))
        else {
            return div();
        };
        card(desk, tab, window)
    })
    .when_some(look, |tip, look| {
        tip.surface(&look, Surface::Popover)
            .p(px(10.))
            .text_color(look.ink)
    })
    .build(window, cx)
}

fn card(desk: &Desk, tab: &DeskTab, window: &Window) -> gpui::Div {
    let look = desk.look();
    let placements = layout::placements(&tab.root);
    let limit = row_limit(window.viewport_size().height.as_f32());
    let remaining = placements.len().saturating_sub(limit);
    let width = (window.viewport_size().width.as_f32() - 64.).clamp(1., CARD_WIDTH);
    div()
        .debug_selector(|| "tab-context".into())
        .w(px(width))
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .w_full()
                .whitespace_normal()
                .line_clamp(2)
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(desk::tab_title(desk, tab)),
        )
        .children(
            placements
                .into_iter()
                .take(limit)
                .map(|placement| pane_row(pane_context(desk, placement), &placement.id, &look)),
        )
        .when(remaining > 0, |card| {
            card.child(
                div()
                    .debug_selector(|| "tab-context-more".into())
                    .pl(px(21.))
                    .text_xs()
                    .text_color(from_hex(&look.chrome.muted))
                    .child(format!("+{remaining} more")),
            )
        })
}

fn pane_row(pane: PaneContext, id: &str, look: &Look) -> gpui::Div {
    let id = id.to_owned();
    div()
        .debug_selector(move || format!("tab-context-pane-{id}"))
        .h(px(ROW_HEIGHT - 6.))
        .flex_shrink_0()
        .flex()
        .gap(px(8.))
        .child(div().pt(px(3.)).child(pane.harness.map_or_else(
            || side_lead(pane.attention, pane.state, None, false, look),
            |harness| {
                super::harness_mark(harness, pane.state, 13., look)
                    .debug_selector(|| "tab-context-harness".into())
                    .into_any_element()
            },
        )))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .child(pane.title),
                        )
                        .when(!pane.status.is_empty(), |row| {
                            row.child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(
                                        pane.state.map_or(look.ink, |state| {
                                            from_hex(agent_swatch(state))
                                        }),
                                    )
                                    .child(pane.status),
                            )
                        }),
                )
                .when_some(pane.directory, |row, directory| {
                    row.child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(from_hex(&look.chrome.muted))
                            .child(directory),
                    )
                }),
        )
}
