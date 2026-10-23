//! One tab renderer and one set of tab actions: a compact strip in the
//! titlebar or below the workspace, or rows in the navigator's fixed tab
//! shelf (or, in the plain terminal, which has no sidebar, a rail of its own). A tab whose agent needs a person wears that state itself: a
//! wash and a mark on its corner, not an entry in a separate list. A tab
//! leads with its agent's harness mark, and one only agents drive sits back
//! (shorter and fainter) until it is selected or hovered.

pub(super) mod hover;
mod map;

use gpui::prelude::*;
use gpui::{
    AnimationExt, AnyElement, ClickEvent, Context, FontWeight, IntoElement, MouseButton,
    MouseDownEvent, SharedString, Window, div, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use phux_protocol::ids::ResourceKey;

use crate::connection::AgentState;
use crate::settings::TabPosition;
use crate::workspace::layout::{self, DeskTab};

use super::super::color::from_hex;
use super::super::desk::{self, Desk, Modal};
use super::super::pane_identity::Harness;
use super::super::skin::{Look, Surface, Surfaced};
use super::super::ui::list::{CHILD_INSET, ROW, ROW_GAP, ROW_INSET, row_icon, row_radius};
use super::super::ui::menu::Anchor;
use super::super::ui::status::{attention_wash, harness_mark, state_dot, strongest};
use super::super::ui::{attention_mark, ghost_icon};
use super::tab_glow;

/// Both selected and inactive tabs reserve the surface's one-point rim.
const TAB_RIM: f32 = 1.;
/// The widest a top or bottom tab grows: tabs share the strip up to this.
const TAB_MAX: f32 = 280.;
/// Enough room for one readable horizontal tab and its actions while scrolling.
pub(crate) const MIN_STRIP_WIDTH: f32 = 160.;

/// The plain terminal's side tabs: a rail this wide beside the panes.
pub(crate) const RAIL_WIDTH: f32 = 208.;

/// Titlebar tab height for a bar `titlebar` points tall: five points clear
/// of either edge, up to the desk's tab.
pub(super) fn tab_height(titlebar: f32) -> f32 {
    (titlebar - 10.).clamp(22., 30.)
}

/// The cached bottom strip uses the same horizontal tabs as the titlebar.
pub(super) fn build_bottom(desk: &mut Desk, _: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    let look = desk.look();
    let lead = super::panes::lead_inset(desk, &look);
    let moves = super::chrome_moves(desk, cx);
    div()
        .id("bottom-tab-bar")
        .debug_selector(|| "bottom-tab-bar".into())
        .size_full()
        .pr(px(look.skin.shape.gutter))
        .flex()
        .items_center()
        .child(strip(desk, TabPosition::Bottom, &look, cx))
        // On the cards' column, moving with it as the sidebar does.
        .with_spring(
            "bottom-tab-lead",
            super::chrome_spring(moves, lead),
            |bar, inset| bar.pl(px(inset)),
        )
        .into_any_element()
}

/// The plain terminal's side tabs: the sidebar's tab list alone in a rail
/// beside the panes, so tabs stay on the side the prefs put them.
pub(super) fn build_rail(desk: &mut Desk, _: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    let look = desk.look();
    div()
        .id("tab-rail")
        .debug_selector(|| "tab-rail".into())
        .size_full()
        .flex()
        .flex_col()
        .pt(px(4.))
        .border_r(px(look.skin.shape.divider.max(1.)))
        .border_color(look.divider)
        .child(super::navigator::tab_list(desk, &look, cx))
        .into_any_element()
}

/// The tabs at `position`, measured as painted for drag reordering.
pub(super) fn strip(
    desk: &Desk,
    position: TabPosition,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let (tabs, active) = desk.link.as_ref().map_or((&[][..], ""), |link| {
        (
            link.workspace.tabs.as_slice(),
            link.workspace.active_id.as_str(),
        )
    });
    let geometry = desk.tab_geometry.clone();
    geometry.borrow_mut().prepare(position, tabs.len());
    let side = position == TabPosition::Side;
    // Beside the sidebar, top tabs start on the cards' column (the titlebar
    // rail spans the sidebar); the bottom bar insets itself.
    let lead_pad = if position == TabPosition::Top && super::navigator::wanted(desk) {
        super::panes::lead_inset(desk, look).max(4.)
    } else {
        4.
    };
    let id = match position {
        TabPosition::Top => "top-tabs",
        TabPosition::Bottom => "bottom-tabs",
        TabPosition::Side => "side-tabs",
    };
    div()
        .id(id)
        .debug_selector(move || id.into())
        .when(side, |list| list.w_full())
        .when(!side, |list| {
            list.flex_1().min_w_0().h_full().overflow_x_scroll()
        })
        .child(
            // Measure inside the scroller: its children then include the actual
            // painted scroll offset, not the unscrolled layout coordinates.
            div()
                .flex()
                .when(side, |list| list.flex_col().gap(px(1.)).w_full())
                .when(!side, |list| {
                    list.gap(px(5.))
                        .w_full()
                        .min_w(px(tabs.len() as f32 * 105. + 3. + lead_pad - 4.))
                        .h_full()
                        .items_center()
                        .when(position == TabPosition::Top, |list| {
                            list.pl(px(lead_pad)).pr(px(4.))
                        })
                })
                .children(tabs.iter().map(|tab| {
                    tab_button(desk, tab, active, position, look, cx).into_any_element()
                }))
                .on_children_prepainted(move |bounds, _, _| {
                    let mut geometry = geometry.borrow_mut();
                    for (index, bounds) in bounds.into_iter().enumerate() {
                        geometry.record(index, position, bounds);
                    }
                }),
        )
}

/// The most pressing agent state in `tab`, only when it needs a person.
pub(super) fn tab_attention(desk: &Desk, tab: &DeskTab) -> Option<AgentState> {
    strongest(
        layout::placements(&tab.root)
            .into_iter()
            .filter_map(|placement| placement.terminal_id.as_ref().and_then(|id| desk.agent(id)))
            .filter(|info| info.needs_you())
            .map(|info| info.state),
    )
}

/// The calmer state an agent in `tab` is in, for a quiet leading dot.
fn tab_agent(desk: &Desk, tab: &DeskTab) -> Option<AgentState> {
    strongest(
        layout::placements(&tab.root)
            .into_iter()
            .filter_map(|placement| placement.terminal_id.as_ref().and_then(|id| desk.agent(id)))
            .map(|info| info.state)
            .filter(|state| *state != AgentState::Unknown),
    )
}

/// A sidebar tab's leading slot: a lit dot while it needs a person, else its
/// agent's harness mark, else its agent's state, else a terminal glyph.
fn side_lead(
    attention: Option<AgentState>,
    agent: Option<AgentState>,
    harness: Option<Harness>,
    selected: bool,
    look: &Look,
) -> AnyElement {
    if attention.is_some() {
        return state_dot(attention, agent).into_any_element();
    }
    if let Some(harness) = harness {
        return harness_mark(harness, agent, 13., look).into_any_element();
    }
    if agent.is_some() {
        return state_dot(None, agent).into_any_element();
    }
    Icon::new(IconName::SquareTerminal)
        .size(px(13.))
        .text_color(from_hex(if selected {
            &look.chrome.subtext
        } else {
            &look.chrome.faint
        }))
        .into_any_element()
}

/// A titlebar tab's leading slot: nothing while its corner mark asks for a
/// person, else what a sidebar tab leads with.
fn top_lead(
    attention: Option<AgentState>,
    agent: Option<AgentState>,
    harness: Option<Harness>,
    selected: bool,
    look: &Look,
) -> Option<AnyElement> {
    attention
        .is_none()
        .then(|| side_lead(None, agent, harness, selected, look))
}

/// A titlebar tab's close button: on hover, and always on the selected tab.
fn tab_close(id: &str, selected: bool, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let hover = look.hover;
    let selector = SharedString::from(format!("tab-close-{id}"));
    let close = id.to_owned();
    div()
        .id(selector.clone())
        .debug_selector(move || selector.to_string())
        .flex_shrink_0()
        .size(px(18.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .text_color(from_hex(&look.chrome.muted))
        .invisible()
        .group_hover("tab", |style| style.visible())
        .when(selected, |close| close.visible())
        .hover(move |style| style.bg(hover))
        .child("✕")
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |desk, _, window, cx| {
            desk.close_tab(&close);
            desk.after_command(window, cx);
            cx.stop_propagation();
        }))
}

/// A sidebar tab's actions, as a row's are: an ellipsis on hover, always on
/// the selected tab and while its menu shows.
fn tab_more(desk: &Desk, id: &str, selected: bool, cx: &mut Context<Desk>) -> impl IntoElement {
    let key = format!("tab-more-{id}");
    let open = desk.menu_open(&format!("tab-{id}"));
    let (tab, anchor) = (id.to_owned(), key.clone());
    div()
        .relative()
        .flex_shrink_0()
        .debug_selector({
            let key = key.clone();
            move || key.clone()
        })
        .invisible()
        .group_hover("tab", |style| style.visible())
        .when(selected || open, |more| more.visible())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(desk.anchors.probe(key.clone()))
        .child(
            ghost_icon(SharedString::from(key), IconName::Ellipsis, "Tab actions")
                .toggled(open)
                .on_click(cx.listener(move |desk, event: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    let anchor = desk
                        .anchors
                        .get(&anchor)
                        .map_or(Anchor::At(event.position()), Anchor::Below);
                    desk.open_tab_menu(&tab, anchor, window, cx);
                })),
        )
}

/// The tab names its focused terminal, just as its title does. Modifier
/// selection uses that terminal's organization key without activating or
/// arming a tab reorder; keyless tabs retain their ordinary behavior.
fn tab_press(
    desk: &mut Desk,
    id: &str,
    key: Option<ResourceKey>,
    event: &MouseDownEvent,
    window: &mut Window,
    cx: &mut Context<Desk>,
) {
    desk.tab_peek.click = None;
    if event.modifiers.platform
        && let Some(key) = key
    {
        desk.toggle_resource_selection(key, cx);
    } else {
        let peek = desk.active_tab_id().as_deref() == Some(id) && !event.modifiers.modified();
        desk.select_tab(id);
        desk.begin_tab_drag(
            id.to_owned(),
            event.position.x.as_f32(),
            event.position.y.as_f32(),
            cx,
        );
        desk.tab_peek.click = peek.then(|| id.to_owned());
    }
    desk.after_command(window, cx);
}

fn tab_resource_key(desk: &Desk, tab: &DeskTab) -> Option<ResourceKey> {
    let link = desk.link.as_ref()?;
    let terminal = layout::focused_placement(tab)?.terminal_id.as_ref()?;
    link.connection.pane(terminal)?.key
}

/// Rename follows an ordinary double click, not an organization selection
/// whose modifier happened to be released between press and release.
fn rename_requested(event: &ClickEvent) -> bool {
    if event.click_count() < 2 || event.modifiers().platform {
        return false;
    }
    match event {
        ClickEvent::Mouse(click) => !click.down.modifiers.platform,
        _ => true,
    }
}

fn tab_click(
    desk: &mut Desk,
    id: &str,
    event: &ClickEvent,
    window: &mut Window,
    cx: &mut Context<Desk>,
) {
    let peek = desk.tab_peek.click.take().as_deref() == Some(id);
    if rename_requested(event) {
        desk.tab_peek.close();
        desk.open_modal(
            Modal::Rename {
                tab_id: id.to_owned(),
            },
            window,
            cx,
        );
    } else if peek && event.click_count() == 1 {
        desk.toggle_tab_peek(window, cx);
    }
}

fn tab_label(desk: &Desk, tab: &DeskTab, selected: bool, look: &Look) -> gpui::Div {
    let id = tab.id.clone();
    div()
        .debug_selector(move || format!("tab-label-{id}"))
        .flex_1()
        .min_w_0()
        .truncate()
        .text_sm()
        .when(selected, |label| label.font_weight(FontWeight::MEDIUM))
        .text_color(if selected {
            look.ink
        } else {
            from_hex(&look.chrome.subtext)
        })
        .child(desk::tab_title(desk, tab))
}

fn tab_actions(
    desk: &Desk,
    tab: &DeskTab,
    side: bool,
    selected: bool,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    if side {
        tab_more(desk, &tab.id, selected, cx).into_any_element()
    } else {
        tab_close(&tab.id, selected, look, cx).into_any_element()
    }
}

fn tab_button(
    desk: &Desk,
    tab: &DeskTab,
    active: &str,
    position: TabPosition,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let height = tab_height(look.skin.chrome.titlebar);
    let tab_selector = SharedString::from(format!("tab-{}", tab.id));
    let selected = tab.id == active;
    let key = tab_resource_key(desk, tab);
    let organizing = key.is_some_and(|key| desk.organization.selected.contains(&key));
    let hover = look.hover;
    let attention = tab_attention(desk, tab);
    let agent = tab_agent(desk, tab);
    let harness = desk.tab_harness(tab);
    // Only agents drive it and nothing asks for a person: it sits back until
    // selected or hovered, as its panes do.
    let depth = look.skin.material.depth;
    let recessed = !selected && attention.is_none() && desk.tab_recessed(tab);
    let fade = if recessed {
        (1. - 2. * depth.veil).clamp(0., 1.)
    } else {
        1.
    };
    let sunk = if recessed { depth.inset } else { 0. };
    let (drag_id, rename_id, middle_id, menu_id) = (
        tab.id.clone(),
        tab.id.clone(),
        tab.id.clone(),
        tab.id.clone(),
    );
    let side = position == TabPosition::Side;
    let map = map::render(desk, tab, selected, look);
    let tooltip_tab = tab.id.clone();
    let handle = cx.weak_entity();
    div()
        .id(tab_selector.clone())
        .debug_selector(move || tab_selector.to_string())
        .group("tab")
        .relative()
        .flex_shrink_0()
        .pr(px(4.))
        .when(side, |tab| {
            // A row under its session, indented past the session's caret.
            tab.min_h(px(ROW))
                .w_full()
                .pl(px(CHILD_INSET - TAB_RIM))
                .pr(px(ROW_INSET - TAB_RIM))
        })
        .when(!side, |tab| {
            tab.h(px(height - sunk))
                .min_w(px(100.))
                .max_w(px(TAB_MAX))
                .flex_1()
                .pl(px(10.))
        })
        .flex()
        .items_center()
        .gap(px(if side { ROW_GAP } else { 6. }))
        .when(selected, |tab| {
            tab.surface(look, if side { Surface::SideTab } else { Surface::Tab })
                .shadow(tab_glow(look))
        })
        .when(!selected, |tab| {
            tab.rounded(px(if side {
                row_radius(&look.skin)
            } else {
                look.skin.shape.tab_radius
            }))
            .border_1()
            .border_color(gpui::transparent_black())
            .when(fade < 1., |tab| tab.opacity(fade))
            .hover(move |style| style.bg(hover).opacity(1.))
        })
        .when_some(attention.filter(|_| !selected), |tab, state| {
            tab.bg(attention_wash(state))
        })
        .when(organizing, |tab| tab.bg(look.active))
        .block_mouse_except_scroll()
        .when(side, |tab| {
            tab.child(row_icon(side_lead(
                attention, agent, harness, selected, look,
            )))
        })
        .when(!side, |tab| {
            // A fixed slot, so the label holds still as an agent comes and goes;
            // like a row's, set a point low to meet the label's lowercase.
            tab.child(
                div()
                    .size(px(13.))
                    .relative()
                    .top(px(1.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .children(top_lead(attention, agent, harness, selected, look)),
            )
        })
        .child(tab_label(desk, tab, selected, look))
        .children(map)
        .child(tab_actions(desk, tab, side, selected, look, cx))
        .children(
            attention
                .filter(|_| !side)
                .map(|state| attention_mark(state, look)),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |desk, event: &gpui::MouseDownEvent, window, cx| {
                tab_press(desk, &drag_id, key, event, window, cx);
            }),
        )
        .on_mouse_down(
            MouseButton::Middle,
            cx.listener(move |desk, _, window, cx| {
                desk.close_tab(&middle_id);
                desk.after_command(window, cx);
            }),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |desk, event: &gpui::MouseDownEvent, window, cx| {
                desk.open_tab_menu(&menu_id, Anchor::At(event.position), window, cx);
            }),
        )
        .on_click(cx.listener(move |desk, event: &ClickEvent, window, cx| {
            tab_click(desk, &rename_id, event, window, cx);
        }))
        .tooltip(move |window, cx| hover::tooltip(&tooltip_tab, &handle, window, cx))
}
